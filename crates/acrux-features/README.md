# acrux-features

Fonctionnalités métier : édition, pages, annotations, formulaires, biffure, export, sécurité…

Voir `ARCHITECTURE.md` à la racine pour la place de ce crate dans l'ensemble,
et `src/lib.rs` pour la liste des modules et de ceux qui restent à écrire.

## Remplir un formulaire sur place (`forms`)

L'inventaire, le remplissage, l'aplatissement et le FDF sont là depuis
longtemps ; ce que l'application demande en plus pour qu'on remplisse **dans
la page**, comme dans Acrobat :

| Fonction | Ce qu'elle rend |
| --- | --- |
| `field_look` | l'aspect d'un widget (corps, couleur, fond, bordure, marges, cases d'un peigne, famille de la police) : la saisie se dessine comme l'apparence se dessinera |
| `FieldLook::text_size` | le corps d'un texte selon la règle de la taille automatique, **la même** que celle qui écrit l'apparence : rien ne saute à la validation |
| `tab_order` | l'ordre de Tab : `/Tabs /R` (rangées), `/C` (colonnes), sinon l'ordre de `/Annots` ; un arrêt par groupe radio |
| `list_row_at` | l'option d'une liste sous un point, avec la géométrie de l'apparence (`/TI` compris) |
| `reset_fields` | « Effacer le formulaire » : `/DV`, sinon une valeur vide |
| `prepare_display` | les apparences qui manquent (`/NeedAppearances`, widget sans `/AP`), à l'ouverture ; rien n'est touché dans un formulaire complet |

Le corpus `synthese/formulaire-obligatoire-sans-apparence.pdf` réunit ce que
`prepare_display` répare : des valeurs sans apparence, une apparence
périmée, deux champs obligatoires et un ordre de tabulation par colonnes.

## Annoter le texte (`annotations`)

Relire un document, c'est souligner, barrer, insérer et remplacer. Ce que
pose `add_annotation` — avec une apparence écrite, pour que tout lecteur
l'affiche de la même façon :

| Annotation | Ce qu'elle devient dans le fichier |
| --- | --- |
| `Markup` | `/Highlight`, `/Underline`, `/StrikeOut` ou `/Squiggly` : **une** annotation par page, un quadrilatère par ligne (`/QuadPoints`), comme Acrobat ; couleurs par défaut dans `MarkupKind::default_color` |
| `Caret` | signe d'insertion `/Caret` entre deux caractères, le texte à ajouter dans `/Contents` |
| `Replace` | le passage barré et un signe d'insertion **groupés** (§12.5.6.2) : le signe porte le texte proposé (`/IT /Replace`), le barré le suit (`/IRT` vers lui, `/RT /Group`, `/IT /StrikeOutTextEdit`) ; `remove_annotation` retire le groupe entier |

Chaque annotation créée porte un identifiant unique `/NM` et sa date de
création. L'identité (`AnnotMeta`) se tire **avant** l'écriture, par qui décide
de poser l'annotation, et se transmet telle quelle à `add_annotation_with` :
l'application applique chaque modification à deux copies du document et la
rejoue à chaque annulation, et une même modification doit donner partout la
même annotation. `list_annotations` rend `/NM`, `/IRT`, `/RT` et `/IT` : de quoi
regrouper un remplacement, et plus tard suivre les réponses.

Le corpus réel `reels/chrome-skia-2pages-texte-tableau-svg.pdf` sert
d'épreuve (`tests/text_markup_corpus.rs`) : un mot souligné, un autre
remplacé, relus après enregistrement, et le texte de la page inchangé.

## Les modèles 3D (`three_d`)

Un PDF peut porter un objet en trois dimensions : une annotation `/3D` réserve
un rectangle de la page, et son flux contient le modèle, au format **U3D**
(ECMA-363) ou **PRC** (ISO 14739). Acrux lit l'U3D, le dessine, et le fait
tourner à la souris ; le PRC est reconnu et annoncé comme non lu, plutôt que
deviné.

| Module | Ce qu'il fait |
| --- | --- |
| `three_d/bits` | le **décodeur arithmétique** d'U3D : sans lui, pas un entier n'est lisible dans un bloc |
| `three_d/u3d` | les blocs : nœuds, maillage de base, nuanceurs, matériaux |
| `three_d/draw` | le rendu : projection, tampon de profondeur, lampe frontale |
| `three_d` | le côté PDF : trouver les annotations, lire la vue par défaut (`/3DV`, `/C2W`, `/FOV`, `/BG`) |

Le décodeur arithmétique est la pièce maîtresse : U3D comprime **toutes** les
données de ses blocs avec un codeur adaptatif à contextes (§10 de la norme).
Il est ici écrit dans les deux sens — la lecture pour les fichiers, l'écriture
pour les épreuves — si bien qu'un modèle écrit puis relu doit redonner
exactement le même cube, et qu'un fichier venu d'ailleurs se lit aussi.

Ce qui n'est **pas** lu : le raffinement progressif (un maillage U3D peut être
affiné après son maillage de base), les textures, l'animation, les squelettes,
et le PRC. Le maillage de base est le modèle entier dans la grande majorité
des fichiers.

