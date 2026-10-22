# hauru-top (Rust)

Portage Rust (version 1.2.1) du tableau de bord
[`../hauru-top.py`](../hauru-top.py), avec le même rendu et les mêmes options.
Objectif : un **binaire natif** (≈ 790 Ko), sans Python ni `rich`, pour lancer
le moniteur instantanément.

## Compilation et lancement

```bash
cd hauru-top-rs
cargo build --release
./target/release/hauru-top
```

Depuis la racine du dépôt, le lanceur compile la première fois si nécessaire :

```bash
./hauru-top-rust
```

Options et touches sont identiques à la version Python :

```bash
hauru-top -i 0.5
hauru-top --once --width 140 --height 40 > vue.txt
hauru-top --color always
hauru-top --ascii
```

`q` quitter · `p`/`Espace` pause · `r` reset de l'historique.

## Ce que le portage reprend

- en-tête CPU / RAM / VRAM, load average, uptime, fréquences et températures
  par socket ;
- deux cartes CPU (sockets) côte à côte, une ligne par cœur physique, les deux
  threads SMT **empilés** dans une barre unique (`▀` haut / `▄` bas) ;
- deux cartes GPU NVIDIA (UTIL, VRAM, PWR, CLK, température, ventilateur) ;
- panneau **LIAISONS** : barres PCIe TX/RX par lien (échelle log, NVML),
  NVLink (vitesse, état, erreurs), QPI (topologie), tensor (GPM si disponible)
  et carte AMD (température, VRAM, horloges, ventilateur, DPM) ;
- historique sparkline CPU/GPU/RAM/VRAM, avec **rouge et `!` dès qu'un thread
  atteint 100 %** (seuil 99,5 %) ;
- couleurs 24 bits / 256 / 16, respect de `NO_COLOR`, mode `--ascii` ;
- disposition adaptative : minimum 76×22, panneau LIAISONS dès 33 lignes,
  historique réduit à 38/39/40 lignes, six séries à 43 ;
- `nvidia-smi` borné à 2,5 s, erreur affichée sans interrompre l'affichage ;
- restauration du terminal sur `q`, `Ctrl+C`, SIGTERM et SIGHUP.

## Dépendances

Rust stable (testé avec 1.97), Linux uniquement (lecture de `/proc` et `/sys`,
`nvidia-smi`). Crates :

| Crate | Usage |
|---|---|
| `crossterm` | mode brut, taille du terminal, événements clavier/redimensionnement |
| `unicode-width` | largeur d'affichage des accents et des demi-blocs |
| `signal-hook` | sortie propre sur SIGTERM / SIGHUP |
| `libloading` | chargement de NVML pour le débit PCIe |
| `libc` | heure locale (`localtime_r`) |

Le fichier `Cargo.lock` est versionné pour des builds reproductibles ; `target/`
est ignoré.

## Tests effectués

- `cargo test` : largeur exacte des barres et sparklines, alignement des
  pourcentages, troncature, extrémités du dégradé, largeur des glyphes.
- **88 combinaisons de tailles** (76×22 → 200×60) : hauteur, largeur et pied de
  page conformes, panneau LIAISONS et historique adaptatif compris.
- Comparaison avec la version Python aux tailles 76×34, 100×33, 120×34 et
  140×40 : **positions des bordures identiques**.
- Tests sous PTY : pause, reset, redimensionnement, `q`, `Ctrl+C`, SIGTERM ;
  écran alterné et curseur restaurés, aucune panique.
- Panne de `nvidia-smi` (code 1) et délai dépassé (processus qui dort 5 s) :
  message dans la carte GPU et le pied de page, sortie en 3 s au total.
- Données de liaisons vérifiées en direct : débits PCIe TX/RX (NVML), NVLink
  25,781 Go/s et 0 erreur, AMD 66 °C / 0,2 sur 2,0 Gio / 300-1000 MHz /
  ventilateur 47 %.
- Barres PCIe comparées en 76×34, 100×33, 120×34 et 140×40 : même légende
  `PCIe log · ▀TX ▄RX`, rendu ASCII `^TX vRX`, bordures identiques.
- Modes couleur : vrai 24 bits, 256 couleurs, `NO_COLOR`, `--color always`.

## Notes

- La carte AMD (Radeon HD 7750) est suivie pour ses capteurs (température,
  VRAM, horloges, ventilateur, DPM) ; son activité n'est pas exposée par
  `gpu_busy_percent` sur ce noyau, comme dans la version Python.
- **QPI** : le débit exige `perf` et `CAP_PERFMON`/root ; seule la topologie
  (2 sockets, 2 liens, 9,6 GT/s) est affichée.
- **Tensor** : NVML GPM n'est pas supporté sur Turing et DCGM est absent ;
  la ligne affiche `n/a`.
- **NVLink** : vitesse, état et erreurs sont disponibles ; les compteurs de
  débit Turing répondent `N/A`.
- **Barres PCIe** : échelle logarithmique 1 Mo/s–10 Go/s, TX en haut, RX en
  bas ; ce n'est pas un pourcentage de la capacité (les débits exacts sont
  affichés à côté).
- Le binaire est propre à Linux x86_64 ; il n'embarque pas `nvidia-smi`.
- La version Python reste la référence ; les deux implémentations partagent le
  même contrat d'affichage et les mêmes options.
