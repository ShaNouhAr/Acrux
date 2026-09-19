# acrux-features

Fonctionnalités métier : édition, pages, annotations, formulaires, biffure, export, sécurité…

Voir `ARCHITECTURE.md` à la racine pour la place de ce crate dans l'ensemble,
et `src/lib.rs` pour la liste des modules et de ceux qui restent à écrire.

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
