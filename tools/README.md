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
Key 0x72 -Shift   # Maj+F3 : occurrence précédente (-Shift tient Maj le temps de la touche)
Key 0x75          # F6 : zone suivante
Click 300 85      # clic en coordonnées client
Click 150 400 -Ctrl   # Ctrl+clic (ajoute une vignette à la sélection) ; -Shift : Maj+clic (une plage)
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
`ACRUX_CLIPBOARD_IMAGE` fait de même pour une **image** : le chemin d'un fichier image (PNG,
BMP…) que `Ctrl+V` sur la page et « Nouveau PDF depuis le presse-papiers » liront comme si on
l'avait copiée. `ACRUX_OPEN_IMAGE` répond au choix d'une image (« Ajouter une image », « Depuis
une image… » du sélecteur de tampons), comme `ACRUX_OPEN_FILE` à l'ouverture d'un document.
Le journal note `tampon : …` (sélecteur, choix, `tampon posé page N …`) et `image : …`
(`image posée page N en (x, y), L × H pt`).

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

Deux options servent aux outils de dessin : `-Shift` tient Maj enfoncée pendant tout le geste
(Maj simulée pour la fenêtre invisible et `MK_SHIFT` dans les mouvements, comme la vraie
souris) ; `-Hold` ne relâche pas le bouton, pour capturer l'aperçu du geste en cours, que
`Release x y` termine.

```powershell
Drag -Shift @(@(500,300), @(560,330), @(600,350))   # une ellipse devient un cercle
Drag -Hold @(@(300,300), @(360,340), @(420,380))    # le geste reste en l'air…
Shot "apercu"                                      # …on voit l'aperçu…
Release 420 380                                    # …puis la forme est posée
```

Les coordonnées se donnent **comme on les lit sur la capture** : le harnais calcule l'échelle
entre le tampon dessiné et les coordonnées des messages de souris (la fenêtre n'étant jamais
montrée, les deux diffèrent quand l'écran n'est pas à 100 %).

`Click -Ctrl` et `Click -Shift` postent `WM_KEYDOWN` de Ctrl ou de Maj avant le clic, puis
`WM_KEYUP` après : la fenêtre invisible retient ces touches (`GetKeyState` ne voit pas le vrai
clavier) et les rend au clic, avec `MK_CONTROL` ou `MK_SHIFT` dans `wParam` comme la vraie souris.
C'est ainsi qu'on essaie la sélection multiple des vignettes (Ctrl+clic, Maj+clic).

Le clic droit, le clic du milieu et le menu contextuel au clavier ont les leurs :

```powershell
RightClick 450 344   # clic droit (survol, appui, relâchement) : le menu s'ouvre à l'appui
MiddleClick 120 75   # clic du milieu : ferme l'onglet visé, ou saisit la page pour la faire glisser
ShiftF10             # Maj+F10 ; la touche « menu » du clavier est Key 0x5D
```

`Resize 820 700` redimensionne la fenêtre invisible (taille extérieure en pixels physiques) sans
jamais la montrer ni lui donner le premier plan : c'est l'essai d'une fenêtre étroite, où la barre
d'outils efface ses boutons secondaires. L'échelle des coordonnées est recalculée après.

Une info-bulle n'apparaît qu'après un délai de survol : `Hover x y`, puis
`Start-Sleep -Milliseconds 900` avant le `Shot`. Le réveil qui la fait peindre part de la
fenêtre elle-même, le harnais n'a rien à poster de plus.

Le relâchement sur place ne choisit rien, comme sous Windows : on choisit ensuite au clavier
(`Key 0x28` puis `Key 0x0D`), par l'initiale (`Typing "c"`) ou d'un `Click` sur l'élément. En mode
invisible, rien ne sort de la fenêtre : « Copier » et « Copier le chemin » écrivent dans le journal
(`presse-papiers : …`) au lieu du presse-papiers, un lien suivi note `adresse : …` au lieu d'ouvrir
le navigateur, et « Ouvrir le dossier du fichier » note `dossier : …` au lieu d'ouvrir
l'Explorateur.

La molette et Ctrl+touche ont leurs fonctions :