Le rendu tourne autour de 4 ms par image pour un modèle de cinq mille
triangles en 500 × 900 : manipuler un objet reste fluide sans carte
graphique.

## Lire un fichier image (`stamp::image`)

Un PDF se fabrique souvent à partir d'images : un scan, une photo, une
signature. Les décodeurs sont écrits ici, sans bibliothèque, et rendent tous
la même chose — un `Raster` : des pixels en gris ou en RVB, plus un canal
alpha séparé, prêt à devenir un `/SMask`.

| Format | Ce qui est lu |
| --- | --- |
| JPEG | **incorporé octet pour octet** : jamais décodé, donc jamais dégradé |
| PNG | profondeurs 1 à 16 bits, gris, RVB, palette, alpha (Adam7 refusé) |
| BMP | 1 à 32 bits, palette, `BI_BITFIELDS`, `BI_RLE4`, `BI_RLE8`, lignes dans les deux sens |
| GIF | palette globale ou locale, entrelacement, couleur transparente, LZW de la norme GIF (bit de poids faible en tête) |
| TIFF | **multipage** ; compressions 1, 2, 3, 4, 5, 7, 8, 32773, 32946 ; 1 à 16 bits ; gris, RVB, palette, CMJN ; prédicteur horizontal |

Le TIFF ne fait guère que lire des étiquettes : ses compressions sont celles
des flux PDF, et passent donc par `acrux-codecs` — le Groupe 4 d'un scanner
emprunte le décodeur CCITT qui sert déjà aux images incorporées.

**Un TIFF de plusieurs pages rend plusieurs images** (`decode_all`) : une
feuille numérisée par page, ce qui permet à `create::from_images` d'en faire
un document d'un seul geste.

Les épreuves comparent, au pixel près, chaque format à la version PNG du même
motif, toutes écrites par **GDI+** (`tests/corpus/images/`) : ce sont les
fichiers d'un autre programme, seule façon d'éprouver un décodeur pour de
bon.

## Multimédia (`media`)

Un PDF ne contient pas « une vidéo » : il contient une annotation qui occupe un
rectangle, une action qui dit quoi jouer, et un fichier incorporé quelque part.
Trois générations de spécification se superposent, et ce module les connaît
toutes les trois :

| Forme | Norme | Usage |
|---|---|---|
| `/Screen` + action `/Rendition` | §13.2 | la forme courante depuis Acrobat 6 |
| `/RichMedia` | §13.7 | ce qu'écrit Acrobat récent |
| `/Movie` | PDF 1.1 | obsolète, encore lu |
| `/Sound` | §13.3 | du son seul, sans image |

Le `/Sound` est à part. Il ne désigne pas un fichier mais **des échantillons
bruts** : une fréquence, un nombre de voies, un codage, et les nombres posés
tels quels dans un flux. Pas d'en-tête, pas de conteneur — du son nu, comme sur
un disque des années quatre-vingt. L'extraction lui rend donc un en-tête WAV,
seul moyen d'en faire un fichier que le reste du monde sait ouvrir, en
retournant au passage les octets : un PDF range ses échantillons en
gros-boutien, un WAV en petit-boutien. Les lois µ et A de la téléphonie sont
ramenées à du PCM 16 bits, que tous les lecteurs savent jouer.

Le module **trouve** le média, dit de quoi il s'agit, et sait en extraire les
octets. Il ne décode rien : la lecture est l'affaire de la plateforme.

Un média **extérieur** au document — un chemin sur le disque, une adresse
réseau — est listé comme tel et jamais ouvert de lui-même. Un fichier PDF est
une donnée venue d'ailleurs ; suivre ce qu'il désigne sans rien demander
reviendrait à exécuter ses instructions. De même, le nom du fichier extrait est
choisi par nous et jamais repris du document, qui pourrait appeler sa vidéo
`..{B}..{B}Windows{B}System32{B}quelque-chose.dll`.

