# acrux-fonts

Parseurs de polices écrits de zéro, sans dépendance externe (uniquement
`acrux-core` et la bibliothèque standard). Le crate transforme les programmes de
police incorporés dans les PDF (ISO 32000-2 §9.9) en contours vectoriels
(`acrux_core::Path`, unités de police) et en métriques ; la rasterisation est
faite par `acrux-graphics`, l'interprétation des opérateurs de texte par
`acrux-render`.

Voir `ARCHITECTURE.md` à la racine pour la place de ce crate dans l'ensemble.

## Ce qui est couvert

| Module | Contenu | Spécification |
|---|---|---|
| `reader` | Lecteur d'octets big-endian borné (u8/u16/i16/u24/u32/i32, Fixed, F2Dot14, offsets 1-4 octets, tags). Aucune lecture ne panique. | OpenType « Data types » |
| `glyph` | Trait `GlyphProvider` (`glyph_path`, `advance`, `units_per_em`, `glyph_count`) implémenté par les trois programmes de police. | — |
| `truetype` | `.ttf` (0x00010000, `true`), `.ttc` (index de police), OpenType `OTTO` (délègue à `cff`). Tables `head`, `maxp`, `loca`, `glyf` (glyphes simples avec points implicites → `quad_to` ; composites : tous les drapeaux d'arguments, d'échelle et de matrice 2×2, appariement de points, profondeur ≤ 8, budget de composants), `hhea`/`hmtx`, `cmap` formats 0, 4, 6, 12 pour toute (plateforme, encodage), `post` 1.0/2.0 avec les 258 noms Macintosh, `OS/2`. | OpenType (Microsoft) |
| `cff` | CFF nu (`FontFile3` /Type1C, /CIDFontType0C) et table `CFF `. En-tête, INDEX, DICT (entiers, réels en quartets), Top DICT, Private DICT, charsets formats 0/1/2 et prédéfinis (ISOAdobe, Expert, ExpertSubset), encodages standard et personnalisés (formats 0/1 + suppléments), CID-keyed (ROS, FDArray, FDSelect formats 0/3). Charstrings Type 2 complets : tracé, courbes, flex (`flex`, `hflex`, `hflex1`, `flex1`), hints et masques (`hintmask`/`cntrmask` avec comptage des stems), sous-routines locales/globales avec biais, largeur, `endchar` avec `seac`, opérateurs arithmétiques et de pile. Les 391 chaînes standard sont incluses. | Adobe TN #5176, #5177 |
| `type1` | PFA (binaire ou hexadécimal), PFB, `FontFile`. Partie claire (`/FontName`, `/FontMatrix`, `/Encoding`), déchiffrement eexec (r = 55665) et charstrings (r = 4330, `/lenIV`), `/Subrs`, `/CharStrings`. Charstrings Type 1 complets : `hsbw`, `sbw`, tracé, courbes, `closepath`, `callsubr`, `div`, `seac`, `callothersubr`/`pop` (flex OtherSubrs 0-2 → deux courbes, hint replacement OtherSubrs 3 ignoré, contrôle de compteurs 12-13 ignoré, OtherSubrs inconnus rendus aux `pop`), `setcurrentpoint`. Les fonctions `encrypt`/`decrypt` sont publiques (utiles aux tests et à l'écriture future). | Adobe Type 1 Font Format |
| `encodings` | StandardEncoding, WinAnsiEncoding (notes 3, 5, 6 de l'annexe D), MacRomanEncoding (complété par les symboles Mac OS Roman), MacExpertEncoding, PDFDocEncoding (code → nom), encodages intégrés de Symbol et ZapfDingbats ; `BaseEncoding` ; `glyph_name_to_unicode` (AGL réduite à environ 900 noms courants + règles `uniXXXX`, `uXXXX[XX]`, suffixes `.sc`/`.alt`, ligatures `a_b`, noms d'index `gNN`/`cidNN` → `None`). | ISO 32000-2 annexe D, AGL Specification |
| `cmap` | CMaps incorporées (`begincodespacerange`, `begincidrange`, `begincidchar`, `usecmap` vers Identity, `/WMode`, `/CMapName`), `Identity-H`/`Identity-V`, découpage des chaînes en codes de 1 à 4 octets selon §9.7.6.3 (y compris codes invalides et chaînes tronquées), ToUnicode (`beginbfchar`, `beginbfrange` avec destination unique ou tableau, destinations UTF-16BE avec paires de substitution). | ISO 32000-2 §9.7.5, §9.7.6, §9.10.3 ; Adobe TN #5014 |

Garanties communes : pas de `unsafe`, pas de `unwrap`/`expect`/`panic` hors
tests, toutes les lectures bornées, récursions (composites, sous-routines,
`seac`) limitées en profondeur, budget d'opérations par glyphe, boucles
bornées par la taille des données. Les polices tronquées (cas fréquent des
sous-ensembles incorporés) donnent des glyphes vides et des tables absentes,
jamais une erreur fatale.

## Les quatorze polices standard (`standard`)

Un PDF a le droit d'écrire ceci, et c'est valide :

```
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>
```

Pas de largeurs, pas de descripteur, pas de programme de police. Le lecteur est
censé **connaître** Helvetica. Ce module est cette connaissance : les largeurs
des fichiers AFM d'Adobe, en millièmes d'em, plus les grandeurs du descripteur.

Sans elles, la seule issue est de mesurer une police du système — et le même
fichier ne se compose plus pareil d'une machine à l'autre, quand il se compose.
Sur une machine sans police du tout, les largeurs tombent à zéro et les lettres
s'empilent au même endroit.

Trois détails qui comptent :

- les accentués prennent l'avance de leur lettre de base, parce que ces polices
  les construisent par superposition — mais le `i` accentué se bâtit sur le `i`
  **sans point**, plus large (278 contre 222) ;
- `Arial`, `TimesNewRoman` et `CourierNew` sont reconnus comme Helvetica, Times
  et Courier : les fonderies les ont dessinées pour être interchangeables, et
  d'innombrables fichiers écrivent `Arial` sans rien incorporer ;
- Symbol porte **son propre encodage** : au code 0x61 il y a `alpha`, pas `a`.

Les tables sont recoupées à chaque exécution des tests avec les polices
métriquement compatibles installées sur la machine (`tests/standard_metrics.rs`).
Ce juge extérieur a déjà rattrapé trois erreurs et comblé huit manques.

Ce qui manque : les largeurs de ZapfDingbats. Les inventer serait pire que
l'absence — `width_by_name` rend `None`, et l'appelant sait qu'il doit se
rabattre ailleurs.

## Les noms d'une police (`names`)

La table `name` d'une police OpenType dit sous quels noms elle se présente :
famille, sous-famille, nom complet, nom PostScript. C'est par là qu'un lecteur
retrouve une police installée, et c'est le seul lien fiable entre ce qu'un PDF
demande (`/BaseFont /Calibri-Bold`) et ce qu'il y a sur le disque.

Deviner le nom du fichier ne marche que sur Windows : macOS empile ses polices
dans des recueils `.ttc` aux noms arbitraires, Linux les éparpille par famille,
et rien ne garantit qu'un fichier nommé `arial.ttf` contienne Arial.

Les identifiants 16 et 17 priment sur 1 et 2 quand ils existent : ils décrivent
la famille telle que le dessinateur la conçoit, là où 1 et 2 sont contraints par
le vieux modèle « régulier, gras, italique, gras italique » de Windows. C'est ce
qui fait que « Segoe UI Semibold » se range sous « Segoe UI » et non dans une
famille à part.

Une table abîmée ne rend pas d'erreur : elle donne moins de noms. Un index se
construit sur des centaines de fichiers, dont certains sont cassés ; en refuser
un ne doit pas coûter les autres.

## La police de secours (`fallback`)

**Dernier recours seulement.** Tant que la machine a de vraies polices, c'est
l'une d'elles qui sert, jusqu'à prendre n'importe laquelle ayant le bon
caractère : une vraie police mal choisie vaut mieux qu'une lettre dessinée par
nous. Mais sur une machine sans une seule police installée — un conteneur, une
image de compilation minimale — il n'y a rien à substituer, et la page reste
blanche alors que le fichier est parfaitement valide. Acrobat embarque pour ce cas
Adobe Sans MM. Voici la nôtre.

Chaque lettre est décrite comme on la tracerait à la plume, puis épaissie :

```
("H", 722.0, "M 140 0 L 140 700 M 582 0 L 582 700 M 140 350 L 582 350"),
("O", 778.0, "A 389 350 320 350 0 360"),
```

| Commande | Effet |
|---|---|
| `M x y` | commence un trait |
| `L x y` | segment droit |
| `C x1 y1 x2 y2 x y` | cubique de Bézier |
| `A cx cy rx ry a0 a1` | arc d'ellipse, angles en degrés |
| `. x y` | point (celui du `i`, celui du `?`) |

Repères, en millièmes d'em : ligne de base 0, hauteur d'œil 500, capitale 700,
hampes 730, jambages −210, plume 80.

Le squelette a un avantage qui n'est pas la brièveté : **il ne peut pas se
tromper de sens**. Un contour rempli doit tourner dans le bon sens et ses
contre-formes dans l'autre ; une erreur et la lettre se remplit à l'envers.
Ici tous les morceaux engendrés tournent pareil, le remplissage non nul les
réunit, et corriger un `g` revient à déplacer un nombre et à regarder.

La chasse est ensuite ajustée à celle que le document déclare, si bien que les
lignes tombent juste même quand les lettres ne sont pas les bonnes.

Couverture : tout WinAnsiEncoding, accents compris (composés à la volée sur
leur lettre). Pas le grec de Symbol ni les fleurons de ZapfDingbats — ceux-là
restent vides plutôt que de sortir une lettre latine à leur place.

## Ce qui n'est pas encore couvert

- **Hinting** : instructions TrueType (`fpgm`, `prep`, `cvt `, instructions
  de `glyf`) et hints Type 1 / Type 2 sont lus et ignorés ; le rendu compte
  sur l'anti-aliasing.
- **GSUB / GPOS / shaping** : ligatures, crénage, écritures complexes,
  texte bidirectionnel (module `shaping` à venir).
- **Variations** (`fvar`, `gvar`, CFF2) : non lues.
- **Polices bitmap** (`EBDT`/`EBLC`, `CBDT`, `sbix`) : non lues.
- **CMaps CJK prédéfinies** (UniGB-UCS2-H, 90ms-RKSJ-H…) : `CMap::predefined`
  ne connaît que `Identity-H` et `Identity-V` ; `usecmap` vers une autre
  CMap prédéfinie est ignoré.
- **Type 3** : les glyphes définis par des flux de contenu sont traités dans
  `acrux-render`, qui possède l'interpréteur de contenu.
- **Substitution** de polices non incorporées (14 polices standard et
  polices de secours métriquement compatibles) : module `substitution` à venir.
- CFF : l'Expert Encoding prédéfini (annexe B de TN #5176) n'est pas fourni
  (`encoding_code_to_gid` renvoie `None`, passer par les noms) ; la
  FontMatrix d'un Font DICT n'est pas composée avec celle du Top DICT ;
  les charstrings de type inconnu donnent des glyphes vides.
- TrueType : `cmap` formats 2, 8, 10, 13, 14 non lus ; `kern`, `name`,
  `vhea`/`vmtx` (écriture verticale) non lus.
- AGL : liste réduite ; les noms de ZapfDingbats (`a1`…`a191`) et les
  écritures hors latin/grec/cyrillique de base ne sont pas mappés.

## Tests

```
cargo test -p acrux-fonts
```

Aucun fichier de police n'est nécessaire : chaque module fabrique ses
polices de test dans le code (encodeur de tables TrueType, encodeur
INDEX/DICT CFF, chiffrement eexec/charstring Type 1). Chaque parseur a un
test qui tronque la police de test à toutes les longueurs et un test sur des
octets pseudo-aléatoires, pour vérifier l'absence de panique.
