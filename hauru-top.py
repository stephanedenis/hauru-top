#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""hauru-top — tableau de bord terminal de la station Hauru.

But
---
Afficher en un seul écran, rafraîchi chaque seconde et en couleur :

* la charge des **deux processeurs** (sockets) du poste, cœur physique par cœur
  physique, avec les **deux threads hyperthread empilés dans une même barre**
  (moitié haute / moitié basse), pour un groupement visuel de chaque cœur ;
* la charge, la **VRAM**, la puissance, la température et les fréquences des
  **deux GPU** NVIDIA ;
* les métriques globales : charge CPU, RAM, swap, VRAM cumulée, load average,
  uptime, fréquences et températures par socket ;
* un panneau LIAISONS : barres PCIe TX/RX par lien (génération, largeur, débit
  NVML), NVLink (état, vitesse, erreurs), QPI (topologie), tensor (GPM si
  disponible) et carte AMD (température, VRAM, horloges, ventilateur, DPM) ;
* un petit historique sparkline quand la fenêtre est assez haute.

Effets de bord
--------------
Aucun : le script ne fait que lire `/proc`, `/sys`, interroger `nvidia-smi` et
NVML (chargé via `ctypes`). Il n'écrit ni ne modifie rien sur le système.

Garde-fous
----------
* Aucun privilège root n'est nécessaire (les capteurs coretemp et nvidia-smi
  sont lisibles par l'utilisateur).
* `nvidia-smi` est appelé avec un délai maximal ; en cas d'échec le tableau de
  bord continue de fonctionner et signale l'erreur dans le pied de page.
* Le terminal est restauré (écran alterné, curseur, mode clavier) à la sortie,
  y compris sur Ctrl+C ou SIGTERM.

Dépendances
-----------
Python 3.10+, le paquet `rich` et `ctypes` (stdlib)
pour NVML. Aucun autre paquet.

Un portage Rust autonome vit dans `hauru-top-rs/` : même
rendu, mêmes options, binaire sans Python ni `rich` (voir son README).

Mode d'emploi
-------------
    ./hauru-top.py
    ./hauru-top.py --interval 0.5
    ./hauru-top.py --once --width 140 --height 40 > vue.txt

Touches : `q` quitter, `p` pause, `r` remettre l'historique à zéro.

Notes Hauru
-----------
* 2 × Xeon E5-2687W v3 : 20 cœurs physiques, 40 threads. Les frères SMT sont
  lus dans `topology/thread_siblings_list` (0/20, 1/21, …, 10/30, …) et non
  codés en dur.
* 2 × Quadro RTX 5000 (16 Gio chacune), reliées par un lien NVLink de
  25,781 Go/s. Le lien PCIe descend en Gen1 au repos et remonte en Gen3.
* La carte AMD (Radeon HD 7750) est suivie pour la température, la VRAM, les
  horloges et le ventilateur ; son activité n'est pas exposée par
  `gpu_busy_percent` sur ce noyau.
* L'historique CPU passe en rouge dès qu'un thread atteint 100 % (seuil 99,5 %
  pour tenir compte de l'arrondi d'affichage).
"""

from __future__ import annotations

import argparse
import csv
import ctypes
import math
import os
import re
import select
import shutil
import signal
import subprocess
import sys
import time
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Iterable, Optional, Sequence

try:
    from rich.cells import cell_len
    from rich.console import Console
    from rich.live import Live
    from rich.text import Text
except ModuleNotFoundError:  # pragma: no cover - message destiné à l'utilisateur
    sys.exit(
        "hauru-top : le paquet Python « rich » est requis.\n"
        "Sur openSUSE : sudo zypper install python3-rich\n"
        "Ailleurs : python3 -m pip install --user rich"
    )


APP = "hauru-top"
VERSION = "1.2.1"

# Un thread est considéré à 100 % dès que l'affichage arrondi donne 100 %.
SATURATED_THRESHOLD = 99.5

# ---------------------------------------------------------------------------
# Palette (Tokyo Night : lisible sur fond sombre, quantifiée par rich sinon)
# ---------------------------------------------------------------------------

BORDER = "#3b4261"
DIM = "#737aa2"
DIM_SOFT = "#565f89"
TEXT = "#c0caf5"
TITLE = "#7aa2f7"
CPU_C = "#7dcfff"
GPU_C = "#bb9af7"
GREEN = "#9ece6a"
YELLOW = "#e0af68"
ORANGE = "#ff9e64"
RED = "#f7768e"
TRACK = "#363c55"

# Seuils de couleur de charge : froid (bleu) -> chaud (rouge)
LOAD_STOPS: tuple[tuple[float, tuple[int, int, int]], ...] = (
    (0.00, (86, 182, 255)),
    (0.35, (125, 207, 255)),
    (0.55, (158, 206, 106)),
    (0.72, (224, 175, 104)),
    (0.86, (255, 158, 100)),
    (1.00, (247, 118, 142)),
)

SPARK_UNICODE = "▁▂▃▄▅▆▇█"
SPARK_ASCII = " .:-=+*#"
PARTIAL_UNICODE = "▏▎▍▌▋▊▉"
PARTIAL_ASCII = "#"

# Remplis au démarrage par main() selon la locale / --ascii
UNICODE = True
GLYPH = {
    "tl": "╭", "tr": "╮", "bl": "╰", "br": "╯",
    "h": "─", "v": "│", "full": "█", "track": "░", "dot": "●",
    "pair_top": "▀", "pair_bottom": "▄",
}


def configure_glyphs(unicode_ok: bool) -> None:
    """Choisit le jeu de caractères (UTF-8 ou repli ASCII intégrable)."""
    global UNICODE
    UNICODE = unicode_ok
    if UNICODE:
        GLYPH.update(
            tl="╭", tr="╮", bl="╰", br="╯", h="─", v="│",
            full="█", track="░", dot="●", pair_top="▀", pair_bottom="▄",
        )
    else:
        GLYPH.update(
            tl="+", tr="+", bl="+", br="+", h="-", v="|",
            full="#", track=".", dot="*", pair_top="^", pair_bottom="v",
        )


# ---------------------------------------------------------------------------
# Petites aides couleur / texte
# ---------------------------------------------------------------------------


def _rgb(value: str) -> tuple[int, int, int]:
    value = value.lstrip("#")
    return int(value[0:2], 16), int(value[2:4], 16), int(value[4:6], 16)


def _hex(rgb: Iterable[float]) -> str:
    r, g, b = (max(0, min(255, int(round(c)))) for c in rgb)
    return f"#{r:02x}{g:02x}{b:02x}"


def shade(color: str, factor: float) -> str:
    """Éclaircit (factor > 1) ou assombrit (factor < 1) une couleur."""
    return _hex(c * factor for c in _rgb(color))


def load_color(fraction: float) -> str:
    """Couleur d'une charge 0..1 (bleu froid -> rouge chaud)."""
    f = max(0.0, min(1.0, fraction))
    for (p0, c0), (p1, c1) in zip(LOAD_STOPS, LOAD_STOPS[1:]):
        if f <= p1:
            t = 0.0 if p1 == p0 else (f - p0) / (p1 - p0)
            return _hex(
                (c0[0] + (c1[0] - c0[0]) * t,
                 c0[1] + (c1[1] - c0[1]) * t,
                 c0[2] + (c1[2] - c0[2]) * t)
            )
    return _hex(LOAD_STOPS[-1][1])


def value_color(fraction: float) -> str:
    """Couleur d'une valeur : grise si négligeable, sinon dégradé de charge."""
    return DIM if fraction < 0.04 else load_color(fraction)


def temp_color(temp: Optional[float]) -> str:
    """Couleur d'une température (verte tant que c'est confortable)."""
    if temp is None:
        return DIM
    if temp < 60:
        return GREEN
    if temp < 72:
        return YELLOW
    if temp < 84:
        return ORANGE
    return RED


def pad_to(line: Text, width: int) -> Text:
    """Complète (ou coupe) une ligne pour qu'elle fasse exactement `width`."""
    missing = width - cell_len(line.plain)
    if missing > 0:
        line.append(" " * missing)
    elif missing < 0:
        line.truncate(width, overflow="crop")
    return line


def fit(text_value: str, width: int) -> str:
    if width <= 0:
        return ""
    if cell_len(text_value) <= width:
        return text_value
    if width <= 1:
        return "…"[:width]
    cut = text_value
    while cut and cell_len(cut + "…") > width:
        cut = cut[:-1]
    return cut + "…"


def pct(value: float) -> str:
    return f"{max(0.0, min(100.0, value)):3.0f}%"


# ---------------------------------------------------------------------------
# Barres et sparklines
# ---------------------------------------------------------------------------


def bar(fraction: float, width: int, color: Optional[str] = None) -> Text:
    """Barre de progression lissée (blocs partiels) avec piste discrète."""
    width = max(1, width)
    f = max(0.0, min(1.0, fraction))
    base = color or load_color(f)
    filled = f * width
    full = int(filled)
    eighth = int((filled - full) * 8)
    if eighth >= 8:
        full += 1
        eighth = 0
    full = min(full, width)

    out = Text()
    for i in range(width):
        if i < full:
            char = GLYPH["full"]
            style = shade(base, 0.90 + 0.20 * (i / max(1, width - 1)))
        elif i == full and eighth > 0:
            char = PARTIAL_UNICODE[eighth - 1] if UNICODE else PARTIAL_ASCII
            style = shade(base, 1.02)
        else:
            char = GLYPH["track"]
            style = TRACK
        out.append(char, style=style)
    return out