```bash
acr media rapport.pdf                      # inventaire
acr media rapport.pdf --extract ./medias   # sort les fichiers incorporés
```

Dans l'application : un clic sur l'affiche lance la lecture.

## Modifier les objets (`edit_objects`)

Le pendant de « Modifier le PDF » d'Acrobat pour tout ce qui n'est pas du
texte : images, dessins vectoriels, groupes, dégradés, et les blocs de texte
pris comme des blocs. Déplacer, redimensionner, pivoter, recadrer, réordonner,
supprimer, remplacer une image.

### La chirurgie, pas la régénération

Un éditeur médiocre relit la page, se fait une idée du contenu, et réécrit un
flux neuf : tout bouge, même ce à quoi on n'a pas touché. Ici, chaque objet est
repéré par sa **plage d'octets** dans le flux décodé. Déplacer un objet, c'est
insérer `q … cm` devant sa plage et `Q` derrière ; le supprimer, c'est retirer
sa plage. Le reste du flux est recopié **octet pour octet**, et les tests le
vérifient des deux côtés : les octets d'avant et d'après la plage éditée sont
identiques, et le rendu hors de la zone déplacée ne change pas d'un pas de
quantification.

### La conjugaison

La matrice demandée est exprimée en **espace page** : « décale de 10 points »
s'écrit `Matrix::translate(10, 0)`, quelle que soit la matrice sous laquelle
l'objet est dessiné. Comme l'objet est tracé sous une matrice `C`, la matrice
réellement insérée est `N = C · M · C⁻¹` — une conjugaison, valable quelle que
soit l'imbrication des `q`/`Q` au-dessus.

Deux détails qui font la différence entre « ça marche » et « c'est propre » :

- un bloc `BT … ET` laisse derrière lui la police et l'interligne, dont un bloc
  suivant peut hériter : on l'encadre donc par `N cm … N⁻¹ cm` plutôt que par
  `q … Q`, qui les reprendrait ;
- une découpe est posée **dans l'espace de l'objet** (les quatre coins du
  rectangle transformés par `C⁻¹`), et non en remettant la matrice à l'identité
  puis en la rétablissant : écrire `C⁻¹` puis `C` avec six décimales chacun ne
  redonne pas exactement `C`, l'image serait rééchantillonnée, et **tous** ses
  pixels changeraient pour un recadrage qui ne devait toucher qu'un bord.

### Ce que l'inventaire sait

| Champ | À quoi il sert |
|---|---|
| `kind`, `name` | image, formulaire, tracé, texte, dégradé ; nom de la ressource |
| `bbox` | boîte en espace page — pour un bloc de texte, la vraie boîte de ses glyphes, appariée **par indice d'opération** avec l'extraction de texte |
| `matrix` | la matrice du tracé, d'où vient la conjugaison |
| `range`, `ops` | les octets et les opérations qui le dessinent |
| `clip` | la découpe en vigueur ; `cut()` dit si elle rogne **cet objet-là**, ce qui n'est pas la même chose que « il y a une découpe » — Chrome et Word en posent une d'office |
| `state` | couleurs et trait à réémettre pour le redessiner ailleurs |

En ligne de commande :

```bash
acr objects contrat.pdf --page 1
acr edit-object contrat.pdf --page 1 --object 3 --move 40,-120 --scale 1.4 -o modifie.pdf
acr edit-object contrat.pdf --page 1 --object 3 --place 300,120,420,200 -o modifie.pdf
acr edit-object contrat.pdf --page 1 --object 3 --crop 178,589,250,685 -o modifie.pdf
acr edit-object contrat.pdf --page 1 --object 3 --order devant -o modifie.pdf
acr edit-object contrat.pdf --page 1 --object 2,4,7 --align gauche -o modifie.pdf
```

Dans l'application : touche `O`. Clic pour sélectionner, glisser pour déplacer,
poignées pour redimensionner (Maj garde les proportions), flèches pour ajuster
au point près, `[` `]` pour l'ordre, Suppr pour retirer.

Tests : `tests/edit_objects_corpus.rs`.

## Lire le texte d'une image (`ocr`)

Un document scanné ne porte pas de texte, seulement des pixels. Ce module les
relit, pour qu'on puisse **modifier** ce texte comme n'importe quel autre.

La chaîne tient en trois temps, et chacun a son fichier :