```powershell
Wheel 815 540 -3          # trois crans vers le bas, pointeur en (815, 540)
Wheel 815 540 1 -Ctrl     # Ctrl+molette : un cran de zoom vers le pointeur
Wheel 815 540 0.25 -Ctrl  # un quart de cran, comme un pavé tactile de précision
KeyCtrl 0x23              # Ctrl+Fin (0x24 : Ctrl+Origine)
ShotNow "apercu"          # capture immédiate, sans attendre la fin des rendus
```

`Shot` attend 700 ms que les rendus arrivent ; `ShotNow` capture tout de suite ce qui est à
l'écran — par exemple la page étirée qu'affiche le zoom pendant le rendu à la nouvelle échelle.
Le journal note `zoom : 300 % vers (x, y)` à chaque changement de zoom, `aperçu étiré : …` quand
une page est montrée étirée, et `rendu : page N en M ms` pour chaque rendu (la barre d'état ne
l'affiche plus).

La recherche note ce qu'elle fait dans le journal : `recherche : ouverte « … »`, `recherche :
« mot » : 12 occurrence(s)` à la fin de chaque parcours, `recherche : « mot » 3 sur 12` à
chaque saut, `recherche : respecter la casse activé`, `recherche : fermée`. Un parcours
interrompu par une frappe ne note pas de total. Pour sélectionner un mot de la page avant
`Chord "f"`, un `Drag` doit **commencer sur le texte** : parti de la marge, le geste fait
glisser la page au lieu de sélectionner.

`WM_MOUSEWHEEL` est le seul message de souris dont la position est en coordonnées
**d'écran** : `Wheel` applique l'échelle puis convertit le point (`ClientToScreen`), comme le
fait Windows, et l'application le reconvertit. Poster à la main un `WM_MOUSEWHEEL` avec des
coordonnées client viserait donc à côté. Le journal (`Journal "Wheel"`) montre la position
reçue, en coordonnées de la capture.

La navigation a ses trois fonctions :

```powershell
AltKey 0x25          # Alt+← : vue précédente (0x27 : Alt+→, vue suivante)
XButton 1 700 600    # bouton « précédent » de la souris (4) ; XButton 2 : « suivant » (5)
CtrlShiftKey 0xBB    # Ctrl+Maj+Plus : tourner la vue (0xBD : Ctrl+Maj+Moins ; 0x09 : Ctrl+Maj+Tab)
KeyCtrl 0x09         # Ctrl+Tab (0x73 : Ctrl+F4, fermer l'onglet)
```

Alt+flèche arrive en `WM_SYSKEYDOWN`, avec le bit 29 de `lParam` qui dit qu'Alt est enfoncée :
c'est ce bit que lit l'application, `GetKeyState` ne voyant rien en mode invisible. Le journal
note `historique : vue retenue (page N), saut vers la page M` à chaque saut retenu,
`historique : vue précédente → page N` (ou `suivante`) à chaque retour, `historique : pas de vue
précédente` quand la pile est vide, et `vue pivotée : 90°` à chaque rotation de la vue. Pour
ouvrir deux onglets d'un coup, `Start-App "a.pdf b.pdf"` (chemins sans espace).

Les commentaires notent leurs gestes : `commentaire : sélection page 1 #4 Square`, `commentaire :
déplacé page 1 #4 [x0 y0 x1 y1]`, `redimensionné`, `couleur`, `opacité`, `supprimé`, `bulle`,
`réponse publiée`, `Marked(true)` ou `Review(Accepted)` pour une case ou un statut. Le harnais n'a
pas de double-clic tout fait : on poste `0x0201`, `0x0202`, puis `0x0203` (`WM_LBUTTONDBLCLK`) et
`0x0202` au même point — c'est ce qui ouvre la bulle d'un surlignage. Une question de
l'application (« Supprimer le commentaire et ses 2 réponse(s) ? ») se répond au clavier, `Entrée`
pour le bouton principal : `ACRUX_CONFIRM` ne vaut que pour les dialogues du système.

Codes de touches utiles : `0x72` F3, `0x73` F4, `0x74` F5, `0x75` F6, `0x7A` F11, `0x0D` Entrée, `0x1B` Échap,
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
