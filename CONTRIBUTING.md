# Contribuer à Acrux

Merci de vouloir aider. Ce projet est conçu pour que n'importe qui puisse y contribuer,
même sans expérience du format PDF. Ce guide explique tout ce qu'il faut savoir.

## 1. Installation en trois commandes

1. Installer Rust stable : https://rustup.rs (Windows : `rustup-init.exe` ; macOS/Linux : la commande curl du site).
   - **Windows** : le toolchain MSVC a besoin de l'éditeur de liens Microsoft. Installer
     les « Visual Studio Build Tools » avec la charge de travail « Développement Desktop en C++ » :
     `winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`.
   - **Linux** : un compilateur C de base (`build-essential` ou équivalent) pour l'éditeur de liens.
   - **macOS** : `xcode-select --install`.
2. Cloner le dépôt puis, à sa racine :

```bash
cargo build
```

```bash
cargo test --workspace
```

C'est tout. Aucune dépendance système, aucun outil supplémentaire.

## 2. Les règles en une minute

- **Jamais de bibliothèque PDF ni de code copié d'un autre lecteur PDF.** On implémente à
  partir des spécifications (ISO 32000-2, TrueType, JPEG…). Une PR qui ajoute une dépendance
  PDF, un moteur de rendu ou un rasteriseur externe est refusée.
- **Toute dépendance externe est justifiée** dans `docs/DEPENDANCES.md` avant d'être ajoutée.
- **Pas de `unsafe`.** Le workspace l'interdit (`unsafe_code = "forbid"`). Si un cas semble
  l'exiger, ouvrir une issue d'abord.
- **Jamais de panique sur un fichier utilisateur.** Un PDF corrompu produit une erreur typée
  (`acrux_core::Error`), pas un `unwrap()`. Clippy avertit sur `unwrap`, `expect` et `panic`.
- **Une fonctionnalité = ses tests + ses fichiers de test.** Voir §4.
- **Le code est documenté.** Chaque élément public a un commentaire `///` ; les commentaires
  citent la section de spécification concernée, par exemple `// ISO 32000-2 §7.5.4`.
- **Lisible avant malin.** Fonctions courtes, noms explicites, pas d'astuce non commentée.

## 3. Avant d'ouvrir une pull request

```bash
cargo fmt --all
```

```bash
cargo clippy --workspace --all-targets
```

```bash
cargo test --workspace
```

Les trois doivent passer sans avertissement. L'intégration continue les relance.

## 4. Ajouter un test

- **Test unitaire** : dans le fichier concerné, bloc `#[cfg(test)] mod tests`.
- **PDF de test** : déposer le fichier dans `tests/corpus/<fonctionnalité>/` avec un nom
  descriptif (`xref-stream-hybride.pdf`, `police-cff-cid.pdf`). Le fichier doit être libre de
  droits ou créé par vous. Les fichiers privés vont dans `tests/corpus/private/` (ignoré par git).
- **Test de rendu** : `cargo test -p acrux-render --test render_corpus` rend chaque PDF du corpus
  à 72 dpi et le compare à `tests/reference/<même chemin>-pNNN.png`. Pour un nouveau fichier,
  le test écrit le rendu dans `tests/reference/actual/` : **vérifiez-le à l'œil** (et, si
  possible, contre Acrobat) avant de le copier dans `tests/reference/`. En cas d'écart, une
  image des différences (`.diff.png`) est produite à côté.
- **PDF chiffré dans le corpus** : nommer le fichier `…-mdp-<mot de passe>.pdf` ; les tests
  d'intégration lisent le mot de passe dans le nom.
- **Générer un PDF de test** : soit à la main (voir `build_pdf` dans les tests de
  `acrux-document`), soit avec Chrome sans interface, ce qui donne des fichiers réalistes :
  ```bash
  chrome --headless=new --disable-gpu --no-pdf-header-footer --print-to-pdf=sortie.pdf page.html
  ```
  Déposer le `.html` à côté du PDF sous le nom `<pdf>.source.html`, et vérifier le rendu en
  comparant notre sortie à la capture que Chrome fait de la **même** page HTML
  (`chrome --headless=new --screenshot=page.png --window-size=900,1300 page.html`).
- **Mesure de performance** : `cargo test -p acrux-render --release --test bench_corpus -- --ignored --nocapture`
  affiche le temps de rendu par page du corpus (ordre de grandeur attendu : 20 ms pour une A4
  à 150 dpi, quelques centaines de millisecondes pour une page très chargée).
