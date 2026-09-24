<img src="branding/acrux-icon-128.png" alt="" width="96" align="left" hspace="16">

# Acrux

Logiciel PDF natif, open source, écrit de zéro en Rust, qui vise à égaler puis dépasser
Adobe Acrobat : lecture, édition de qualité professionnelle, organisation, formulaires,
signatures, sécurité, accessibilité, prépresse, OCR et IA locale.

<br clear="left">

> Deux exécutables : **`acrux`**, l'application, et **`acr`**, la ligne de commande.
> La marque vit dans [`branding/`](branding/README.md).

**État.** Le moteur lit, répare, déchiffre, modifie et réécrit des PDF ; `acr render` produit des
images identiques à celles de Chrome sur le corpus. L'application `acrux.exe` lit, cherche,
sélectionne, annote, biffe, remplit les formulaires, organise les pages, modifie le texte, exporte
et imprime, avec une palette de commandes (Ctrl+Maj+P) et un panneau à cinq onglets. La ligne de
commande `acr` couvre une quarantaine d'opérations, dont la comparaison de documents, les
filigranes, l'accessibilité, le contrôle en amont PDF/A et PDF/X, et les signatures numériques.
La composition typographique est faite : ligatures, crénage, formes contextuelles arabes,
algorithme bidirectionnel et CJC vertical, lus directement dans les tables `GSUB`, `GPOS` et
`GDEF` des polices (`acrux-fonts/shape.rs`, note `spec-notes/opentype-gsub-gpos.md`).
Restent à faire, pour l'essentiel : l'OCR, les écritures syllabiques (indiennes, khmère, thaïe)
et les portages macOS et Linux. Voir [ROADMAP.md](ROADMAP.md).

## Principes
- **Tout est écrit ici.** Aucune bibliothèque PDF, aucun moteur de rendu, aucun rasteriseur
  de polices externe. On lit les spécifications, pas le code des autres.
- **Natif.** Un exécutable, pas de web, pas d'Electron, pas de runtime à installer.
- **Qualité Acrobat ou rien.** Une fonctionnalité incomplète reste expérimentale.
- **Contribuable par n'importe qui.** Un module par dossier, un README par module,
  des tests partout, une doc d'architecture à jour.

Détails : [CHARTE_PROJET.md](CHARTE_PROJET.md) · [ARCHITECTURE.md](ARCHITECTURE.md) ·
[CONTRIBUTING.md](CONTRIBUTING.md) · [FONCTIONNALITES_ADOBE_ACROBAT.txt](FONCTIONNALITES_ADOBE_ACROBAT.txt)

## Installer