1. `ocr::image` sépare **l'encre du papier** par le seuil d'Otsu (1979) —
   celui qui rend les deux groupes de pixels les plus distincts possible.
   Aucun réglage à donner : c'est l'histogramme de l'image qui décide. Le
   module garde aussi la **couverture** en demi-teintes, car comparer des
   formes demande le lissage, pas seulement le noir et blanc.
2. `ocr::segment` découpe, par la méthode dite **en croix** : ce que des
   rangées blanches séparent, puis ce que des colonnes blanches séparent, et
   l'on recommence. Un en-tête qui traverse la page se détache ainsi du
   corps, et le corps se sépare alors en colonnes. La gouttière se mesure en
   **largeurs de lettre** — trois lettres séparent deux colonnes, une seule
   sépare deux mots.
3. `ocr::shapes` reconnaît : chaque tache est ramenée à une grille de 16 × 16
   et comparée aux mêmes grilles tirées des **polices installées sur la
   machine**. Deux mesures s'ajoutent à la forme, sans lesquelles « o », « O »
   et « 0 » seraient indiscernables : la proportion de la boîte, et sa place
   par rapport à la ligne de base, en hauteurs d'x.

Deux choix font toute la précision :

- **Les modèles sont dessinés puis relus par le même chemin que l'image** —
  même lissage, même binarisation, même grille. Un contour rasterisé
  directement en seize cases ne ressemble pas à une lettre photographiée puis
  réduite, et l'appariement s'effondre.
- **La segmentation est arbitrée par la reconnaissance.** Deux lettres qui se
  touchent ne font qu'une tache ; aucune mesure géométrique ne les distingue
  d'un « m », qui a lui aussi un creux au milieu. On lit donc la tache
  entière, puis ses deux moitiés, et l'on garde ce qui se lit le mieux.

Rien n'est appris. Le taux de reconnaissance mesuré sur les polices du
système va de 98 % à 100 % dès 28 pixels de corps, 85 % à 20 pixels, 41 % à
14 — c'est la résolution qui commande, comme pour toute reconnaissance. Sur
une vraie page rendue en 300 ppp, l'écart d'édition moyen est de **0,09**.

`ocr::mask` couvre une zone de la page d'un rectangle plein : c'est ce qui
efface le texte d'origine avant d'écrire le nouveau par-dessus. L'image
elle-même n'est jamais touchée.

## Remplir et signer (`fillsign`)

L'outil le plus utilisé d'Acrobat, et le sens courant de « signer un PDF » :
poser sa signature à la main sur une page. Rien à voir avec `signature`, qui
fait de la cryptographie — ici il n'y a aucune preuve, juste un dessin. Les
deux se combinent : on pose la signature manuscrite, puis on scelle avec
`signature::sign`.

| Source | Ce qui est produit |
|---|---|
| tracée au pointeur (`Item::Drawn`) | un **contour rempli** à largeur variable, bouts effilés, virages ronds (`fillsign::ink`) |
| tapée (`Item::Typed`) | une police manuscrite du système, incorporée en sous-ensemble |
| importée (`Item::Image`) | l'image **détourée** : le papier devient transparent (`fillsign::cutout`) |
| texte (`Item::Text`) | du texte libre : nom, date, numéro |
| marque (`Item::Mark`) | coche, croix, rond, trait, point (`fillsign::marks`) |

Chaque élément posé est une annotation avec son apparence, donc déplaçable et
supprimable ; `flatten` les fond définitivement dans le contenu des pages.

### L'encre (`fillsign::ink`)

Le morceau qui fait la différence entre une signature crédible et un gribouillis
raide. Un relevé de pointeur passe par : nettoyage des points redondants,
lissage sans retard (moyenne exponentielle dans les deux sens), largeur déduite
de la **vitesse** — ou de la pression si le stylet en donne —,
ré-échantillonnage à pas constant, effilage des deux bouts, décalage latéral
avec éventail dans les virages serrés, calottes rondes, puis lissage du contour
en cubiques de Bézier. Le résultat est un chemin fermé rempli en règle non
nulle : les boucles des virages très serrés se comblent au lieu de percer.

`Pen::for_extent` proportionne la plume au dessin : un geste tracé dans une
fenêtre de 700 pixels garde la même allure une fois réduit à 170 points.

### Le détourage (`fillsign::cutout`)

Une photo de signature prise au téléphone a un côté plus clair que l'autre et
une ombre dans un coin. Le papier est donc estimé **localement** — maximum de
luminance sur un voisinage plus large que le trait, puis adouci — et chaque
pixel est jugé par rapport à son propre voisinage, pas par rapport à un seuil
global. L'ombre n'est plus prise pour de l'encre. Sortie : une image de base et
son `/SMask`, rognées à ce que l'encre occupe.

