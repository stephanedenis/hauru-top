# hauru-top — tableau de bord terminal

**Version :** 1.2.1 — 21 octobre 2026
**Implémentations :** Python 3.10+ ([`hauru-top.py`](hauru-top.py)) et
Rust ([`hauru-top-rs/`](hauru-top-rs/), binaire natif sans Python)
**Dépôt :** https://github.com/stephanedenis/hauru-top
**Dépendances :** `rich` pour Python ; `cargo` pour Rust.
Aucun privilège root n'est nécessaire.

> `hauru-top` lit uniquement `/proc`, `/sys`, `nvidia-smi` et NVML et ne modifie
> rien sur le système. Le terminal est restauré à la sortie, y compris sur
> `Ctrl+C`, SIGTERM et SIGHUP.

## Installation

```bash
git clone https://github.com/stephanedenis/hauru-top.git
cd hauru-top
python3 -m pip install --user -r requirements.txt   # version Python
(cd hauru-top-rs && cargo build --release)          # version Rust (optionnelle)
```

## But

Afficher d'un seul coup d'œil, en couleur et rafraîchi chaque seconde :

- la charge des **deux sockets** Xeon, **cœur physique par cœur physique**, avec
  les deux threads hyperthread **empilés dans une même barre** (moitié haute /
  moitié basse), pour un groupement visuel de chaque cœur ;
- la charge, la VRAM, la puissance, la température, le ventilateur et les
  fréquences des **deux GPU** NVIDIA ;
- les métriques globales : CPU total, RAM, swap, VRAM cumulée, load average,
  uptime, fréquences et températures par socket ;
- un historique sparkline (CPU global ou par socket, GPU, RAM, VRAM) quand la
  fenêtre est assez haute ;
- un panneau **LIAISONS** : barres PCIe TX/RX par lien (NVML), état/vitesse/
  erreurs NVLink, topologie QPI, activité tensor et capteurs de la carte AMD.

```
╭─ ● HAURU ─────────────────────────────────────────────── 13:32:51  ·  charge 6.30  ·  up 7j 03h ─╮
│ CPU  ██▍░░░░░░░░░░░░░░░░░  12%   RAM  ████████▎░░░░░░░░░░░  42%   VRAM ░░░░░░░░░░░░░░░░░░░░   0% │
│ 40 thr · 1.53 GHz · 38/46°C      52.6/125.8 Gio · swap 16.9       0.0/32.0 Gio                   │
╰──────────────────────────────────────────────────────────────────────────────────────────────────╯

╭─ CPU 0 · Xeon E5-2687W v3 ──────  10% · 38°C ─╮  ╭─ CPU 1 · Xeon E5-2687W v3 ──────  14% · 46°C ─╮
│ 00 00/20 ░░░░░░░░░░░░░░░░░░░░░░░░ ▀  2% ▄  0% │  │ 00 10/30 ░░░░░░░░░░░░░░░░░░░░░░░░ ▀  2% ▄  0% │
│ 01 01/21 ▀▀░░░░░░░░░░░░░░░░░░░░░░ ▀ 10% ▄  0% │  │ 01 11/31 ▄▄▄░░░░░░░░░░░░░░░░░░░░░ ▀  0% ▄ 12% │
│ 02 02/22 ▀▀▀▀▀▀▀▀▀░░░░░░░░░░░░░░░ ▀ 38% ▄  0% │  │ 02 12/32 ▀░░░░░░░░░░░░░░░░░░░░░░░ ▀  4% ▄  2% │
```

> Les deux threads partagent chaque barre : la moitié haute est le premier
> thread, la moitié basse le second. En couleur, les deux moitiés sont teintées
> séparément ; en texte brut, la couleur de fond disparaît et la moitié basse
> n'est plus visible. Les repères `▀` / `▄` à droite donnent les deux
> pourcentages.

## Lancement

Depuis la racine du dépôt :

```bash
./hauru-top.py                    # Python, 1 Hz, plein écran, q pour quitter
./hauru-top.py -i 0.5             # deux rafraîchissements par seconde
./hauru-top.py --once             # une image, puis sortie
./hauru-top.py --once --width 140 --height 40 > vue.txt

./hauru-top-rust                  # Rust, compilation initiale automatique
(cd hauru-top-rs && cargo build --release)
./hauru-top-rs/target/release/hauru-top --once --width 140 --height 40
```