Téléchargez **`acrux-setup.exe`** depuis la [page des
versions](https://github.com/ShaNouhAr/Acrux/releases/latest) et lancez-le.

L'installation se fait **dans votre compte** (`%LOCALAPPDATA%\Programs\Acrux`)
et ne demande **aucun droit d'administrateur** : elle fonctionne donc sur un
poste de travail verrouillé. Elle pose un raccourci dans le menu Démarrer,
propose Acrux dans « Ouvrir avec » pour les PDF — sans jamais s'imposer comme
lecteur par défaut — et s'inscrit dans « Applications installées », d'où elle se
retire proprement.

Qui préfère ne rien installer prend `acrux-<version>-portable.zip` : les deux
exécutables, à lancer depuis n'importe où, y compris une clé USB.

L'installateur est lui aussi écrit dans ce dépôt
([`crates/acrux-setup/`](crates/acrux-setup/)) : un exécutable auto-extractible,
sans WiX, sans NSIS, sans Inno Setup.

> Les exécutables ne sont pas signés par un certificat commercial — ils coûtent
> quelques centaines d'euros par an. Windows SmartScreen affichera donc un
> avertissement au premier lancement. Chaque version publie ses sommes SHA-256,
> et tout le code est ici.

### Mises à jour

Au lancement, **au plus une fois par jour**, Acrux demande à la page des
versions de ce dépôt s'il en existe une plus récente, et le signale dans la
barre d'état. C'est la seule chose qui sorte de votre machine : une requête
`GET` publique, sans identifiant et sans statistique. Rien n'est jamais
téléchargé ni installé sans que vous l'ayez demandé.

Pour la couper : « Paramètres » (bouton de la barre d'outils, ou palette) →
« Mises à jour » → « Jamais » ; ou, à la main, `mises-a-jour=0` dans
`%APPDATA%\Acrux\prefs.txt`. La recherche reste disponible à la demande, dans la
même fiche (« Rechercher maintenant ») ou la palette (Ctrl+Maj+P → « Rechercher
les mises à jour »).

## Compiler et tester (3 commandes)

Prérequis : Rust stable via [rustup](https://rustup.rs) et l'éditeur de liens de la
plateforme (Windows : Visual Studio Build Tools C++ ; Linux : `build-essential` ;
macOS : outils Xcode en ligne de commande). Détails dans [CONTRIBUTING.md](CONTRIBUTING.md).

```bash
cargo build --release
```

```bash
cargo test --workspace
```

```bash
cargo run --release --bin acrux
```

L'outil en ligne de commande `acr` (dans `target/release/`) sait déjà :

```bash
acr info fichier.pdf
```

`info`, `pages`, `dump`, `check` inspectent un fichier (y compris cassé ou chiffré avec un mot
de passe vide) ; `rotate`, `delete`, `reorder`, `extract`, `merge`, `rewrite` le modifient
(enregistrement incrémental par défaut, `--full` pour une réécriture complète ; `rewrite --compress`
recompresse les flux en Flate avec notre propre compresseur, `rewrite --compact` y ajoute les flux
d'objets et une table xref compressée) ;
`create` **fabrique** un PDF : `--blank` (page vierge, `--size A4|lettre|légal|tabloïd|dl|c5|210x297mm`,
`--orientation portrait|paysage`, `--pages N`, `--margin N`), `--images a.jpg b.png scan.tif`
(une image par page ou une grille `--grid 2x3`, `--fit contain|cover|actual`, `--dpi N`,
`--background r,g,b` ; formats lus **PNG, JPEG, BMP, GIF et TIFF** — un TIFF multipage, ce que
sort un scanner, donne une page par feuille ; le JPEG est incorporé **octet pour octet**, sans
recompression, les autres sont décodés et recompressés en Flate avec leur `/SMask` s'ils ont de
la transparence), `--text fichier.txt` (vraie mise en page :
coupure aux espaces, césure simple, paragraphes, `--align gauche|droite|centre|justifie` où la
dernière ligne d'un paragraphe ne se justifie jamais, pagination, `--font`, `--size`, `--margin`,
`--header`/`--footer` avec les jetons `{page}` et `{pages}`) et `--markdown fichier.md` (titres,
gras, italique, code, listes, citations, règles, tableaux et **liens cliquables**, les titres
devenant des signets) ; `combine a.pdf b.jpg c.md` réunit PDF, images et fichiers texte ou Markdown
en un seul document (`--bookmarks` : un signet par fichier ; `--toc` : sommaire cliquable en tête ;
`--numbers` : numérotation continue en pied de page) ; `render`
produit des PNG (`--dpi` ; `--rotate 90` tourne l'image sans toucher au fichier, comme la
rotation de la vue de l'application, là où `rotate` écrit `/Rotate`) ; `export` convertit (`--format png|jpeg|images|html|docx|xlsx|md|txt`,
`--dpi`, `--quality`, `--pages`, `--flow`) : pages en PNG ou en JPEG, images incorporées extraites
dans leur format d'origine, page HTML fidèle (texte positionné, images en `data:`, polices du PDF
en WOFF) ou refluée, document Word `.docx` et classeur Excel `.xlsx` des tableaux détectés ;
`find <f> <texte>` cherche avec le moteur de l'application (une ligne par occurrence :
page, ligne, contexte ; `--case` respecte la casse, `--word` ne prend que le mot entier,
`--page N`), et une occurrence peut passer à la ligne dans un paragraphe ;
`text` extrait le texte dans l'ordre de lecture (paragraphes, colonnes,
tableaux, en-têtes et pieds de page ; `--markdown` et `--html` conservent titres, gras, italique,
listes et tableaux, `--layout` garde le texte positionné comme sur la page) ; `annots` et `annotate` listent et
ajoutent des annotations ; `protect` chiffre en AES-256 avec une clé tirée d'un générateur
cryptographique (mot de passe d'ouverture `--user`, mot de passe des permissions `--owner`,
permissions `--print none|low|high`, `--no-modify`, `--no-copy`, `--no-annotate`, `--no-fill`,
`--no-accessibility`, `--no-assemble` ; force des mots de passe affichée) et `unprotect` retire le
chiffrement avec le mot de passe des permissions ; `info` dit le chiffrement, l'accès et les
permissions d'un document protégé ; `redact` biffe définitivement (`--rect`, `--find`, `--pattern email,iban,carte…`, texte de remplacement `--text`) en retirant vraiment le texte et les pixels du fichier, et `sanitize` supprime les données cachées (`--metadata`, `--attachments`, `--javascript`, `--layers`, `--comments`, `--forms`, `--invisible-text`, `--all`) ; `links` et `outline` listent les liens et les signets ;
`attachments` liste les **pièces jointes** (nom, taille, type, page, description), `attach` en
incorpore une (flux compressé en Flate, `/Params` avec taille, dates et somme MD5 ; `--description`,
`--page N` pose en plus une icône de trombone sur la page) et `detach` en extrait une en vérifiant
sa somme de contrôle ; `labels` affiche l'**étiquette** de chaque page et `set-labels` les redéfinit
(`1:i,5:D:1` : chiffres romains puis décimaux ; `1:r:Annexe-` ajoute un préfixe) ;
`metadata` affiche les **propriétés du document** — `/Info`, paquet XMP, divergences entre les
deux, propriétés personnalisées et vue initiale — et `set-metadata` les écrit (`--title`,
`--author`, `--subject`, `--keywords`, `--creator`, `--producer` ; une valeur vide efface le
champ, `--clear` assainit tout) **des deux côtés à la fois**, sans perdre les propriétés XMP
qu'on ne connaît pas ; `view-prefs` lit et règle les **propriétés d'ouverture** (`--page-mode`
signets / vignettes / plein-ecran / calques / pieces-jointes, `--layout` une-page / continu /
deux-colonnes / deux-pages, `--open-page`, `--open-zoom` en pourcentage ou fit / largeur /
hauteur / contenu) ;
`bookmarks` affiche l'arbre des **signets** avec leur style et leur cible, `set-bookmarks` le
remplace depuis un fichier texte (une ligne par signet, `titre<TAB>page`, la profondeur donnée
par les tabulations de début de ligne) et `auto-bookmarks` le déduit des titres du document
(structure balisée si elle existe, sinon la mise en page) ;
`autolink` pose un lien sur **chaque adresse du texte** (http, https, mailto, `www.`) en suivant
le rectangle exact des mots, sans rien dessiner, et `link` en pose un à la main
(`link <f> <page> <x0,y0,x1,y1> <cible>`, la cible étant une adresse, `page:12`,
`fichier.pdf#4` ou `nommee:NextPage`) ;
`edit-text` **modifie le texte dans la page** (`--find`, `--replace`, `--page`, `--all`, `--size`,
`--color r,g,b`, `--font`, `--bold`, `--italic`, `--case`, `--word`) : le flux de contenu est réécrit octet pour octet
sauf le mot visé, réencodé avec la police en place (complétée depuis la police système si un
caractère manque), et le texte qui suit ne bouge pas d'un point ; `reflow` recompose un paragraphe
entier dans sa boîte (`--page`, `--paragraph`, `--text`, `--shrink`) en conservant interligne,
alignement et retrait de première ligne ;
`fields` liste les champs de formulaire (un saut de ligne s'écrit `\n`), `fill` les remplit (`nom=valeur`, apparences régénérées,
`--reset` pour effacer d'abord le formulaire, `--flatten` pour aplatir ; une valeur hors WinAnsiEncoding incorpore la police qu'il faut, donc un formulaire se remplit en japonais ou en russe), `fdf-export` / `fdf-import` échangent les données au format FDF ;
`check-a11y` vérifie l'accessibilité au sens de **PDF/UA-1** (document balisé, langue, titre,
texte de remplacement des figures, ordre de lecture, hiérarchie des titres, en-têtes de tableau,
listes, contraste WCAG, texte extractible, champs sans info-bulle, contenu non balisé ; `--json`
pour une sortie exploitable) et `autotag` **balise automatiquement le document** (arbre de
structure déduit de la mise en page, marques `BDC`/`EMC` insérées dans le flux réécrit octet pour
octet, `/Lang` déduit du texte) ; `preflight --profile pdfa-1b|pdfa-2b|pdfa-3b|pdfx-1a|pdfx-4|pdfua-1`
contrôle la conformité et, avec `--fix -o sortie.pdf`, répare ce qui l'est sans perte (métadonnées
XMP, intention de sortie sRVB avec un profil ICC que nous engendrons, incorporation des polices,
retrait du JavaScript et des pièces jointes, `/Interpolate false`, `/TrimBox`, `/DisplayDocTitle`) ;
`separations` produit l'aperçu de sortie (une image par plaque CMJN et par ton direct, plus le taux
d'encre total) ;
`compare <avant.pdf> <apres.pdf>` **compare deux documents** : les pages sont d'abord appariées par
leur contenu (une page insérée, supprimée ou déplacée est reconnue comme telle), puis le texte de
chaque paire est aligné mot à mot (ajouts, suppressions, remplacements, blocs déplacés d'une page à
l'autre, avec leur rectangle dans chacun des deux documents) ; `--visual` compare aussi les pixels
des deux rendus (`--tolerance N` pour l'anticrénelage) et rend les zones qui changent ; `--pages`
n'affiche que le tableau d'appariement ; `-o rapport.pdf` écrit un **rapport PDF côte à côte**
(ajouts en vert, suppressions en rouge, déplacements en bleu, page de résumé en tête), sinon un
résumé lisible sur la sortie standard ;
`watermark`, `background`, `header-footer` et `bates` posent les **apports visuels** d'Acrobat sur
un document existant, sans jamais réécrire son contenu : le tampon est un flux ajouté au
`/Contents` de la page, le dessin d'un filigrane est un **XObject de formulaire unique partagé par
toutes les pages**, et `unstamp` retire tout cela (le rendu redevient identique au pixel près).
`watermark --text "BROUILLON"` ou `--image logo.png` (PNG ou JPEG ; un JPEG est incorporé tel quel)
avec `--font`, `--size`, `--color r,g,b`, `--opacity` (`/ExtGState` `/ca` `/CA`), `--rotation`,
`--anchor` parmi neuf ancrages (`haut-gauche`… `bas-droite`), `--offset x,y`, `--scale N`
(absolue), `--fit N` (fraction de la page) ou `--stretch`, `--margin`, `--behind` pour passer
derrière le contenu, `--pages 1,3-5` ; `background` fait de même mais toujours derrière, et sans
`--image` ni `--text` remplit la page de la couleur `--color` ; `header-footer` écrit six zones
(`--header-left|-center|-right`, `--footer-left|-center|-right`) où les jetons `{page}`, `{pages}`,
`{date}`, `{time}`, `{datetime}` et `{filename}` sont remplacés page par page, avec
`--margin-top|-bottom|-left|-right`, `--start N` (décalage du numéro de départ) et
`--format 1|i|I|a|A` ; les dates sont écrites en UTC, `--utc-offset +02:00` donne le fuseau voulu
(aucune bibliothèque du projet ne lit l'horloge locale du système) ; `bates` numérote (`--prefix`, `--suffix`, `--digits`, `--start`, `--anchor`,
`--margin-x`, `--margin-y`) et, si on lui donne **plusieurs fichiers**, poursuit la numérotation de
l'un à l'autre (sorties `sortie-1.pdf`, `sortie-2.pdf`…) ; le texte posé utilise une police
standard tant qu'il tient en WinAnsiEncoding, sinon une **police système incorporée en
sous-ensemble** (`acrux-features/fontembed.rs`), ce qui permet de tamponner en japonais, en russe ou
en grec ; dans les deux cas un `/ToUnicode` garde le texte extractible par `acr text` ;
`signatures` liste les **signatures numériques** du document (signataire, date, motif,
`/ByteRange`, sous-filtre), `verify` rend **cinq verdicts séparés** par signature —
condensé des octets couverts, signature RSA, chaîne de certificats jusqu'à une racine
fournie (`--roots <dossier de certificats DER>`), **couverture de tout le fichier** et
**modifications postérieures** (`--json` pour une sortie exploitable) — et `sign` signe
(`--key cle.der --cert cert.der`, éventuellement `--chain`, `--reason`, `--location`,
`--name`, `--field`, et `--page N --rect x0,y0,x1,y1` pour une signature visible) :
enveloppe CMS détachée `adbe.pkcs7.detached` en SHA-256, ajoutée par mise à jour
incrémentale, `/ByteRange` calculé après écriture sans décaler un octet ;
`media` liste les **vidéos et les sons** du document (`/Screen`, `/RichMedia`, `/Movie`, `/Sound`)
avec leur rectangle, leur type et leur taille, et `--extract <dossier>` en sort les fichiers
incorporés — un `/Sound`, qui ne contient que des échantillons bruts, ressort en WAV ;
`3d` liste les **modèles 3D** (annotations `/3D`) avec leur format, leur rectangle, leur vue par
défaut et la géométrie lue — nombre d'objets, de sommets et de triangles —, et `--extract <dossier>`
en sort les fichiers U3D tels quels ; `create --3d <modele.u3d>` fait le chemin inverse, un PDF
portant le modèle, sa vue et l'affiche rendue par notre moteur 3D ;
`objects` liste les **objets dessinés** par une page (images, tracés, blocs de texte, dégradés)
avec leur boîte, et `edit-object` les modifie : `--move dx,dy`, `--scale sx[,sy]` (autour du
centre), `--rotate deg`, `--place x0,y0,x1,y1`, `--crop x0,y0,x1,y1`, `--order
devant|derriere|avancer|reculer`, `--delete`, `--image f.png` pour en remplacer une, et
`--object 2,4,7 --align gauche|centre-x|droite|haut|centre-y|bas` pour en aligner plusieurs ;
l'objet est encadré dans le flux et **tout le reste est recopié octet pour octet** ;
`fillsign` **remplit et signe** — l'outil courant d'Acrobat, celui qui n'a rien de
cryptographique : `place --page N --rect x0,y0,x1,y1` avec `--draw traits.txt` (une signature
**tracée** au pointeur : le relevé devient un contour à largeur variable, effilé aux bouts et
rond dans les virages), `--typed "Élise Marchand"` (une police **manuscrite** du système,
incorporée en sous-ensemble), `--image photo.png` (une photo de signature **détourée** : le
papier disparaît, y compris sous un éclairage inégal ou une ombre de coin), `--text "Paris, le
19 septembre"` ou `--mark check|cross|circle|line|dot` ; `--color r,g,b`, `--pen`, `--stretch`,
`--author`, `--flatten`. `fillsign <f> list` inventorie ce qui a été posé, `remove <page> <n>`
en retire un, `flatten` les fond définitivement dans les pages ;
`bench` mesure les performances. L'option globale
`--password <mdp>` ouvre un document chiffré.

L'application graphique : `acrux.exe fichier.pdf` (ou Ctrl+O, ou déposer un fichier).

**La recherche (Ctrl+F)** est une carte flottante en haut à droite : le champ, « Aa » (respecter
la casse), « Mot entier », occurrence précédente et suivante, et une croix pour fermer ; en
dessous, « 3 sur 12 », ou « Aucun résultat » en rouge avec le champ cerclé de rouge. La première
occurrence montrée est la première **à partir de la page affichée**, comme dans Acrobat, et un
gros document se parcourt par tranches sans figer la fenêtre. Une expression coupée par une fin
de ligne se trouve, césure comprise (« docu- / mentation » en tapant « documentation »), sans
jamais joindre deux paragraphes ; l'apostrophe typographique, l'espace insécable et les ligatures
(« ﬁ ») ne font plus échouer une recherche tapée au clavier.

**Ctrl+Maj+P ouvre la palette de commandes.** Elle liste tout ce que l'application sait faire
avec le raccourci de chaque entrée, se filtre en tapant (sans accents, et « rotation » trouve
« Pivoter la page »), et remplace la barre de menus que ce logiciel n'a pas. Les commandes y
suivent l'ordre de l'usage — ouvrir, enregistrer, rechercher, imprimer, remplir et signer… —,
précédées des cinq dernières lancées depuis la palette, retenues d'une séance à l'autre. Les
lettres trouvées s'éclairent ; la liste défile avec la sélection (flèches, `PgUp` / `PgDn`,
`Ctrl+Origine` / `Ctrl+Fin`, molette, ascenseur), et Entrée lance toujours la ligne que l'on voit.
En anglais, on cherche dans les libellés anglais. Le reste de cette section n'est donc qu'un
aide-mémoire.

**Le clic droit ouvre un menu** (ou la touche « menu » du clavier, ou `Maj+F10`) : sur la page
(copier, surligner, poser une note ici, tout sélectionner, pivoter la page, imprimer, propriétés),
sur une vignette (pivoter, dupliquer, extraire, supprimer **cette** page), sur un onglet (fermer,
fermer les autres, copier le chemin, ouvrir le dossier du fichier) et sur un document récent de
l'accueil (ouvrir, copier le chemin, ouvrir le dossier, retirer de la liste). Chaque élément lance
la commande de la palette et affiche son raccourci ; flèches, `Entrée`, `Échap` et l'initiale d'un
élément s'y emploient comme dans un menu de Windows.

Barre d'outils : accueil, panneau latéral (vignettes et signets), ouvrir, page précédente /
suivante, numéro de page (champ encadré : cliquer pour saisir), zoom −/+ et **liste du zoom**
(la case « 125 % » se déroule : 50 à 400 %, ajustement automatique, à la largeur, à la page, et un
champ où taper n'importe quel niveau, « 137 » ou « 137,5 % », puis `Entrée` ; sans document, elle
affiche « – % » grisé), ajuster à la largeur, disposition des pages, **annuler, rétablir** (grisés quand il n'y a rien à défaire ou à refaire),
pivoter, enregistrer, imprimer, recherche, tous les outils, paramètres, thème (la lune en thème
clair, le soleil en thème sombre : le bouton montre où l'on va). Les bascules du panneau et des
outils s'allument quand ce qu'elles montrent est affiché. Chaque bouton affiche au survol ce qu'il
fera et son raccourci ; un bouton grisé n'a pas d'info-bulle. Dans une fenêtre étroite, les boutons
secondaires (disposition, ajustement, imprimer, pivoter, accueil, puis zoom et pages) s'effacent
pour que le groupe de droite reste visible. Survoler un **onglet** montre le chemin complet du
document et s'il reste des modifications à enregistrer ; le **clic du milieu** le ferme (en
proposant d'abord d'enregistrer). Quand le document déclare des **étiquettes de page** (`/PageLabels`), c'est
l'étiquette qui s'affiche et qui se tape : une préface numérotée i, ii, iii donne « iii / 240 »
dans la barre d'outils et « page iii (3 / 240) » dans la barre d'état, et saisir « iv » ou
« Annexe-A » mène à la bonne page.

La **barre d'état** se clique : la page y prend le champ de page (comme `Ctrl+G`), le zoom y
déroule la même liste que la barre d'outils, au-dessus de lui, et la disposition y passe à la
suivante ; chacun s'éclaire au survol et dit ce que fera le clic. Zoomer ne fait plus clignoter la
page en blanc : en attendant le rendu à la nouvelle échelle, l'image précédente est étirée à sa
place. Le zoom est plafonné pour qu'une page rendue ne dépasse pas 256 Mo (environ 570 % pour de
l'A4 sur un écran à 150 %) ; la barre d'état le dit quand on bute dessus.

| Naviguer | |
| --- | --- |
| `Ctrl+PgDn` / `Ctrl+PgUp` | page suivante / précédente |
| `Origine` / `Fin` | première / dernière page |
| molette, bouton du milieu sur la page, glisser hors du texte | défiler, déplacer (la molette fait défiler ce qui est sous le pointeur — panneau, colonne d'outils —, et jamais le document sous une palette, une invite ou un sélecteur ouverts) |
| clic sur un lien | destination interne, ou adresse dans le navigateur |
| `F4` | panneau latéral : pages (glisser pour réordonner), signets, notes, calques, fichiers joints |
| `Ctrl+G` | aller à une page par son numéro **ou son étiquette** (« iv », « Annexe-A ») |
| `Alt+←` / `Alt+→`, boutons latéraux de la souris | **vue précédente / suivante** : revenir d'où un lien, un signet, `Ctrl+G`, `Origine`/`Fin`, le panneau ou la recherche vous a fait partir (un historique par onglet, aussi dans le clic droit et la palette) |
| `Ctrl+Tab` / `Ctrl+Maj+Tab` | onglet suivant / précédent, même la recherche ouverte ; chaque onglet retrouve la page où on l'a laissé |
| `Ctrl+W`, `Ctrl+F4`, clic du milieu | fermer l'onglet |

| Voir | |
| --- | --- |
| `+` / `-` | zoom, centré sur la vue |
| `Ctrl+molette` | zoom **vers le pointeur** : ce qui est sous la souris y reste (les petits crans d'un pavé tactile s'additionnent) |
| clic sur la case du zoom, ou sur le zoom de la barre d'état | liste du zoom : niveaux, ajustements, niveau tapé |
| `F`, `Ctrl+0` | ajuster à la largeur, ajustement automatique (la liste ajoute « ajuster à la page ») |
| `Ctrl+Maj+Plus` / `Ctrl+Maj+Moins` | **faire pivoter la vue** d'un quart de tour : pour lire un tableau couché ou un plan scanné de travers ; le document n'est pas modifié (la barre d'état affiche « vue 90° », un clic la remet droite) — `R` pivote la page elle-même |
| bouton « disposition » | continu, page unique, continu deux pages, deux pages |
| `F11` | plein écran |
| `F5` | mode lecture : plus de barres ni de panneau, seulement les pages (Échap pour revenir) |
| `F6` / `Maj+F6` | zone suivante ou précédente : document, barre d'outils, panneau |
| flèches, `Entrée`, `Espace` | dans une zone : déplacer le focus et activer ; `Échap` revient au document |
| `T` | thème clair / sombre |

| Lire et chercher | |
| --- | --- |
| glisser sur le texte | sélectionner (double-clic : mot, triple-clic : ligne, `Maj+clic` : étendre) |
| `Ctrl+C`, `Ctrl+A` | copier, tout sélectionner |
| `Ctrl+D` | propriétés du document : fichier, taille, pages, version, métadonnées, protection |
| `Ctrl+F` | rechercher : le texte sélectionné sur la page devient la requête ; carte déjà ouverte, la requête est sélectionnée (on la relance ou on tape par-dessus) |
| `Entrée` / `Maj+Entrée`, `F3` / `Maj+F3` | occurrence suivante / précédente (`F3` carte fermée relance la dernière recherche) |
| « Aa », « Mot entier », ‹ › et × de la carte | respecter la casse, mot entier, précédente / suivante, fermer (`Échap`) |
| `Ctrl+H` | rechercher et remplacer (`Tab` : d'un champ à l'autre, `Entrée` : remplacer, ou « Tout remplacer ») |

Un **modèle 3D** (annotation `/3D` au format U3D) s'affiche sur la page ; un clic l'active, on le
tourne en glissant, la molette s'en approche, `Échap` le referme.

Ouvrir une **image** (PNG, JPEG, BMP, GIF, TIFF) la convertit en PDF : un TIFF de scanner donne
une page par feuille, et `Ctrl+S` demande où ranger le document obtenu.

| Modifier | |
| --- | --- |
| `R` / `Maj+R` | pivoter la page courante |
| `Ctrl+Suppr` | supprimer la page (confirmation) |
| `Ctrl+I` | insérer les pages d'un autre fichier avant la page courante |
| palette, clic droit sur une vignette | dupliquer la page, extraire la page dans un nouveau fichier |
| `E` | modifier le texte sélectionné (même police, la ligne se recompose) |
| `H`, `Maj+H` | surligner la sélection, la surligner en y joignant un commentaire |
| `N` | poser une note à la position de la souris |
| clic sur une vidéo ou un son | lecture ; re-clic pour mettre en pause, clic sur la ligne de temps pour se déplacer |
| `Maj+F4` | barre des outils, à droite (modifier, commenter, signer, pages, biffer) |
| `Ctrl+Maj+E` | **modifier le PDF** : on clique dans un texte et on tape ; ailleurs, on pose une zone |
| `O` | modifier les objets : sélection, poignées, ordre, suppression |
| `S` | remplir et signer : signature, paraphe, texte tapé sur la page, stylo, marques (encre au choix) |
| `M`, `Maj+M` | biffure : marquer (réversible), puis appliquer définitivement |
| onglet « Fichiers » du panneau | pièces jointes : clic pour enregistrer, bouton pour en joindre une |
| clic sur un champ de formulaire | le remplir **sur place** : on tape dans le champ, `Entrée` valide, `Échap` annule ; liste déroulante sous son champ |
| `Tab` / `Maj+Tab` | champ suivant, précédent (ordre `/Tabs` du document) ; flèches : ligne d'une liste, bouton d'un groupe radio |
| barre de formulaire, palette | surligner les champs, effacer le formulaire, aplatir le formulaire |
| `Ctrl+Z` / `Ctrl+Y` (ou `Ctrl+Maj+Z`), ou les flèches de la barre d'outils | annuler, rétablir (dans un bloc en cours de saisie, étape par étape d'abord) |

| Enregistrer | |
| --- | --- |
| `Ctrl+S` | ajout incrémental (réécriture complète si le fichier a été réparé) |
| `Ctrl+Maj+S` | enregistrer sous (réécriture complète) |
| `Ctrl+P` | imprimer (dialogue Windows, pages ajustées à la zone imprimable) |
| `Ctrl+E` | exporter : l'extension choisie décide du format (html, docx, xlsx, md, txt, png, jpg) |

L'export est fait par le fil de rendu : une centaine de pages en images ne fige pas la fenêtre,
et la barre d'état annonce la fin. Le titre porte « * » tant que des modifications ne sont pas
enregistrées. Les documents chiffrés
demandent leur mot de passe à l'ouverture, et ce qu'ils interdisent est dit dans la barre d'état.

« Protéger par mot de passe » (palette, ou colonne d'outils) pose un mot de passe d'ouverture et
un mot de passe des permissions, chacun saisi deux fois avec une jauge de force, et règle
l'impression (non, basse, haute résolution), la modification, la copie, les commentaires, les
formulaires, l'accessibilité et l'assemblage. Un document ouvert avec le seul mot de passe
d'ouverture n'accorde que ses permissions ; chaque refus propose le mot de passe des permissions,
qui seul permet aussi de changer ou de retirer la protection.

**« Paramètres »** (bouton de la barre d'outils, ou palette) est une fiche : langue (celle du
système, français ou anglais), thème clair ou sombre, recherche des mises à jour au démarrage ou
jamais, version installée. Un clic change le réglage sur-le-champ. Dans cette fiche comme dans
toutes les questions et invites, un bouton n'agit qu'au relâchement — on se ravise en glissant
hors de lui —, Tab passe d'un élément à l'autre et Échap ferme.

Les réglages (thème, disposition, zoom, panneau, taille de fenêtre, fichiers récents) sont
conservés dans `%APPDATA%\Acrux\prefs.txt`, un fichier texte `clé=valeur` lisible et
modifiable à la main ; une ligne inconnue ou illisible retombe simplement sur la valeur par défaut.

Variables d'environnement de diagnostic : `ACRUX_LOG=<fichier>` journalise les événements et
les peintures, `ACRUX_SHOT=<fichier.ppm>` écrit le tampon de la fenêtre (PPM binaire) à chaque peinture,
`ACRUX_THEME=light` démarre en thème clair (il force le thème clair quelles que soient les
préférences), `ACRUX_PRINTER=<nom>` (et `ACRUX_PRINT_OUTPUT=<fichier>`)
imprime sans dialogue vers cette imprimante (tests : « Microsoft Print to PDF »).

## Organisation

| Dossier | Rôle |
|---|---|
| `crates/acrux-core` | Types de base, erreurs, géométrie |
| `crates/acrux-document` | Syntaxe PDF : objets, xref, flux, pages, écriture |
| `crates/acrux-codecs` | Flate, LZW, CCITT, JPEG, JPEG 2000, JBIG2… |
| `crates/acrux-fonts` | TrueType, CFF, Type1, encodages, sous-ensembles, composition (GSUB/GPOS, bidi) |
| `crates/acrux-graphics` | Rasteriseur 2D, couleur, transparence |
| `crates/acrux-render` | Interpréteur de contenu de page, extraction de texte |
| `crates/acrux-features` | Édition de texte in place, pages, annotations, formulaires, biffure, export… |
| `crates/acrux-app` | Application native (exécutable `acrux`) |
| `crates/acrux-cli` | Outil `acr` pour contributeurs et scripts |
| `tests/corpus` | PDF de test, `tests/reference` images de référence, `tests/fuzz` cibles de fuzzing |
| `spec-notes` | Résumés des spécifications avec renvois vers le code |
| `docs` | Documentation utilisateur et développeur |

## Licence
Apache-2.0. Voir [LICENSE](LICENSE).