En ligne de commande :

```bash
acr fillsign contrat.pdf place --page 1 --rect 330,90,560,175 --draw traits.txt -o signe.pdf
acr fillsign contrat.pdf place --page 1 --rect 60,600,300,660 --typed "Élise Marchand" -o signe.pdf
acr fillsign contrat.pdf place --page 1 --rect 80,300,330,390 --image photo-signature.png -o signe.pdf
acr fillsign contrat.pdf place --page 1 --rect 64,540,116,580 --mark check -o signe.pdf
acr fillsign signe.pdf list
acr fillsign signe.pdf flatten -o definitif.pdf
```

Dans l'application : touche `S`, ou « Remplir et signer » dans la palette.

Tests : `tests/fillsign_corpus.rs` (géométrie de l'encre, pose et retrait au
pixel près sur tout le corpus, aplatissement qui ne déplace rien, détourage).

## Chercher dans le texte (`text::search`)

Un seul moteur, [`text::find_matches`], sert la carte de recherche de
l'application, le remplacement (`edit_text::find_ranges_with`, que « Tout
remplacer » réécrit) et la commande `acr find` : ce qui est surligné est ce
qui sera remplacé, dans le même ordre.

| Ce qu'il sait | Comment |
| --- | --- |
| casse, mot entier | `SearchOptions { match_case, whole_word }` ; la borne de mot ne compte que du côté d'une lettre (« (art » se trouve collé) |
| passer à la ligne | le texte d'un **paragraphe** est parcouru d'un tenant : une fin de ligne vaut une espace, une césure se referme avec la règle du texte des paragraphes (`layout::is_hyphen_break`) ; rien ne se joint d'un paragraphe ou d'une cellule à l'autre |
| comparer comme on lit | repli **un pour un** : blancs (insécable, fine) → espace, apostrophe et guillemets typographiques → droits, minuscule si la casse est libre ; ligatures (« ﬁ ») dépliées, chaque lettre gardant son glyphe |
| désigner l'occurrence | un `MatchPiece` par ligne traversée : ligne, glyphes (comptés comme `TextRange`), boîte ; une occurrence d'un seul morceau est éditable |

Les occurrences ne se chevauchent pas (« aa » deux fois dans « aaaa ») : les
éditions de « Tout remplacer » ne peuvent pas se recouvrir. Une occurrence à
cheval sur deux lignes n'est pas éditable (une édition réécrit une ligne) :
elle est surlignée et comptée, mais `find_ranges` l'omet. Les épreuves :
les tests du module, `tests/search_corpus.rs` sur tout le corpus, et
`acrux-app/tests/replace_corpus.rs` pour le remplacement.

## Export et conversion (`export`)

Le pendant des « Exporter vers » d'Acrobat, construit sur le moteur de rendu
(`acrux-render`) et sur l'extraction de texte structurée (`text`) :

| Appel | Sortie |
|---|---|
| `export_pages_png(doc, pages, dpi, sink)` | une image PNG par page |
| `export_pages_jpeg(doc, pages, dpi, qualité, 4:2:0, sink)` | une image JPEG par page (`acrux_codecs::dct::encode`) |
| `extract_images(doc)` | les images incorporées : les `DCTDecode` octet pour octet, le reste en PNG (alpha si `/SMask`) |
| `export_html(doc, &HtmlOptions)` | un HTML autonome, texte positionné (`Absolute`) ou reflué (`Flow`) |
| `export_docx(doc)` | un document Word (WordprocessingML) |
| `export_tables_xlsx(doc)` | un classeur Excel des tableaux détectés |
| `export_text` / `export_markdown` | via `PageText::to_plain` / `to_markdown` |

`zip` fournit le conteneur des `.docx` et `.xlsx` : écrivain (en-têtes locaux,
répertoire central, deflate via `acrux_codecs::flate::compress`, CRC-32) et
lecteur minimal pour les relire.

En ligne de commande :

```bash
acr export fichier.pdf --format html -o page.html
acr export fichier.pdf --format jpeg --dpi 200 --quality 90 --pages 1-3 -o page.jpg
acr export fichier.pdf --format docx -o document.docx
```

Tests : `cargo test -p acrux-features` (unitaires par module, plus
`tests/export_corpus.rs` et `tests/text_corpus.rs` sur les PDF réels de
`tests/corpus/reels/`).
