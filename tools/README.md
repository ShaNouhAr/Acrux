# Outils de développement

## `headless.ps1` — piloter l'application sans rien afficher

Charge des fonctions PowerShell qui lancent `acrux.exe` en **mode invisible** : la fenêtre
est créée mais jamais montrée, aucun dialogue système ne s'ouvre, et rien n'interrompt la personne
qui travaille sur la machine. L'application peint dans son tampon (que l'on récupère avec
`ACRUX_SHOT`) et reçoit les événements par `PostMessage`.

```powershell
. .\tools\headless.ps1
Start-App "tests\corpus\reels\chrome-skia-2pages-texte-tableau-svg.pdf"
Key 0x73          # F4 : panneau latéral
Key 0x75          # F6 : zone suivante
Click 300 85      # clic en coordonnées client
Hover 300 85      # survol seul, sans clic (état survolé, position d'une note)
Typing "bonjour"  # saisie de texte
Chord "h"         # raccourci Ctrl+H (Chord "p" -Shift : Ctrl+Maj+P)
Shot "avant"      # copie du tampon dans scratchpad/avant.ppm
Journal "focus"   # lignes du journal qui contiennent « focus »
Stop-App
```

Options passées à `Start-App` par la table `-Env`, par exemple pour répondre aux dialogues :

```powershell
Start-App $pdf -Env @{ ACRUX_SAVE_DIR = "C:\temp\sortie"; ACRUX_CONFIRM = "oui" }
```

Sans ces réponses, le mode invisible tient le dialogue pour **annulé** plutôt que de l'ouvrir à
l'écran : ouverture et enregistrement de fichier (`ACRUX_OPEN_FILE`, `ACRUX_SAVE_FILE`,
`ACRUX_SAVE_DIR`), confirmation (`ACRUX_CONFIRM`) et impression (`ACRUX_PRINTER`, avec
`ACRUX_PRINT_OUTPUT`). Le journal note `… : dialogue du système évité (mode invisible)`.

`ACRUX_CLIPBOARD` donne au mode invisible un presse-papiers d'essai (`
` et `
` en toutes
lettres) : le vrai presse-papiers de la machine n'est jamais lu ni écrit.

Les captures sont des PPM bruts ; `tools/ppm2png.py` les convertit en PNG pour les regarder :
`python tools/ppm2png.py capture.ppm capture.png [pas] [x,y,largeur,hauteur]` (le pas sous-échantillonne,
le rectangle recadre).

Deux variables isolent un essai des autres : `ACRUX_TEST_DIR` (dossier des captures, du journal et
de l'instance ; par défaut `%TEMP%\acrux-tests`) et `ACRUX_EXE` (l'exécutable à lancer, par défaut
`target\debug\acrux.exe`). Le harnais n'arrête jamais que l'instance qu'il a lancée lui-même.

`Drag` trace un geste continu (bouton enfoncé, une suite de points, relâchement) : c'est ce
qui permet de tester l'outil « remplir et signer » sans rien afficher. C'est aussi l'épreuve
du « on se ravise » : les boutons des cartes n'agissent qu'au relâchement, pointeur dessus, et
un `Drag` qui part d'un bouton pour finir dehors ne doit rien déclencher.

```powershell
Typing "s"                                    # ouvre « remplir et signer »
Drag @(@(520,690), @(620,610), @(720,700))    # trace la signature dans la fenêtre de capture
Click 1117 865                                # Appliquer
Click 700 1000                                # poser la signature sur la page
```

Les coordonnées se donnent **comme on les lit sur la capture** : le harnais calcule l'échelle
entre le tampon dessiné et les coordonnées des messages de souris (la fenêtre n'étant jamais
montrée, les deux diffèrent quand l'écran n'est pas à 100 %).

Le clic droit et le menu contextuel au clavier ont les leurs :

```powershell
RightClick 450 344   # clic droit (survol, appui, relâchement) : le menu s'ouvre à l'appui
ShiftF10             # Maj+F10 ; la touche « menu » du clavier est Key 0x5D
```

Le relâchement sur place ne choisit rien, comme sous Windows : on choisit ensuite au clavier
(`Key 0x28` puis `Key 0x0D`), par l'initiale (`Typing "c"`) ou d'un `Click` sur l'élément. En mode
invisible, rien ne sort de la fenêtre : « Copier » et « Copier le chemin » écrivent dans le journal
(`presse-papiers : …`) au lieu du presse-papiers, un lien suivi note `adresse : …` au lieu d'ouvrir
le navigateur, et « Ouvrir le dossier du fichier » note `dossier : …` au lieu d'ouvrir
l'Explorateur.

La molette et Ctrl+touche ont leurs fonctions :

```powershell
Wheel 815 540 -3   # trois crans vers le bas, pointeur en (815, 540)
KeyCtrl 0x23       # Ctrl+Fin (0x24 : Ctrl+Origine)
```

`WM_MOUSEWHEEL` est le seul message de souris dont la position est en coordonnées
**d'écran** : `Wheel` applique l'échelle puis convertit le point (`ClientToScreen`), comme le
fait Windows, et l'application le reconvertit. Poster à la main un `WM_MOUSEWHEEL` avec des
coordonnées client viserait donc à côté. Le journal (`Journal "Wheel"`) montre la position
reçue, en coordonnées de la capture.

Codes de touches utiles : `0x73` F4, `0x74` F5, `0x75` F6, `0x7A` F11, `0x0D` Entrée, `0x1B` Échap,
`0x09` Tab, `0x20` Espace, `0x25` à `0x28` flèches gauche, haut, droite, bas, `0x21`/`0x22` page
précédente et suivante, `0x24`/`0x23` Origine et Fin.

## `release.ps1` — assembler une version publiable

Construit `dist/` : l'installateur chargé, l'archive portable, les sommes
SHA-256 et les notes de version. C'est le **même script** qui tourne en local
et dans le workflow de publication, pour que ce qu'on essaie sur sa machine
soit exactement ce qui sera publié.

```powershell
cargo build --release --workspace
.\tools\release.ps1 -Version 0.1.0
```

L'installateur se vérifie sans rien afficher, comme l'application :

```powershell
# Construit sa fenêtre sans la montrer, liste ses contrôles, et quitte.
$env:ACRUX_SETUP_HEADLESS = "1"; .\dist\acrux-setup.exe

# Installe puis désinstalle, sans une seule fenêtre.
.\dist\acrux-setup.exe --silent --dir C:\temp\essai
C:\temp\essai\desinstaller.exe --uninstall --silent
```

## À venir

Comparateur de rendu, générateur de corpus, mesures de performance.