- **Fuzzing** : les parseurs et codecs ont une cible dans `tests/fuzz/`.

### Tester l'interface graphique

L'application se pilote depuis PowerShell et **sans rien afficher** : en mode invisible, la
fenêtre est créée mais jamais montrée, aucun dialogue système ne s'ouvre, et rien ne vient
interrompre la personne qui travaille sur la machine. C'est le mode à utiliser pour tout test.

| Variable | Effet |
|---|---|
| `ACRUX_HEADLESS=1` | la fenêtre n'est jamais affichée ; l'application peint dans son tampon et se pilote par messages |
| `ACRUX_HWND=<fichier>` | la fenêtre y écrit sa poignée au démarrage (une fenêtre cachée n'apparaît pas dans `MainWindowHandle`) |
| `ACRUX_LOG=<fichier>` | journalise chaque événement reçu, chaque peinture et chaque action |
| `ACRUX_SHOT=<fichier.ppm>` | écrit le tampon de la fenêtre à chaque peinture (PPM binaire, non compressé) |
| `ACRUX_THEME=light` | démarre en thème clair quelles que soient les préférences |
| `ACRUX_OPEN_FILE=<chemin>` | réponse du dialogue « Ouvrir » (`-` = annulé) |
| `ACRUX_SAVE_FILE=<chemin>` ou `ACRUX_SAVE_DIR=<dossier>` | réponse des dialogues « Enregistrer » et « Exporter » |
| `ACRUX_CONFIRM=oui\|non` | réponse des demandes de confirmation |
| `ACRUX_PRINTER=<nom>` | imprime sans dialogue (mettre `Microsoft Print to PDF` et `ACRUX_PRINT_OUTPUT`) |

On envoie les événements avec `PostMessage` sur la poignée publiée : `0x0100`/`0x0101` pour une
touche (code virtuel en `wParam`), `0x0102` pour un caractère, `0x0200`/`0x0201`/`0x0202` pour la
souris (`lParam = (y << 16) | x`, coordonnées client). `AppActivate` et `SetForegroundWindow` ne
conviennent pas : Windows refuse le vol de focus quand une autre application travaille, et le test
reste alors silencieusement sans effet.

Le fichier `scratchpad/headless.ps1` fournit le harnais tout fait (`Start-App`, `Key`, `Char`,
`Click`, `Shot`, `Journal`, `Stop-App`). Se fier au journal et à `ACRUX_SHOT` plutôt qu'à une capture
d'écran. Penser aussi à `$env:APPDATA` pointant vers un dossier temporaire pour ne pas écraser les
préférences de la machine, et à n'utiliser que de l'ASCII dans les scripts `.ps1` (PowerShell 5 les
lit en ANSI : les accents envoyés au clavier arriveraient déformés).

## 5. Ajouter une fonctionnalité

1. Vérifier dans `FONCTIONNALITES_ADOBE_ACROBAT.txt` et `ROADMAP.md` où elle se situe.
2. Ouvrir une issue (ou en prendre une existante) pour éviter le travail en double.
3. Lire la section de spécification concernée et en écrire un résumé court dans
   `spec-notes/` si elle n'y est pas encore.
4. Implémenter dans le bon crate (voir `ARCHITECTURE.md`), sans dépendance vers le haut.
5. Tests, doc, `ROADMAP.md` mis à jour (case cochée), PR.

## 6. Conventions

- Langue du code (identifiants) : anglais. Langue des commentaires et de la doc : français
  (les contributeurs non francophones sont bienvenus ; l'anglais est accepté dans les PR).
- Messages de commit : `crate: description courte à l'impératif` (`acrux-codecs: ajoute le décodeur LZW`).
- Une PR = un sujet. Les PR de plus de 800 lignes sont découpées.
- Formatage : `rustfmt` (config à la racine). Lints : `clippy pedantic`.

## 7. Par où commencer

Les issues marquées **`bon premier ticket`** sont des tâches délimitées et documentées.
Bonnes premières contributions typiques : un décodeur simple (`ASCIIHex`, `RunLength`),
un encodage de police, un PDF de test, une note de spécification.

## 8. Code de conduite

Respect, bienveillance, critiques sur le code et jamais sur les personnes.