def bar_shade(base: str, index: int, width: int) -> str:
    """Légère variation de luminosité le long d'une barre."""
    return shade(base, 0.90 + 0.20 * (index / max(1, width - 1)))


def paired_bar(
    top: float,
    bottom: float,
    width: int,
    top_color: Optional[str] = None,
    bottom_color: Optional[str] = None,
) -> Text:
    """Barre unique dont la moitié haute et la moitié basse sont deux threads.

    Le caractère ▀ porte la couleur du thread du haut en premier plan et celle
    du thread du bas en arrière-plan : chaque cellule montre donc les deux
    charges superposées, sans les séparer horizontalement. Les couleurs
    ``top_color``/``bottom_color`` forcent un couple fixe (barres PCIe).
    """
    width = max(1, width)
    top = max(0.0, min(1.0, top))
    bottom = max(0.0, min(1.0, bottom))
    top_cells = int(top * width + 0.5)
    bottom_cells = int(bottom * width + 0.5)
    top_color = top_color or load_color(top)
    bottom_color = bottom_color or load_color(bottom)

    out = Text()
    for index in range(width):
        has_top = index < top_cells
        has_bottom = index < bottom_cells
        top_shade = bar_shade(top_color, index, width)
        bottom_shade = bar_shade(bottom_color, index, width)
        if has_top and has_bottom:
            if UNICODE:
                out.append(
                    GLYPH["pair_top"], style=f"{top_shade} on {bottom_shade}"
                )
            else:
                out.append("#", style=f"bold {top_color}")
        elif has_top:
            out.append(GLYPH["pair_top"], style=f"{top_shade} on {TRACK}")
        elif has_bottom:
            out.append(GLYPH["pair_bottom"], style=f"{bottom_shade} on {TRACK}")
        else:
            out.append(GLYPH["track"], style=TRACK)
    return out


PCIE_LOG_FLOOR_MB_S = 1.0
PCIE_LOG_CEIL_MB_S = 10_000.0


def pcie_log_fraction(mb_s: Optional[float]) -> float:
    """Position logarithmique 1 Mo/s → 10 Go/s pour la barre PCIe."""
    if mb_s is None or mb_s <= PCIE_LOG_FLOOR_MB_S:
        return 0.0
    return min(
        1.0,
        math.log(mb_s / PCIE_LOG_FLOOR_MB_S)
        / math.log(PCIE_LOG_CEIL_MB_S / PCIE_LOG_FLOOR_MB_S),
    )


def sparkline(
    values: Sequence[float],
    width: int,
    color: str,
    alerts: Optional[Sequence[bool]] = None,
) -> Text:
    """Historique compact, aligné à droite (le plus récent en dernier).

    `alerts` peut marquer, échantillon par échantillon, les secondes où un
    thread a saturé : ces caractères passent en rouge, quelle que soit la
    hauteur de la courbe.
    """
    width = max(0, width)
    vals = list(values)[-width:] if width else []
    flags = list(alerts)[-len(vals):] if alerts else []
    offset = len(vals) - len(flags)
    out = Text()
    out.append(" " * max(0, width - len(vals)))
    palette = SPARK_UNICODE if UNICODE else SPARK_ASCII
    for index, value in enumerate(vals):
        level = int(max(0.0, min(100.0, value)) / 100.0 * 7.999)
        alert = index >= offset and bool(flags[index - offset])
        if alert:
            style = RED
        elif level > 0:
            style = color
        else:
            style = shade(color, 0.55)
        out.append(palette[level], style=style)
    return out


# ---------------------------------------------------------------------------
# Cartes (bordures arrondies, titre à gauche, valeur à droite)
# ---------------------------------------------------------------------------


def card(
    width: int,
    height: int,
    title: Text,
    right: Optional[Text],
    body: Sequence[Text],
) -> list[Text]:
    """Construit une carte de width × height lignes, chacune de largeur exacte."""
    width = max(12, width)
    height = max(3, height)
    inner = width - 2
    content_w = inner - 2  # une espace de marge de chaque côté

    # --- bordure supérieure -------------------------------------------------
    left_len = cell_len(title.plain)
    max_right = inner - left_len - 8
    if right is not None and max_right < 4:
        right = None
    if right is not None:
        if cell_len(right.plain) > max_right:
            right = right.copy()
            right.truncate(max_right, overflow="ellipsis")

    top = Text()
    top.append(GLYPH["tl"], style=BORDER)
    top.append(GLYPH["h"] + " ", style=BORDER)
    top.append_text(title)
    top.append(" ", style=BORDER)

    if right is not None:
        right_len = cell_len(right.plain)
        fill = inner - (left_len + 3) - (right_len + 3)
        top.append(GLYPH["h"] * max(0, fill), style=BORDER)
        top.append(" ", style=BORDER)
        top.append_text(right)
        top.append(" " + GLYPH["h"], style=BORDER)
    else:
        fill = inner - (left_len + 3)
        top.append(GLYPH["h"] * max(0, fill), style=BORDER)
    top.append(GLYPH["tr"], style=BORDER)
    pad_to(top, width)

    # --- corps --------------------------------------------------------------
    lines = [top]
    for row in body[: height - 2]:
        line = Text()
        line.append(GLYPH["v"] + " ", style=BORDER)
        line.append_text(pad_to(row.copy(), content_w))
        line.append(" " + GLYPH["v"], style=BORDER)
        pad_to(line, width)
        lines.append(line)
    while len(lines) < height - 1:
        line = Text()
        line.append(GLYPH["v"], style=BORDER)
        line.append(" " * (width - 2))
        line.append(GLYPH["v"], style=BORDER)
        lines.append(line)

    # --- bordure inférieure -------------------------------------------------
    bottom = Text()
    bottom.append(GLYPH["bl"], style=BORDER)
    bottom.append(GLYPH["h"] * inner, style=BORDER)
    bottom.append(GLYPH["br"], style=BORDER)
    lines.append(bottom)
    return lines


def side_by_side(
    left: Sequence[Text], right: Sequence[Text], gap: int
) -> list[Text]:
    """Colle deux blocs de cartes de même hauteur côte à côte."""
    lines: list[Text] = []
    for l_line, r_line in zip(left, right):
        line = Text()
        line.append_text(l_line)
        line.append(" " * gap)
        line.append_text(r_line)
        lines.append(line)
    return lines


# ---------------------------------------------------------------------------
# Topologie et capteurs
# ---------------------------------------------------------------------------


def parse_cpulist(value: str) -> list[int]:
    """Analyse « 0,20 » ou « 0-3,8 » comme dans sysfs."""
    result: list[int] = []
    for part in value.strip().split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            first, last = part.split("-", 1)
            result.extend(range(int(first), int(last) + 1))
        else:
            result.append(int(part))
    return result