Les deux implémentations acceptent les mêmes options et les mêmes touches.
Le détail du portage Rust est dans [`hauru-top-rs/README.md`](hauru-top-rs/README.md).

| Touche | Effet |
|---|---|
| `q` | quitter (le terminal, le curseur et le mode clavier sont restaurés) |
| `p` ou `Espace` | figer/reprendre les mesures (l'horloge continue) |
| `r` | vider l'historique et remettre le compteur d'échantillons à zéro |
| `Ctrl+C` | quitter proprement |

Options : `--interval/-i`, `--once`, `--width`, `--height`,
`--color {auto,always,never}`, `--ascii`, `--version`.

## Ce qui est mesuré et d'où ça vient

| Donnée | Source |
|---|---|
| Charge par thread | delta de `/proc/stat` (busy = total − idle − iowait) |
| Saturation d'un thread | thread ≥ 99,5 % (affiché 100 %) → alerte rouge dans l'historique |
| Cœurs physiques / frères SMT | `/sys/devices/system/cpu/cpu*/topology/` (`thread_siblings_list`, `physical_package_id`, `core_id`) |
| Fréquences | `/sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq` |
| Températures socket | `hwmon/coretemp`, entrées `Package id N` |
| RAM / swap | `/proc/meminfo` (`MemTotal − MemAvailable`, comme `free`) |
| Load average / uptime | `os.getloadavg()` / `/proc/uptime` |
| GPU | `nvidia-smi --query-gpu=…` (appel borné à 2,5 s) |
| PCIe | `pcie.link.gen.current/max`, largeur, débit TX/RX via NVML (`nvmlDeviceGetPcieThroughput`) ; barres log TX/RX ; lien AMD via sysfs |
| NVLink | `nvidia-smi nvlink -s` / `-e` (vitesse, état, compteurs d'erreurs) |
| QPI | topologie et caractéristiques du CPU (E5-2687W v3 : 2 liens à 9,6 GT/s) |
| Tensor | NVML GPM si le GPU le supporte, sinon `n/a` |
| AMD | sysfs `amdgpu` : VRAM, température, horloges, ventilateur, état DPM, lien PCIe |

Les GPU sont découverts dynamiquement : deux RTX 5000 sur Hauru, reliées par un
lien NVLink. La carte AMD (Radeon HD 7750) est suivie pour ses capteurs
disponibles ; son **activité** n'est pas exposée par ce noyau
(`gpu_busy_percent` et `mem_busy_percent` renvoient « Operation not supported »).

## Rendu et adaptation à la fenêtre

- **Largeur minimale : 76 colonnes ; hauteur minimale : 22 lignes.** En dessous,
  un message centré indique la taille requise (et `q` reste actif).
- Les deux sockets sont toujours côte à côte. Chaque cœur physique tient sur
  une seule ligne : une barre unique dont la moitié haute suit le premier
  thread et la moitié basse le second. Les repères `▀` / `▄` à droite rappellent
  l'ordre et donnent les deux pourcentages (12 cellules de barre à 76 colonnes,
  34 à 120, 74 à 200).
- Le panneau **LIAISONS** (5 lignes + bordure, dont 3 barres PCIe) apparaît à
  partir de 33 lignes. Il prend la place de l'historique sur les écrans courts ;
  l'historique revient réduit à 38 lignes (1 série), 39 (2), 40 (3), et complet
  (6 séries) à 43. Sans le panneau, l'historique apparaît dès 30 lignes.
- Couleurs 24 bits si le terminal les annonce, sinon rich quantifie en 256 ou
  16 couleurs. La palette est prévue pour un fond sombre. `NO_COLOR` est
  respecté ; `--color always` force la couleur.
- `--ascii` remplace bordures, barres et sparklines par des caractères ASCII ;
  les demi-blocs appariés deviennent `^` / `v` (et `#` quand les deux threads
  remplissent la cellule).

## Codes couleur

- Barres et pourcentages : dégradé froid → chaud (bleu, vert, jaune, orange,
  rouge) selon la charge. Les valeurs < 4 % sont grisées pour laisser respirer
  les cœurs au repos.
- **Alerte 100 %** : dans l'historique CPU, chaque échantillon où au moins un
  thread atteint 100 % est tracé en rouge, et un `!` rouge suit la valeur
  courante. La courbe reste une moyenne (globale, ou par socket quand six
  séries sont affichées) : le rouge signale la saturation d'un thread, pas la
  moyenne.
- Températures : vert en dessous de 60 °C, jaune en dessous de 72 °C, orange
  en dessous de 84 °C, rouge au-delà.
- Le point devant `HAURU` est vert quand tout répond, orange si `nvidia-smi`
  échoue ou si aucun GPU n'est détecté.

## LIAISONS : PCIe, QPI, NVLink, tensor, AMD

À partir de 33 lignes, un panneau de cinq lignes résume les interconnexions
et la carte AMD. Trois lignes sont consacrées aux barres PCIe, les deux
dernières regroupent NVLink/QPI et tensor/AMD :

```
╭─ LIAISONS ────────────────────────────────────────────────────────────────────────────────────── PCIe log · ▀TX ▄RX ─╮
│ PCIE   GPU 0 Gen3 x16 ▀▀▀▀▀░░░░░░░░░░░░░░░░░░░░░░░░░ ▀4.1 ▄0.3 Mo/s                                                  │
│ PCIE   GPU 1 Gen3 x16 ▀▀▀▀▀▀▀▄░░░░░░░░░░░░░░░░░░░░░░ ▀9.4 ▄10.4 Mo/s                                                 │
│ PCIE   AMD Gen3 x16 ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ débit n/a                                                         │
│ NVLINK 25.781 Go/s · 1 lien actif · 0 erreur · débit n/a  QPI 2 sockets · 2 liens · 9.6 GT/s · n/a (perf absent)     │
│ TENSOR n/a (GPM non supporté, DCGM absent)  AMD 66°C · 0.2/2.0 Gio · 300/1000 MHz · vent. 47% · activité n/a         │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
```

Les trois liens PCIe ont chacun leur barre : **TX en haut (bleu), RX en bas
(vert)**, avec la légende `▀TX ▄RX` dans le titre. L'échelle est
**logarithmique de 1 Mo/s à 10 Go/s** (indiquée `PCIe log`) : un lien x16 Gen3
peut transporter 15,75 Go/s, donc une barre linéaire en pourcentage resterait
vide avec le trafic courant (0,5 à 80 Mo/s). Les débits exacts sont affichés à
droite de chaque barre. La barre AMD reste vide, faute de compteur de débit.

| Ligne | Ce qui est affiché | Limite |
|---|---|---|
| `PCIE` (×3) | une barre par lien (GPU 0, GPU 1, AMD), génération courante/max, largeur et débit TX/RX (NVML) | le débit AMD n'est pas exposé → barre vide + `débit n/a` |
| `NVLINK` + `QPI` | vitesse, lien actif, erreurs ; sockets, liens, 9,6 GT/s | débits non exposés (compteurs Turing, `perf` absent) |
| `TENSOR` + `AMD` | activité tensor via GPM ; température, VRAM, horloges, ventilateur, DPM | tensor `n/a` (GPM Turing/DCGM) ; activité AMD `n/a` |

Le lien PCIe des RTX 5000 **descend en Gen1 au repos** et remonte en Gen3 sous
charge : le panneau affiche `Gen1/3` pour rendre ce comportement lisible.

Pour obtenir davantage :

- **Tensor** : nécessite un GPU Ampere+ avec GPM, ou DCGM/`dcgmi` (absent sur
  cette machine) ; la jauge `UTIL` des cartes GPU reste l'utilisation SM.
- **QPI** : installer `perf`, puis lancer avec `CAP_PERFMON` ou en root pour
  lire les compteurs uncore ; l'application n'implémente pas encore ce mode.
- **NVLink** : les compteurs de débit (`nvidia-smi nvlink -gt d`) répondent
  `N/A` sur ce pilote ; vitesse, état et erreurs restent fiables.
- **AMD** : `amdgpu` n'expose ni `gpu_busy_percent` ni `mem_busy_percent` sur
  Cape Verde ; les capteurs hwmon (température, horloges, PWM) fonctionnent.

## Version Rust

Le portage Rust vit dans [`hauru-top-rs/`](hauru-top-rs/) et se lance avec
`./hauru-top-rust` (compilation initiale automatique) ou directement après
`cargo build --release` dans `hauru-top-rs/`.

Il reprend l'intégralité du contrat d'affichage (version 1.2.1) : barres
appariées en demi-blocs, alertes rouges à 100 %, panneau LIAISONS avec barres
PCIe TX/RX, historique, adaptation 76×22, modes de couleur et `--ascii`. Il ne dépend ni de Python ni de
`rich` ; `nvidia-smi`, NVML (via `libloading`), `/proc` et `/sys` restent les
sources de données. Le binaire produit est spécifique à Linux, dans
`hauru-top-rs/target/release/hauru-top` (répertoire `target/` ignoré par git).

## Limites connues

- `nvidia-smi` est relancé à chaque tick (~30-70 ms) : en cas de blocage du
  pilote, l'appel est coupé au bout de 2,5 s et l'erreur s'affiche dans le pied
  de page ; le tableau de bord continue.
- L'historique est en mémoire (600 points, ~10 min à 1 Hz) et repart à zéro à
  chaque lancement.
- Les GPU sont indexés par leur numéro `nvidia-smi` ; un changement d'ordre au
  boot déplacerait les libellés (GPU 0 / GPU 1), pas les mesures.
- **PCIe / NVLink** : les débits sont lus par NVML et `nvidia-smi` ; les
  barres PCIe sont **logarithmiques** (1 Mo/s–10 Go/s), pas un pourcentage de
  capacité — les débits exacts restent affichés. Les compteurs NVLink de Turing
  ne fournissent pas de débit (`N/A`), et le lien PCIe peut être en Gen1 au
  repos (affiché `Gen1/3`).
- **QPI et tensor** : le trafic QPI exige `perf` + `CAP_PERFMON`/root, et
  l'activité tensor exige GPM (Ampere+) ou DCGM — aucun des deux n'est
  disponible ici ; le panneau l'indique explicitement.
- **AMD** : seule l'activité manque (`gpu_busy_percent` non supporté) ; la
  température, la VRAM, les horloges et le ventilateur sont réels.
- Pas de mode « enregistrement » : pour une trace, utiliser
  `--once` dans une boucle ou `nvidia-smi dmon`.

## Vérifications effectuées

- `--once` comparé à `free -b` et `nvidia-smi` : RAM, swap, VRAM et charge GPU
  concordent (aux variations d'une seconde près).
- Testé sur 88 combinaisons de tailles, de 76×22 à 200×60, pour les deux
  versions ; chaque ligne fait exactement la largeur demandée, y compris les
  barres appariées et les barres PCIe TX/RX du panneau LIAISONS.
- Données de liaisons vérifiées en direct : débits PCIe TX/RX (NVML), NVLink
  25,781 Go/s et 0 erreur, AMD 66 °C / 0,2 sur 2,0 Gio / 300-1000 MHz /
  ventilateur 47 %.
- Barres PCIe comparées en 76×34, 100×33, 120×34 et 140×40 : mêmes positions de
  bordures, même légende `PCIe log · ▀TX ▄RX`, rendu ASCII `^TX vRX`.
- Testé sous PTY : pause, reset, redimensionnement, sortie `q` et `Ctrl+C` ;
  l'écran alterné et le curseur sont restaurés, aucune trace d'exception.
- Portage Rust : 88 tailles, positions de bordures identiques à la version
  Python en 76×34, 100×33, 120×34 et 140×40 ; pause, resize, `q`, `Ctrl+C`,
  SIGTERM et panne `nvidia-smi` vérifiés ; mêmes données de liaisons (NVML via
  `libloading`, NVLink/QPI/AMD).
- Coût applicatif mesuré : négligeable (< 0,2 % d'un cœur hors `nvidia-smi`).

## Licence

MIT — voir [LICENSE](LICENSE). Copyright (c) 2026 Stéphane Denis.

L’utilisation, la modification et la redistribution sont libres, y compris
commercialement, à condition de conserver la mention de copyright et le texte
de la licence. Le logiciel est fourni sans garantie.