def cpu_model() -> str:
    try:
        with open("/proc/cpuinfo", "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if line.startswith("model name"):
                    name = line.split(":", 1)[1].strip()
                    name = name.split(" @")[0]
                    for token in ("Intel(R) ", "CPU ", "(R)", "(TM)"):
                        name = name.replace(token, "")
                    return " ".join(name.split())
    except OSError:
        pass
    return "CPU"


@dataclass(frozen=True)
class Core:
    package: int
    index: int
    core_id: int
    threads: tuple[int, ...]


@dataclass
class Topology:
    cores: list[Core]
    packages: list[int]
    model: str


def detect_topology() -> Topology:
    """Détecte sockets, cœurs physiques et frères SMT depuis sysfs."""
    base = Path("/sys/devices/system/cpu")
    found: dict[tuple[int, int], tuple[int, ...]] = {}
    for entry in sorted(base.glob("cpu[0-9]*")):
        suffix = entry.name[3:]
        if not suffix.isdigit():
            continue
        topology = entry / "topology"
        if not topology.is_dir():
            continue
        try:
            package = int((topology / "physical_package_id").read_text().strip())
            core_id = int((topology / "core_id").read_text().strip())
            siblings = tuple(
                sorted(parse_cpulist((topology / "thread_siblings_list").read_text()))
            )
        except (OSError, ValueError):
            continue
        if siblings:
            found[(package, core_id)] = siblings

    if not found:
        # Repli : deux threads par cœur, un seul socket.
        count = os.cpu_count() or 1
        cores = [
            Core(0, i, i, (i, i + count // 2))
            for i in range(count // 2)
        ]
        return Topology(cores, [0], cpu_model())

    cores: list[Core] = []
    packages = sorted({package for package, _ in found})
    for package in packages:
        entries = sorted(
            ((core_id, siblings) for (pkg, core_id), siblings in found.items() if pkg == package),
            key=lambda item: (min(item[1]), item[0]),
        )
        for index, (core_id, siblings) in enumerate(entries):
            cores.append(Core(package, index, core_id, siblings))
    return Topology(cores, packages, cpu_model())


def read_cpu_times() -> list[tuple[int, ...]]:
    """Temps CPU par thread depuis /proc/stat (index = numéro de CPU)."""
    times: list[tuple[int, ...]] = []
    try:
        with open("/proc/stat", "r", encoding="ascii", errors="replace") as fh:
            for line in fh:
                if not line.startswith("cpu") or len(line) < 4 or line[3] == " ":
                    continue
                fields = line.split()
                if not fields[0][3:].isdigit():
                    continue
                times.append(tuple(int(value) for value in fields[1:]))
    except OSError:
        pass
    return times


def read_meminfo() -> dict[str, int]:
    values: dict[str, int] = {}
    try:
        with open("/proc/meminfo", "r", encoding="ascii", errors="replace") as fh:
            for line in fh:
                key, _, rest = line.partition(":")
                try:
                    values[key] = int(rest.split()[0]) * 1024
                except (IndexError, ValueError):
                    continue
    except OSError:
        pass
    return values


def read_uptime() -> float:
    try:
        return float(Path("/proc/uptime").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return 0.0


def read_frequencies(cpu_count: int) -> list[Optional[float]]:
    """Fréquence instantanée (GHz) de chaque thread, None si indisponible."""
    values: list[Optional[float]] = []
    base = Path("/sys/devices/system/cpu")
    for index in range(cpu_count):
        try:
            khz = int((base / f"cpu{index}" / "cpufreq" / "scaling_cur_freq").read_text())
            values.append(khz / 1_000_000.0)
        except (OSError, ValueError):
            values.append(None)
    return values


class PackageTempReader:
    """Températures « Package id N » des sockets, via hwmon/coretemp."""

    def __init__(self) -> None:
        self._paths: dict[int, Path] = {}
        self._scan()

    def _scan(self) -> None:
        self._paths = {}
        for hwmon in Path("/sys/class/hwmon").glob("hwmon*"):
            try:
                if (hwmon / "name").read_text().strip() != "coretemp":
                    continue
            except OSError:
                continue
            for label_file in hwmon.glob("temp*_label"):
                try:
                    label = label_file.read_text().strip()
                except OSError:
                    continue
                if not label.startswith("Package id "):
                    continue
                try:
                    package = int(label.rsplit(" ", 1)[1])
                except (IndexError, ValueError):
                    continue
                input_file = label_file.with_name(
                    label_file.name.replace("_label", "_input")
                )
                self._paths[package] = input_file

    def read(self) -> dict[int, float]:
        values: dict[int, float] = {}
        stale = False
        for package, path in self._paths.items():
            try:
                values[package] = int(path.read_text().strip()) / 1000.0
            except (OSError, ValueError):
                stale = True
        if stale or not self._paths:
            self._scan()
        return values


# ---------------------------------------------------------------------------
# GPU
# ---------------------------------------------------------------------------

GPU_FIELDS = (
    "index,name,utilization.gpu,memory.used,memory.total,temperature.gpu,"
    "fan.speed,power.draw,power.limit,clocks.current.graphics,"
    "clocks.current.memory,pcie.link.gen.current,pcie.link.width.current,"
    "pcie.link.gen.max,pcie.link.width.max"
)


@dataclass
class Gpu:
    index: int
    name: str
    util: Optional[float]
    mem_used: int
    mem_total: int
    temp: Optional[float]
    fan: Optional[float]
    power: Optional[float]
    power_limit: Optional[float]
    clock: Optional[float]
    mem_clock: Optional[float]
    pcie_gen: Optional[int] = None
    pcie_width: Optional[int] = None
    pcie_gen_max: Optional[int] = None
    pcie_width_max: Optional[int] = None
    pcie_tx_mb_s: Optional[float] = None
    pcie_rx_mb_s: Optional[float] = None
    tensor: Optional[float] = None

    @property
    def mem_fraction(self) -> float:
        return self.mem_used / self.mem_total if self.mem_total else 0.0

    @property
    def power_fraction(self) -> float:
        if not self.power or not self.power_limit:
            return 0.0
        return self.power / self.power_limit


def _number(value: str) -> Optional[float]:
    value = value.strip()
    if not value or value.startswith("[") or value.lower() in ("n/a", "na", "-"):
        return None
    try:
        return float(value)
    except ValueError:
        return None


def read_gpus(nvml: Optional["Nvml"] = None) -> tuple[list[Gpu], Optional[str], float]:
    """Interroge nvidia-smi. Retourne (GPU, erreur éventuelle, latence ms)."""
    exe = shutil.which("nvidia-smi")
    if not exe:
        return [], "nvidia-smi introuvable", 0.0
    started = time.monotonic()
    try:
        proc = subprocess.run(
            [
                exe,
                f"--query-gpu={GPU_FIELDS}",
                "--format=csv,noheader,nounits",
            ],
            capture_output=True,
            text=True,
            timeout=2.5,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return [], f"nvidia-smi : {exc}", (time.monotonic() - started) * 1000.0
    latency = (time.monotonic() - started) * 1000.0
    if proc.returncode != 0:
        message = (proc.stderr or proc.stdout).strip().splitlines()
        return [], message[0] if message else f"code {proc.returncode}", latency

    gpus: list[Gpu] = []
    for row in csv.reader(proc.stdout.splitlines(), skipinitialspace=True):
        if len(row) < 15:
            continue
        try:
            index = int(row[0])
        except ValueError:
            continue
        mem_used = _number(row[3]) or 0.0
        mem_total = _number(row[4]) or 0.0
        pcie_gen = _number(row[11])
        pcie_width = _number(row[12])
        pcie_gen_max = _number(row[13])
        pcie_width_max = _number(row[14])
        tx_mb_s, rx_mb_s = (
            nvml.pcie_mb_s(index) if nvml is not None else (None, None)
        )
        gpus.append(
            Gpu(
                index=index,
                name=row[1].strip(),
                util=_number(row[2]),
                mem_used=int(mem_used * 1024 * 1024),
                mem_total=int(mem_total * 1024 * 1024),
                temp=_number(row[5]),
                fan=_number(row[6]),
                power=_number(row[7]),
                power_limit=_number(row[8]),
                clock=_number(row[9]),
                mem_clock=_number(row[10]),
                pcie_gen=int(pcie_gen) if pcie_gen is not None else None,
                pcie_width=int(pcie_width) if pcie_width is not None else None,
                pcie_gen_max=int(pcie_gen_max) if pcie_gen_max is not None else None,
                pcie_width_max=int(pcie_width_max) if pcie_width_max is not None else None,
                pcie_tx_mb_s=tx_mb_s,
                pcie_rx_mb_s=rx_mb_s,
            )
        )
    gpus.sort(key=lambda gpu: gpu.index)
    return gpus, None, latency


# ---------------------------------------------------------------------------
# NVML (débit PCIe), NVLink, QPI et carte AMD
# ---------------------------------------------------------------------------


class Nvml:
    """Accès minimal à NVML pour le débit PCIe ; repli silencieux si absent."""

    def __init__(self) -> None:
        self.lib = None
        self.handles: dict[int, ctypes.c_void_p] = {}
        try:
            lib = ctypes.CDLL("libnvidia-ml.so.1")
            lib.nvmlInit_v2.restype = ctypes.c_int
            if lib.nvmlInit_v2() != 0:
                return
            lib.nvmlDeviceGetHandleByIndex_v2.argtypes = [
                ctypes.c_uint,
                ctypes.POINTER(ctypes.c_void_p),
            ]
            lib.nvmlDeviceGetHandleByIndex_v2.restype = ctypes.c_int
            lib.nvmlDeviceGetPcieThroughput.argtypes = [
                ctypes.c_void_p,
                ctypes.c_uint,
                ctypes.POINTER(ctypes.c_uint),
            ]
            lib.nvmlDeviceGetPcieThroughput.restype = ctypes.c_int
            self.lib = lib
        except (OSError, AttributeError):
            self.lib = None

    def pcie_mb_s(self, index: int) -> tuple[Optional[float], Optional[float]]:
        """Débit PCIe (TX, RX) en Mo/s, ou (None, None) si indisponible."""
        if self.lib is None:
            return None, None
        handle = self.handles.get(index)
        if handle is None:
            handle = ctypes.c_void_p()
            if self.lib.nvmlDeviceGetHandleByIndex_v2(index, ctypes.byref(handle)) != 0:
                return None, None
            self.handles[index] = handle
        values: list[Optional[float]] = []
        for counter in (0, 1):
            value = ctypes.c_uint()
            if self.lib.nvmlDeviceGetPcieThroughput(
                handle, counter, ctypes.byref(value)
            ) != 0:
                values.append(None)
            else:
                values.append(value.value / 1000.0)  # KB/s -> Mo/s
        return values[0], values[1]


@dataclass
class NvLink:
    speed_gbps: Optional[float]
    links: int
    errors: int
    throughput: Optional[float] = None


@dataclass
class Qpi:
    sockets: int
    links: Optional[int]
    speed_gt_s: Optional[float]
    traffic: Optional[float]
    reason: Optional[str]


@dataclass
class Amd:
    name: str
    temp: Optional[float]
    vram_used: int
    vram_total: int
    sclk_mhz: Optional[float]
    mclk_mhz: Optional[float]
    fan_pct: Optional[float]
    pstate: Optional[str]
    pcie_gen: Optional[int]
    pcie_width: Optional[int]
    busy: Optional[float]


def read_nvlink() -> NvLink:
    """État, vitesse et erreurs NVLink ; le débit n'est pas exposé sur Turing."""
    exe = shutil.which("nvidia-smi")
    if not exe:
        return NvLink(speed_gbps=None, links=0, errors=0)
    speed: Optional[float] = None
    lines = 0
    try:
        proc = subprocess.run(
            [exe, "nvlink", "-s"], capture_output=True, text=True, timeout=3
        )
        for line in proc.stdout.splitlines():
            match = re.search(r"Link\s+\d+:\s*([0-9.]+)\s*GB/s", line)
            if match:
                value = float(match.group(1))
                lines += 1
                speed = value if speed is None else max(speed, value)
    except (OSError, subprocess.TimeoutExpired):
        pass
    errors = 0
    try:
        proc = subprocess.run(
            [exe, "nvlink", "-e"], capture_output=True, text=True, timeout=3
        )
        errors = sum(int(value) for value in re.findall(r"Errors:\s*(\d+)", proc.stdout))
    except (OSError, subprocess.TimeoutExpired):
        pass
    # Les lignes -s sont répétées sur les deux extrémités du même lien.
    return NvLink(speed_gbps=speed, links=1 if lines else 0, errors=errors)


def read_qpi(topology: Topology) -> Qpi:
    """Topologie QPI connue ; le débit exige perf et des privilèges."""
    links: Optional[int] = None
    speed: Optional[float] = None
    if "E5-2687W v3" in topology.model:
        links = 2
        speed = 9.6
    if links is None:
        reason = "modèle inconnu"
    elif shutil.which("perf") is None:
        reason = "perf absent"
    else:
        try:
            paranoid = int(
                Path("/proc/sys/kernel/perf_event_paranoid").read_text().strip()
            )
        except (OSError, ValueError):
            paranoid = 2
        reason = "perf restreint" if paranoid > 1 else "intégration perf non faite"
    return Qpi(
        sockets=len(topology.packages),
        links=links,
        speed_gt_s=speed,
        traffic=None,
        reason=reason,
    )


AMD_NAMES = {"1002:683f": "Radeon HD 7750"}


class AmdReader:
    """Carte AMD pilotée par amdgpu : température, VRAM, horloges, ventilateur."""

    def __init__(self) -> None:
        self.device: Optional[Path] = None
        self.hwmon: Optional[Path] = None
        self.name = "AMD GPU"
        self.pci_id = ""
        for card in Path("/sys/class/drm").glob("card[0-9]*"):
            device = card / "device"
            try:
                if (device / "vendor").read_text().strip() != "0x1002":
                    continue
            except OSError:
                continue
            self.device = device
            try:
                uevent = (device / "uevent").read_text()
                match = re.search(r"PCI_ID=([0-9A-Fa-f:]+)", uevent)
                if match:
                    self.pci_id = match.group(1).lower()
                    self.name = AMD_NAMES.get(self.pci_id, "AMD GPU")
            except OSError:
                pass
            for hwmon in (device / "hwmon").glob("hwmon*"):
                try:
                    if (hwmon / "name").read_text().strip() == "amdgpu":
                        self.hwmon = hwmon
                        break
                except OSError:
                    continue
            break

    def _read(self, path: Path) -> Optional[str]:
        try:
            return path.read_text().strip()
        except OSError:
            return None

    def _number(self, path: Path, scale: float = 1.0) -> Optional[float]:
        value = self._read(path)
        if value is None:
            return None
        try:
            return int(value) / scale
        except ValueError:
            try:
                return float(value) / scale
            except ValueError:
                return None

    def read(self) -> Optional[Amd]:
        if self.device is None:
            return None
        hwmon = self.hwmon
        temp = self._number(hwmon / "temp1_input", 1000) if hwmon else None
        sclk = self._number(hwmon / "freq1_input", 1e6) if hwmon else None
        mclk = self._number(hwmon / "freq2_input", 1e6) if hwmon else None
        pwm = self._number(hwmon / "pwm1") if hwmon else None
        pwm_max = self._number(hwmon / "pwm1_max") if hwmon else None
        fan_pct = 100.0 * pwm / pwm_max if pwm is not None and pwm_max else None
        vram_used = int(self._number(self.device / "mem_info_vram_used") or 0)
        vram_total = int(self._number(self.device / "mem_info_vram_total") or 0)
        pstate = self._read(self.device / "power_dpm_state")
        speed = self._read(self.device / "current_link_speed")
        pcie_gen = None
        if speed:
            try:
                pcie_gen = {2.5: 1, 5.0: 2, 8.0: 3, 16.0: 4, 32.0: 5}.get(
                    float(speed.split()[0])
                )
            except ValueError:
                pass
        pcie_width = self._number(self.device / "current_link_width")
        busy = self._number(self.device / "gpu_busy_percent")
        return Amd(
            name=self.name,
            temp=temp,
            vram_used=vram_used,
            vram_total=vram_total,
            sclk_mhz=sclk,
            mclk_mhz=mclk,
            fan_pct=fan_pct,
            pstate=pstate,
            pcie_gen=pcie_gen,
            pcie_width=int(pcie_width) if pcie_width is not None else None,
            busy=busy,
        )


# ---------------------------------------------------------------------------
# Métriques agrégées
# ---------------------------------------------------------------------------


@dataclass
class Metrics:
    cpu_pct: list[float]
    cpu_total: float
    socket_pct: dict[int, float]
    socket_freq: dict[int, Optional[float]]
    socket_temp: dict[int, Optional[float]]
    freq_avg: Optional[float]
    ram_used: int
    ram_total: int
    ram_pct: float
    swap_used: int
    swap_total: int
    vram_used: int
    vram_total: int
    vram_pct: float
    load: tuple[float, float, float]
    uptime: float
    gpus: list[Gpu]
    gpu_error: Optional[str]
    nvlink: Optional[NvLink] = None
    qpi: Optional[Qpi] = None
    amd: Optional[Amd] = None


@dataclass
class History:
    cpu: deque = field(default_factory=lambda: deque(maxlen=600))
    socket: dict[int, deque] = field(default_factory=dict)
    ram: deque = field(default_factory=lambda: deque(maxlen=600))
    vram: deque = field(default_factory=lambda: deque(maxlen=600))
    gpu: dict[int, deque] = field(default_factory=dict)
    saturated: deque = field(default_factory=lambda: deque(maxlen=600))
    socket_saturated: dict[int, deque] = field(default_factory=dict)

    def ensure_socket(self, package: int) -> deque:
        return self.socket.setdefault(package, deque(maxlen=600))

    def ensure_socket_saturated(self, package: int) -> deque:
        return self.socket_saturated.setdefault(package, deque(maxlen=600))

    def ensure_gpu(self, index: int) -> deque:
        return self.gpu.setdefault(index, deque(maxlen=600))

    def clear(self) -> None:
        self.cpu.clear()
        self.ram.clear()
        self.vram.clear()
        self.saturated.clear()
        for series in self.socket.values():
            series.clear()
        for series in self.socket_saturated.values():
            series.clear()
        for series in self.gpu.values():
            series.clear()


class Sampler:
    """Échantillonne le système à intervalle régulier et tient l'historique."""

    def __init__(self, interval: float) -> None:
        self.interval = interval
        self.topology = detect_topology()
        self.temps = PackageTempReader()
        self.prev_cpu = read_cpu_times()
        self.metrics: Optional[Metrics] = None
        self.history = History()
        self.samples = 0
        self.gpu_latency_ms = 0.0
        self.nvml = Nvml()
        self.nvlink = read_nvlink()
        self.qpi = read_qpi(self.topology)
        self.amd = AmdReader()

    # -- échantillonnage ----------------------------------------------------
    def reset_cpu_baseline(self) -> None:
        """Reprend une référence CPU propre (après une pause prolongée)."""
        self.prev_cpu = read_cpu_times()

    def prime(self) -> None:
        """Attend une demi-seconde puis prend la première mesure utile.

        La référence /proc/stat a été lue dans __init__ : la fenêtre couvre
        donc bien le sommeil, et non un delta de quelques microsecondes.
        """
        time.sleep(0.5)
        self.sample(record=True)

    def sample(self, record: bool = True) -> Metrics:
        times = read_cpu_times()

        per_thread: list[float] = []
        for index, current in enumerate(times):
            if index >= len(self.prev_cpu):
                per_thread.append(0.0)
                continue
            previous = self.prev_cpu[index]
            deltas = [c - p for c, p in zip(current, previous)]
            total = sum(deltas)
            idle = deltas[3] if len(deltas) > 3 else 0
            if len(deltas) > 4:
                idle += deltas[4]  # iowait
            if total <= 0:
                per_thread.append(0.0)
            else:
                per_thread.append(max(0.0, min(100.0, 100.0 * (total - idle) / total)))
        self.prev_cpu = times

        gpus, gpu_error, gpu_latency = read_gpus(self.nvml)
        self.gpu_latency_ms = gpu_latency
        if record and self.samples % 10 == 0:
            self.nvlink = read_nvlink()
        amd = self.amd.read()

        mem = read_meminfo()
        ram_total = mem.get("MemTotal", 0)
        ram_available = mem.get("MemAvailable")
        if ram_available is None:
            # Noyaux anciens : reconstruction de l'équivalent « available ».
            ram_available = (
                mem.get("MemFree", 0) + mem.get("Buffers", 0) + mem.get("Cached", 0)
            )
        ram_used = max(0, ram_total - ram_available)
        swap_total = mem.get("SwapTotal", 0)
        swap_used = max(0, swap_total - mem.get("SwapFree", 0))

        vram_used = sum(gpu.mem_used for gpu in gpus)
        vram_total = sum(gpu.mem_total for gpu in gpus)

        frequencies = read_frequencies(len(times))
        temps = self.temps.read()

        socket_pct: dict[int, float] = {}
        socket_freq: dict[int, Optional[float]] = {}
        socket_temp: dict[int, Optional[float]] = {}
        socket_saturated: dict[int, bool] = {}
        for package in self.topology.packages:
            values = [
                per_thread[t]
                for core in self.topology.cores
                if core.package == package
                for t in core.threads
                if 0 <= t < len(per_thread)
            ]
            socket_pct[package] = sum(values) / len(values) if values else 0.0
            socket_saturated[package] = any(
                value >= SATURATED_THRESHOLD for value in values
            )
            freqs = [
                frequencies[t]
                for core in self.topology.cores
                if core.package == package
                for t in core.threads
                if 0 <= t < len(frequencies) and frequencies[t] is not None
            ]
            socket_freq[package] = sum(freqs) / len(freqs) if freqs else None
            socket_temp[package] = temps.get(package)

        freq_avg = None
        known_freqs = [value for value in frequencies if value is not None]
        if known_freqs:
            freq_avg = sum(known_freqs) / len(known_freqs)

        cpu_total = sum(per_thread) / len(per_thread) if per_thread else 0.0
        try:
            load = os.getloadavg()
        except OSError:
            load = (0.0, 0.0, 0.0)

        metrics = Metrics(
            cpu_pct=per_thread,
            cpu_total=cpu_total,
            socket_pct=socket_pct,
            socket_freq=socket_freq,
            socket_temp=socket_temp,
            freq_avg=freq_avg,
            ram_used=ram_used,
            ram_total=ram_total,
            ram_pct=100.0 * ram_used / ram_total if ram_total else 0.0,
            swap_used=swap_used,
            swap_total=swap_total,
            vram_used=vram_used,
            vram_total=vram_total,
            vram_pct=100.0 * vram_used / vram_total if vram_total else 0.0,
            load=load,
            uptime=read_uptime(),
            gpus=gpus,
            gpu_error=gpu_error,
            nvlink=self.nvlink,
            qpi=self.qpi,
            amd=amd,
        )
        self.metrics = metrics

        if record:
            self.history.cpu.append(cpu_total)
            self.history.ram.append(metrics.ram_pct)
            self.history.vram.append(metrics.vram_pct)
            self.history.saturated.append(
                any(value >= SATURATED_THRESHOLD for value in per_thread)
            )
            for package in self.topology.packages:
                self.history.ensure_socket(package).append(socket_pct.get(package, 0.0))
                self.history.ensure_socket_saturated(package).append(
                    socket_saturated.get(package, False)
                )
            for gpu in gpus:
                self.history.ensure_gpu(gpu.index).append(gpu.util or 0.0)
            self.samples += 1
        return metrics


# ---------------------------------------------------------------------------
# Formatage
# ---------------------------------------------------------------------------


def fmt_gio(value: int) -> str:
    return f"{value / (1024 ** 3):.1f}"


def fmt_uptime(seconds: float) -> str:
    seconds = max(0, int(seconds))
    days, rest = divmod(seconds, 86400)
    hours, rest = divmod(rest, 3600)
    minutes = rest // 60
    if days:
        return f"{days}j {hours:02d}h"
    if hours:
        return f"{hours}h {minutes:02d}m"
    return f"{minutes}m"


# ---------------------------------------------------------------------------
# Rendu des sections
# ---------------------------------------------------------------------------


def choose(candidates: Sequence[Text], width: int) -> Optional[Text]:
    for candidate in candidates:
        if cell_len(candidate.plain) <= width:
            return candidate
    return candidates[-1] if candidates else None


def details_line(parts: Sequence[tuple[str, Optional[str]]], width: int) -> Text:
    """Assemble des fragments « valeur » séparés par « · », en coupant au besoin."""
    chosen: list[tuple[str, Optional[str]]] = []
    for label, style in parts:
        candidate = " · ".join([item[0] for item in chosen] + [label])
        if cell_len(candidate) <= width:
            chosen.append((label, style))
        elif not chosen:
            chosen.append((fit(label, width), style))
        else:
            break
    out = Text()
    for index, (label, style) in enumerate(chosen):
        if index:
            out.append(" · ", style=DIM_SOFT)
        out.append(label, style=style or DIM)
    return out


def header_block(m: Metrics, width: int, clock: str, dot_style: str) -> list[Text]:
    content_w = width - 4
    gap = 3
    budget = max(20, (content_w - 2 * gap) // 3)
    bar_w = max(6, budget - 10)

    chips = [
        ("CPU", m.cpu_total / 100.0, CPU_C),
        ("RAM", m.ram_pct / 100.0, GREEN),
        ("VRAM", m.vram_pct / 100.0, GPU_C),
    ]
    row1 = Text()
    for index, (label, fraction, accent) in enumerate(chips):
        if index:
            row1.append(" " * gap)
        chip = Text()
        chip.append(f"{label:<4} ", style=f"bold {accent}")
        chip.append_text(bar(fraction, bar_w))
        chip.append(" " + pct(fraction * 100.0), style=f"bold {value_color(fraction)}")
        row1.append_text(pad_to(chip, budget))
    pad_to(row1, content_w)

    # Détails alignés sous chaque indicateur.
    cpu_parts: list[tuple[str, Optional[str]]] = [
        (f"{len(m.cpu_pct)} thr", DIM),
    ]
    if m.freq_avg is not None:
        cpu_parts.append((f"{m.freq_avg:.2f} GHz", DIM))
    temps = [value for value in m.socket_temp.values() if value is not None]
    if temps:
        cpu_parts.append(
            (
                "/".join(f"{value:.0f}" for value in temps) + "°C",
                temp_color(max(temps)),
            )
        )
    cpu_detail = details_line(cpu_parts, budget)

    ram_parts: list[tuple[str, Optional[str]]] = [
        (f"{fmt_gio(m.ram_used)}/{fmt_gio(m.ram_total)} Gio", DIM),
    ]
    if m.swap_total:
        ram_parts.append((f"swap {fmt_gio(m.swap_used)}", DIM))
    ram_detail = details_line(ram_parts, budget)

    vram_parts: list[tuple[str, Optional[str]]] = [
        (f"{fmt_gio(m.vram_used)}/{fmt_gio(m.vram_total)} Gio", DIM),
    ]
    vram_detail = details_line(vram_parts, budget)

    row2 = Text()
    for index, detail in enumerate((cpu_detail, ram_detail, vram_detail)):
        if index:
            row2.append(" " * gap)
        row2.append_text(pad_to(detail, budget))
    pad_to(row2, content_w)

    title = Text()
    title.append(GLYPH["dot"], style=dot_style)
    title.append(" HAURU", style=f"bold {TITLE}")
    right_candidates = [
        Text(f"{clock}  ·  charge {m.load[0]:.2f}  ·  up {fmt_uptime(m.uptime)}", style=DIM),
        Text(f"{clock}  ·  charge {m.load[0]:.2f}", style=DIM),
        Text(clock, style=DIM),
    ]
    right = choose(right_candidates, width - 2 - cell_len(title.plain) - 8)
    return card(width, 4, title, right, [row1, row2])


def cpu_card(socket: int, m: Metrics, topology: Topology, width: int) -> list[Text]:
    content_w = width - 4
    bar_w = max(8, content_w - 21)

    cores = [core for core in topology.cores if core.package == socket]
    body: list[Text] = []
    for core in cores:
        values = []
        for thread in core.threads:
            values.append(m.cpu_pct[thread] if 0 <= thread < len(m.cpu_pct) else 0.0)
        while len(values) < 2:
            values.append(0.0)

        row = Text()
        row.append(f"{core.index:02d} ", style=f"bold {CPU_C}")
        pair = "/".join(f"{thread:02d}" for thread in core.threads)
        row.append(f"{pair:<5}", style=DIM)
        row.append(" ")
        row.append_text(paired_bar(values[0] / 100.0, values[1] / 100.0, bar_w))
        row.append(" ")
        top_color = value_color(values[0] / 100.0)
        bottom_color = value_color(values[1] / 100.0)
        row.append(GLYPH["pair_top"], style=f"bold {top_color}")
        row.append(pct(values[0]), style=f"bold {top_color}")
        row.append(" ")
        row.append(GLYPH["pair_bottom"], style=f"bold {bottom_color}")
        row.append(pct(values[1]), style=f"bold {bottom_color}")
        body.append(pad_to(row, content_w))

    # Titre : modèle si la place le permet, sinon « 10C/20T ».
    load = m.socket_pct.get(socket, 0.0)
    freq = m.socket_freq.get(socket)
    temp = m.socket_temp.get(socket)
    threads = sum(len(core.threads) for core in cores)
    left_candidates = [
        Text(f"CPU {socket}", style=f"bold {CPU_C}").append(
            f" · {topology.model}", style=DIM
        ),
        Text(f"CPU {socket}", style=f"bold {CPU_C}").append(
            f" · {len(cores)}C/{threads}T", style=DIM
        ),
        Text(f"CPU {socket}", style=f"bold {CPU_C}"),
    ]
    # On garde le premier titre qui laisse de la place à la valeur de droite.
    title = left_candidates[-1]
    for candidate in left_candidates:
        if width - 2 - cell_len(candidate.plain) - 8 >= 10:
            title = candidate
            break

    color = f"bold {value_color(load / 100.0)}"

    def right_variant(with_temp: bool, with_freq: bool) -> Text:
        out = Text(pct(load), style=color)
        if with_temp and temp is not None:
            out.append(" · ", style=DIM_SOFT)
            out.append(f"{temp:.0f}°C", style=f"bold {temp_color(temp)}")
        if with_freq and freq is not None:
            out.append(" · ", style=DIM_SOFT)
            out.append(f"{freq:.2f} GHz", style=DIM)
        return out

    right_candidates: list[Text] = []
    if temp is not None and freq is not None:
        right_candidates.append(right_variant(True, True))
    if temp is not None:
        right_candidates.append(right_variant(True, False))
    if freq is not None:
        right_candidates.append(right_variant(False, True))
    right_candidates.append(right_variant(False, False))
    max_right = width - 2 - cell_len(title.plain) - 8
    right = choose(right_candidates, max_right)
    return card(width, 12, title, right, body)


def gpu_body(gpu: Gpu, content_w: int) -> list[Text]:
    # Colonnes alignées : libellé (4) + 2, barre, 2, valeur, puis pourcentage.
    value_w = 13  # « 15.9/15.9 Gio »
    bar_w = max(6, content_w - 6 - value_w - 2 - 4)
    rows: list[Text] = []

    util = gpu.util
    row = Text()
    row.append("UTIL", style=f"bold {GPU_C}")
    row.append("  ")
    row.append_text(bar((util or 0.0) / 100.0, bar_w))
    label = pct(util) if util is not None else "  —%"
    row.append(" " + label, style=f"bold {value_color((util or 0.0) / 100.0)}")
    if gpu.fan is not None:
        fan_text = f"{gpu.fan:.0f}% fan"
        padding = content_w - cell_len(row.plain) - cell_len(fan_text)
        if padding > 0:
            row.append(" " * padding + fan_text, style=DIM)
    rows.append(pad_to(row, content_w))

    mem_value = f"{fmt_gio(gpu.mem_used)}/{fmt_gio(gpu.mem_total)} Gio"
    row = Text()
    row.append("VRAM", style=f"bold {GPU_C}")
    row.append("  ")
    row.append_text(bar(gpu.mem_fraction, bar_w))
    row.append(" " + mem_value, style=TEXT)
    tail = pct(gpu.mem_fraction * 100.0)
    padding = content_w - cell_len(row.plain) - len(tail)
    row.append(" " * max(1, padding) + tail, style=f"bold {value_color(gpu.mem_fraction)}")
    rows.append(pad_to(row, content_w))

    row = Text()
    row.append("PWR", style=f"bold {GPU_C}")
    row.append("   ")
    if gpu.power is not None:
        row.append_text(bar(gpu.power_fraction, bar_w))
        if gpu.power_limit:
            value = f"{gpu.power:.0f}/{gpu.power_limit:.0f} W"
        else:
            value = f"{gpu.power:.0f} W"
        row.append(" " + value, style=TEXT)
        if gpu.power_limit:
            tail = pct(gpu.power_fraction * 100.0)
            padding = content_w - cell_len(row.plain) - len(tail)
            row.append(
                " " * max(1, padding) + tail,
                style=f"bold {value_color(gpu.power_fraction)}",
            )
    else:
        row.append("—", style=DIM)
    rows.append(pad_to(row, content_w))

    row = Text()
    row.append("CLK", style=f"bold {GPU_C}")
    if gpu.clock is not None:
        row.append(f"   {gpu.clock:.0f} MHz", style=TEXT)
    if gpu.mem_clock is not None:
        row.append(f" · MEM {gpu.mem_clock:.0f} MHz", style=DIM)
    rows.append(pad_to(row, content_w))
    return rows


def gpu_card(gpu: Gpu, width: int, height: int = 6) -> list[Text]:
    content_w = width - 4
    title = Text(f"GPU {gpu.index}", style=f"bold {GPU_C}")
    name_room = cell_len(title.plain) + 3 + cell_len(gpu.name) + 1
    if name_room <= width - 14:
        title.append(f" · {gpu.name}", style=DIM)
    right = Text()
    if gpu.temp is not None:
        right.append(f"{gpu.temp:.0f}°C", style=f"bold {temp_color(gpu.temp)}")
    else:
        right.append("—", style=DIM)
    body = gpu_body(gpu, content_w)
    if height < 6:
        body = body[: max(1, height - 2)]
    return card(width, height, title, right, body)


def gpu_placeholder(width: int, height: int, message: str) -> list[Text]:
    title = Text("GPU", style=f"bold {GPU_C}")
    body = [Text(fit(message, width - 6), style=ORANGE)]
    return card(width, height, title, None, body)


def history_block(m: Metrics, hist: History, width: int, rows: int, interval: float) -> list[Text]:
    content_w = width - 4
    entries: list[
        tuple[str, Sequence[float], str, float, Optional[Sequence[bool]]]
    ] = []
    if rows >= 6 and len(m.socket_pct) >= 2:
        for package in sorted(m.socket_pct):
            entries.append(
                (
                    f"CPU {package}",
                    hist.socket.get(package, deque()),
                    CPU_C,
                    m.socket_pct[package],
                    hist.socket_saturated.get(package, deque()),
                )
            )
    else:
        entries.append(
            ("CPU", hist.cpu, CPU_C, m.cpu_total, hist.saturated)
        )
    for gpu in m.gpus[:2]:
        series = hist.gpu.get(gpu.index, deque())
        entries.append((f"GPU {gpu.index}", series, GPU_C, gpu.util or 0.0, None))
    entries.append(("RAM", hist.ram, GREEN, m.ram_pct, None))
    entries.append(("VRAM", hist.vram, YELLOW, m.vram_pct, None))
    entries = entries[:rows]

    spark_w = max(4, content_w - 12)
    body: list[Text] = []
    for label, series, color, current, alerts in entries:
        row = Text()
        row.append(f"{label:<6}", style=f"bold {color}")
        row.append_text(sparkline(series, spark_w, color, alerts))
        row.append(" " + pct(current), style=f"bold {color}")
        if alerts and alerts[-1]:
            row.append("!", style=f"bold {RED}")
        else:
            row.append(" ")
        body.append(pad_to(row, content_w))

    title = Text("HISTORIQUE", style=f"bold {TITLE}")
    window = min(len(hist.cpu), spark_w) * interval
    right = Text(f"{window:.0f} s", style=DIM)
    return card(width, len(body) + 2, title, right, body)


def _join_segments(parts: list[tuple[str, str]]) -> list[tuple[str, str]]:
    joined: list[tuple[str, str]] = []
    for index, (text, style) in enumerate(parts):
        if index:
            joined.append((" · ", DIM_SOFT))
        joined.append((text, style))
    return joined


def links_block(m: Metrics, width: int) -> list[Text]:
    """Barres PCIe par lien, puis NVLink/QPI et tensor/AMD (5 lignes)."""
    content_w = max(10, width - 4)
    compact = content_w < 100
    compact_pcie = content_w < 88
    wide_amd = content_w >= 150
    bar_w = max(6, min(30, content_w - 46))
    rows: list[Text] = []
    top_glyph = GLYPH["pair_top"]
    bottom_glyph = GLYPH["pair_bottom"]

    def push(row: Text) -> None:
        rows.append(pad_to(row, content_w))

    def add_parts(row: Text, parts: list[tuple[str, str]]) -> None:
        for text, style in _join_segments(parts):
            row.append(text, style=style)

    # --- PCIe : une barre par lien, TX en haut et RX en bas -------------
    for gpu in m.gpus[:2]:
        gen = f"Gen{gpu.pcie_gen}" if gpu.pcie_gen else "Gen?"
        if (
            not compact_pcie
            and gpu.pcie_gen
            and gpu.pcie_gen_max
            and gpu.pcie_gen != gpu.pcie_gen_max
        ):
            gen = f"Gen{gpu.pcie_gen}/{gpu.pcie_gen_max}"
        lane = f"x{gpu.pcie_width}" if gpu.pcie_width else "x?"
        if (
            not compact_pcie
            and gpu.pcie_width
            and gpu.pcie_width_max
            and gpu.pcie_width != gpu.pcie_width_max
        ):
            lane = f"x{gpu.pcie_width}/{gpu.pcie_width_max}"
        desc = (
            f"G{gpu.index} {gen}{lane}"
            if compact_pcie
            else f"GPU {gpu.index} {gen} {lane}"
        )
        row = Text()
        row.append("PCIE   ", style=f"bold {TITLE}")
        row.append(f"{desc} ", style=TEXT)
        row.append_text(
            paired_bar(
                pcie_log_fraction(gpu.pcie_tx_mb_s),
                pcie_log_fraction(gpu.pcie_rx_mb_s),
                bar_w,
                TITLE,
                GREEN,
            )
        )
        row.append(" ", style=TRACK)
        if gpu.pcie_tx_mb_s is not None and gpu.pcie_rx_mb_s is not None:
            row.append(top_glyph, style=f"bold {TITLE}")
            row.append(f"{gpu.pcie_tx_mb_s:.1f} ", style=TEXT)
            row.append(bottom_glyph, style=f"bold {GREEN}")
            row.append(f"{gpu.pcie_rx_mb_s:.1f} Mo/s", style=TEXT)
        else:
            row.append("débit n/a", style=DIM)
        push(row)

    # --- PCIe AMD : lien connu, débit non exposé ------------------------
    row = Text()
    row.append("PCIE   ", style=f"bold {TITLE}")
    if m.amd and m.amd.pcie_gen:
        lane = f"x{m.amd.pcie_width}" if m.amd.pcie_width else ""
        row.append("AMD ", style=f"bold {ORANGE}")
        row.append(
            f"Gen{m.amd.pcie_gen}{' ' if not compact_pcie else ''}{lane} ",
            style=TEXT,
        )
        row.append_text(paired_bar(0.0, 0.0, bar_w, TITLE, GREEN))
        row.append(" débit n/a", style=DIM)
    else:
        row.append("aucune carte AMD détectée", style=DIM)
    push(row)

    # --- NVLink + QPI ---------------------------------------------------
    nvlink: list[tuple[str, str]] = []
    if m.nvlink and m.nvlink.speed_gbps is not None:
        if compact:
            nvlink.append((f"{m.nvlink.speed_gbps:.1f}Go/s", TEXT))
            nvlink.append((f"{m.nvlink.links}lien", DIM))
        else:
            nvlink.append((f"{m.nvlink.speed_gbps:.3f} Go/s", TEXT))
            nvlink.append((f"{m.nvlink.links} lien actif", DIM))
        if m.nvlink.errors:
            nvlink.append(
                (
                    f"{m.nvlink.errors}err"
                    if compact
                    else f"{m.nvlink.errors} erreur(s)",
                    RED,
                )
            )
        else:
            nvlink.append(("0err" if compact else "0 erreur", DIM))
        if m.nvlink.throughput is not None:
            nvlink.append(
                (
                    f"{m.nvlink.throughput:.1f}Go/s"
                    if compact
                    else f"{m.nvlink.throughput:.1f} Go/s",
                    TEXT,
                )
            )
        else:
            nvlink.append(("n/a" if compact else "débit n/a", DIM))
    else:
        nvlink.append(
            ("aucun lien actif" if compact else "aucun lien NVLink actif", DIM)
        )

    qpi: list[tuple[str, str]] = []
    if m.qpi:
        if compact:
            qpi.append((f"{m.qpi.sockets}s", TEXT))
            if m.qpi.links:
                qpi.append((f"{m.qpi.links}liens", TEXT))
            if m.qpi.speed_gt_s is not None:
                qpi.append((f"{m.qpi.speed_gt_s:.1f}GT/s", TEXT))
            if m.qpi.traffic is not None:
                qpi.append((f"{m.qpi.traffic:.1f}Go/s", TEXT))
            else:
                qpi.append(("n/a", DIM))
        else:
            qpi.append((f"{m.qpi.sockets} sockets", TEXT))
            if m.qpi.links:
                qpi.append((f"{m.qpi.links} liens", TEXT))
            if m.qpi.speed_gt_s is not None:
                qpi.append((f"{m.qpi.speed_gt_s:.1f} GT/s", TEXT))
            if m.qpi.traffic is not None:
                qpi.append((f"{m.qpi.traffic:.1f} Go/s", TEXT))
            else:
                qpi.append(
                    (
                        f"n/a ({m.qpi.reason})" if m.qpi.reason else "n/a",
                        DIM,
                    )
                )
    else:
        qpi.append(("n/a", DIM))

    row = Text()
    row.append("NVLINK ", style=f"bold {GPU_C}")
    add_parts(row, nvlink)
    row.append("  ", style=DIM_SOFT)
    row.append("QPI ", style=f"bold {CPU_C}")
    add_parts(row, qpi)
    push(row)

    # --- Tensor + AMD ---------------------------------------------------
    tensor_gpus = [gpu for gpu in m.gpus if gpu.tensor is not None]
    if tensor_gpus:
        tensor = [
            (f"GPU{gpu.index} {gpu.tensor:.0f}%", TEXT) for gpu in tensor_gpus
        ]
    elif compact:
        tensor = [("n/a", DIM)]
    else:
        tensor = [("n/a (GPM non supporté, DCGM absent)", DIM)]

    amd_parts: list[tuple[str, str]] = []
    if m.amd:
        if wide_amd:
            amd_parts.append((m.amd.name, TEXT))
        if m.amd.temp is not None:
            amd_parts.append((f"{m.amd.temp:.0f}°C", TEXT))
        if compact:
            amd_parts.append(
                (
                    f"{fmt_gio(m.amd.vram_used)}/{fmt_gio(m.amd.vram_total)}G",
                    TEXT,
                )
            )
        else:
            amd_parts.append(
                (
                    f"{fmt_gio(m.amd.vram_used)}/{fmt_gio(m.amd.vram_total)} Gio",
                    TEXT,
                )
            )
        if m.amd.sclk_mhz is not None:
            clocks = f"{m.amd.sclk_mhz:.0f}"
            if m.amd.mclk_mhz is not None:
                clocks += f"/{m.amd.mclk_mhz:.0f}"
            amd_parts.append(
                (f"{clocks}MHz" if compact else f"{clocks} MHz", TEXT)
            )
        if m.amd.fan_pct is not None:
            amd_parts.append(
                (
                    f"{m.amd.fan_pct:.0f}%"
                    if compact
                    else f"vent. {m.amd.fan_pct:.0f}%",
                    DIM,
                )
            )
        if wide_amd and m.amd.pstate:
            amd_parts.append((m.amd.pstate, DIM))
        if m.amd.busy is not None:
            amd_parts.append(
                (
                    f"act.{m.amd.busy:.0f}%"
                    if compact
                    else f"activité {m.amd.busy:.0f}%",
                    TEXT,
                )
            )
        else:
            amd_parts.append(
                ("act.n/a" if compact else "activité n/a", DIM)
            )
    else:
        amd_parts.append(("aucune carte AMD détectée", DIM))

    row = Text()
    row.append("TENSOR ", style=f"bold {GPU_C}")
    add_parts(row, tensor)
    row.append("  ", style=DIM_SOFT)
    row.append("AMD ", style=f"bold {ORANGE}")
    add_parts(row, amd_parts)
    push(row)

    title = Text("LIAISONS", style=f"bold {TITLE}")
    if compact:
        right = Text("PCIe log", style=DIM)
    else:
        right = Text("PCIe log · ", style=DIM)
        right.append(top_glyph, style=f"bold {TITLE}")
        right.append("TX ", style=DIM)
        right.append(bottom_glyph, style=f"bold {GREEN}")
        right.append("RX", style=DIM)
    return card(width, 7, title, right, rows)


def footer_block(
    width: int,
    m: Metrics,
    interval: float,
    samples: int,
    gpu_ms: float,
    paused: bool,
) -> Text:
    left = Text()
    if paused:
        left.append("PAUSE", style=f"bold {ORANGE}")
        left.append("   ", style=DIM)
    left.append("q", style=f"bold {TEXT}")
    left.append(" quitter", style=DIM)
    left.append("   ", style=DIM)
    left.append("p", style=f"bold {TEXT}")
    left.append(" pause", style=DIM)
    left.append("   ", style=DIM)
    left.append("r", style=f"bold {TEXT}")
    left.append(" reset", style=DIM)

    if m.gpu_error:
        message = m.gpu_error
        if not message.lower().startswith("nvidia-smi"):
            message = "nvidia-smi : " + message
        right = Text(fit(message, max(10, width // 2)), style=ORANGE)
    else:
        right = Text(
            f"intervalle {interval:.1f} s  ·  nvidia-smi {gpu_ms:.0f} ms  ·  {samples} éch.",
            style=DIM,
        )
    gap = width - cell_len(left.plain) - cell_len(right.plain)
    if gap <= 0 and not m.gpu_error:
        right = Text(f"{interval:.1f} s · {gpu_ms:.0f} ms", style=DIM)
        gap = width - cell_len(left.plain) - cell_len(right.plain)
    line = Text()
    line.append_text(left)
    if gap > 0:
        line.append(" " * gap)
        line.append_text(right)
    return pad_to(line, width)


def too_small(width: int, height: int) -> list[Text]:
    title = Text("hauru-top", style=f"bold {TITLE}")
    body = [
        Text(
            f"terminal trop petit : {width}×{height} — minimum {MIN_WIDTH}×{MIN_HEIGHT}",
            style=ORANGE,
        ),
        Text("agrandir la fenêtre (ou --width / --height)", style=DIM),
        Text("q pour quitter", style=DIM),
    ]
    card_w = min(width, 64)
    card_h = min(max(5, height), 7)
    lines = card(card_w, card_h, title, None, body[: card_h - 2])
    left = max(0, (width - card_w) // 2)
    lines = [Text(" " * left) + line for line in lines]
    top = max(0, (height - len(lines)) // 2)
    return [Text(" " * width)] * top + lines


# ---------------------------------------------------------------------------
# Assemblage de la frame
# ---------------------------------------------------------------------------

MIN_WIDTH = 76
MIN_HEIGHT = 22


def build_frame(
    sampler: Sampler,
    width: int,
    height: int,
    paused: bool = False,
) -> Text:
    if width < MIN_WIDTH or height < MIN_HEIGHT:
        lines = too_small(width, height)
    else:
        m = sampler.metrics
        assert m is not None
        clock = datetime.now().strftime("%H:%M:%S")
        gpu_h = 6 if height >= 24 else 5

        # Blocs : en-tête, CPU, GPU, historique éventuel, pied de page.
        blocks: list[list[Text]] = []
        gpu_error = m.gpu_error is not None
        dot = ORANGE if (gpu_error or not m.gpus) else GREEN
        blocks.append(header_block(m, width, clock, dot))

        half_left = (width - 2) // 2
        half_right = width - 2 - half_left
        if len(sampler.topology.packages) > 1:
            cpu_left = cpu_card(
                sampler.topology.packages[0], m, sampler.topology, half_left
            )
            cpu_right = cpu_card(
                sampler.topology.packages[1], m, sampler.topology, half_right
            )
            blocks.append(side_by_side(cpu_left, cpu_right, 2))
        else:
            blocks.append(
                cpu_card(sampler.topology.packages[0], m, sampler.topology, width)
            )

        if len(m.gpus) >= 2:
            left = gpu_card(m.gpus[0], half_left, gpu_h)
            right = gpu_card(m.gpus[1], half_right, gpu_h)
            blocks.append(side_by_side(left, right, 2))
        elif len(m.gpus) == 1:
            blocks.append(gpu_card(m.gpus[0], width, gpu_h))
        else:
            message = m.gpu_error or "aucun GPU NVIDIA interrogé"
            blocks.append(gpu_placeholder(width, gpu_h, message))

        # LIAISONS : panneau de cinq lignes, dès que la hauteur le permet.
        # Il prend la place de l'historique sur les écrans courts ; l'historique
        # revient (réduit) à partir de 38 lignes.
        links_height = 7 if height >= 33 else 0
        if links_height:
            blocks.append(links_block(m, width))

        # Hauteur restante pour l'historique. Avec le panneau LIAISONS il y a
        # cinq lignes d'espacement, quatre sans lui.
        base_height = sum(len(block) for block in blocks) + 1  # + pied de page
        reserved_gaps = 5 if links_height else 4
        max_history = height - base_height - reserved_gaps
        if max_history >= 8:
            hist_rows = 6
        elif max_history >= 7:
            hist_rows = 5
        elif max_history >= 6:
            hist_rows = 4
        elif max_history >= 5:
            hist_rows = 3
        elif max_history >= 4:
            hist_rows = 2
        elif max_history >= 3:
            hist_rows = 1
        else:
            hist_rows = 0
        if hist_rows:
            blocks.append(
                history_block(m, sampler.history, width, hist_rows, sampler.interval)
            )

        total = sum(len(block) for block in blocks) + 1
        available_gaps = height - total
        wanted_gaps = len(blocks)
        gaps = min(wanted_gaps, max(0, available_gaps))

        lines: list[Text] = []
        for index, block in enumerate(blocks):
            if index > 0 and gaps > 0:
                lines.append(Text(" " * width))
                gaps -= 1
            lines.extend(block)
        if gaps > 0:
            lines.append(Text(" " * width))
        # Pied de page : ancré en bas quand l'écart reste raisonnable, sinon
        # collé au contenu (un grand vide au-dessus d'une barre d'état serait
        # plus étrange qu'un vide sous elle).
        room = height - len(lines) - 1
        if 0 <= room <= 8:
            while len(lines) < height - 1:
                lines.append(Text(" " * width))
        lines.append(
            footer_block(
                width,
                m,
                sampler.interval,
                sampler.samples,
                sampler.gpu_latency_ms,
                paused,
            )
        )

    # Toutes les lignes font exactement `width` pour éviter les résidus de
    # l'image précédente ; on complète jusqu'à `height`.
    final: list[Text] = []
    for index in range(height):
        if index < len(lines):
            final.append(pad_to(lines[index], width))
        else:
            final.append(Text(" " * width))
    return Text("\n").join(final)


# ---------------------------------------------------------------------------
# Boucle interactive
# ---------------------------------------------------------------------------


def poll_key(timeout: float) -> Optional[str]:
    """Lit une touche sans bloquer ; None si le délai expire."""
    if not sys.stdin.isatty():
        time.sleep(max(0.0, timeout))
        return None
    try:
        ready, _, _ = select.select([sys.stdin], [], [], max(0.0, timeout))
    except (OSError, ValueError):
        time.sleep(max(0.0, timeout))
        return None
    if not ready:
        return None
    try:
        data = os.read(sys.stdin.fileno(), 1)
    except OSError:
        return None
    if not data:
        return None
    return data.decode("utf-8", errors="ignore")


def run_live(sampler: Sampler, console: Console) -> int:
    import termios
    import tty

    interactive = sys.stdin.isatty()
    old_attrs = None
    fd = sys.stdin.fileno() if interactive else None
    if interactive and fd is not None:
        old_attrs = termios.tcgetattr(fd)
        tty.setcbreak(fd)

    stop = False

    def _stop(_signum: int, _frame: object) -> None:
        nonlocal stop
        stop = True

    previous_term = signal.signal(signal.SIGTERM, _stop)
    paused = False

    try:
        sampler.prime()
        frame = build_frame(sampler, console.size.width, console.size.height, paused)
        with Live(
            frame,
            console=console,
            screen=True,
            auto_refresh=False,
            vertical_overflow="crop",
        ) as live:
            next_tick = time.monotonic()
            while not stop:
                size = console.size
                live.update(
                    build_frame(sampler, size.width, size.height, paused),
                    refresh=True,
                )
                next_tick += sampler.interval
                while not stop:
                    remaining = next_tick - time.monotonic()
                    if remaining <= 0:
                        break
                    key = poll_key(min(0.2, remaining))
                    if key in ("q", "Q"):
                        return 0
                    if key in ("p", "P", " "):
                        paused = not paused
                        if not paused:
                            # Évite de moyenner toute la durée de la pause.
                            sampler.reset_cpu_baseline()
                        live.update(
                            build_frame(
                                sampler, size.width, size.height, paused
                            ),
                            refresh=True,
                        )
                        next_tick = time.monotonic() + sampler.interval
                    elif key in ("r", "R"):
                        sampler.history.clear()
                        sampler.samples = 0
                        live.update(
                            build_frame(
                                sampler, size.width, size.height, paused
                            ),
                            refresh=True,
                        )
                if stop:
                    break
                if not paused:
                    sampler.sample()
                now = time.monotonic()
                if now - next_tick > sampler.interval:
                    next_tick = now
    except KeyboardInterrupt:
        return 0
    finally:
        signal.signal(signal.SIGTERM, previous_term)
        if interactive and fd is not None and old_attrs is not None:
            try:
                termios.tcsetattr(fd, termios.TCSADRAIN, old_attrs)
            except termios.error:
                pass
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog=APP,
        description="Tableau de bord terminal : cœurs physiques (HT), GPU, RAM/VRAM.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "exemples :\n"
            "  hauru-top.py                       # 1 Hz, plein écran, quitter avec q\n"
            "  hauru-top.py -i 0.5                # rafraîchissement deux fois par seconde\n"
            "  hauru-top.py --once --width 140 --height 40 > vue.txt\n"
        ),
    )
    parser.add_argument(
        "-i", "--interval", type=float, default=1.0,
        help="période d'échantillonnage en secondes (défaut : 1.0)",
    )
    parser.add_argument(
        "--once", action="store_true",
        help="n'affiche qu'une seule image puis quitte (sans écran alterné)",
    )
    parser.add_argument(
        "--width", type=int, default=None,
        help="largeur forcée (utile pour --once ou les tests)",
    )
    parser.add_argument(
        "--height", type=int, default=None,
        help="hauteur forcée (utile pour --once ou les tests)",
    )
    parser.add_argument(
        "--color", choices=("auto", "always", "never"), default="auto",
        help="couleurs ANSI : auto (défaut), always, never",
    )
    parser.add_argument(
        "--ascii", action="store_true",
        help="bordures, barres et sparklines en ASCII (texte accentué conservé)",
    )
    parser.add_argument(
        "--version", action="version", version=f"{APP} {VERSION}",
    )
    args = parser.parse_args(argv)
    if not 0.2 <= args.interval <= 30.0:
        parser.error("--interval doit être compris entre 0.2 et 30 secondes")
    if args.width is not None and args.width < 20:
        parser.error("--width doit valoir au moins 20")
    if args.height is not None and args.height < 5:
        parser.error("--height doit valoir au moins 5")
    return args


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)

    encoding = (sys.stdout.encoding or "").lower()
    unicode_ok = (not args.ascii) and ("utf" in encoding)
    if not unicode_ok:
        # Évite un UnicodeEncodeError sur un terminal/locale non UTF-8 :
        # les accents seront remplacés, les bordures passent en ASCII.
        try:
            sys.stdout.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    configure_glyphs(unicode_ok)

    if not args.once and not sys.stdout.isatty():
        print(
            f"{APP}: la sortie n'est pas un terminal ; utiliser --once pour une image.",
            file=sys.stderr,
        )
        return 2

    # Attention : dans rich, color_system=None désactive la couleur ; c'est
    # la chaîne « auto » qui demande la détection du terminal.
    if args.color == "always":
        force_terminal: Optional[bool] = True
        color_system: Optional[str] = "truecolor"
        no_color: Optional[bool] = False
    elif args.color == "never":
        force_terminal = False
        color_system = None
        no_color = True
    else:
        force_terminal = None
        color_system = "auto"
        no_color = None

    console = Console(
        force_terminal=force_terminal,
        color_system=color_system,
        no_color=no_color,
        width=args.width,
        height=args.height,
        highlight=False,
        soft_wrap=False,
    )

    sampler = Sampler(args.interval)

    if args.once:
        sampler.prime()
        size = console.size
        frame = build_frame(sampler, size.width, size.height, paused=False)
        console.print(frame)
        if sampler.metrics and sampler.metrics.gpu_error:
            print(f"{APP}: {sampler.metrics.gpu_error}", file=sys.stderr)
        return 0

    return run_live(sampler, console)


if __name__ == "__main__":
    sys.exit(main())
