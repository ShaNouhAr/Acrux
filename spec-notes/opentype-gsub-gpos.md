# Composition typographique : GSUB, GPOS, GDEF, kern (OpenType Layout)

## En deux phrases
Poser du texte, ce n'est pas dessiner un glyphe par caractère : il faut remplacer `f` + `i` par
la ligature `fi`, choisir la forme initiale d'une lettre arabe, rapprocher le `V` du `A`, poser
l'accent au bon endroit, et réordonner ce qui se lit de droite à gauche.
Les tables `GSUB` (substitutions) et `GPOS` (positions) d'une police décrivent tout cela ;
`acrux-fonts` les lit (`opentype/`) et les applique (`shape.rs`), avec les données Unicode
nécessaires écrites à la main dans `unicode/`.

## Le pipeline

```
texte (String)
  │  unicode::bidi::reorder_visual      ordre visuel : passages de niveau pair/impair
  ▼
passages (Run { start, end, level })
  │  unicode::grapheme::cluster_boundaries    grappes → numéros de grappe (octets)
  │  unicode::ccc (tri canonique des marques)
  │  cmap (caractère → glyphe)
  │  unicode::joining (init/medi/fina/isol → masques)
  ▼
tampon de GlyphInfo { gid, cluster, mask, lig_id, avances, décalages }
  │  GSUB : chaque table de recherche du plan, dans l'ordre des indices
  ▼
tampon substitué (ligatures, formes contextuelles, décompositions)
  │  avances de hmtx (ou vmtx en vertical)
  │  GPOS : crénage, ancrages, cursif
  │  resolve_attachments : décalages relatifs → absolus
  │  inversion du passage si droite à gauche
  ▼
Vec<ShapedGlyph { gid, cluster, advance, offset_x, offset_y }>
```

## Le format

### Squelette commun (`opentype/common.rs`)

`GSUB` et `GPOS` ont **exactement** le même en-tête :

```text
uint16 majorVersion (1)     uint16 minorVersion (0 ou 1)
Offset16 scriptListOffset   Offset16 featureListOffset   Offset16 lookupListOffset
[Offset32 featureVariationsOffset]        (version 1.1, non lu : polices variables)
```

Trois listes chaînées :

- **ScriptList** → `Script` (`latn`, `arab`, `hebr`, `DFLT`…) → `LangSys` (`FRA `, `TRK `, ou
  le `LangSys` par défaut) → indices de fonctionnalités, plus un `requiredFeatureIndex`.
- **FeatureList** → `Feature` (tag `liga`, `kern`, `init`…) → indices de tables de recherche.
  Le même tag apparaît plusieurs fois : une entrée par (script, langue). Dans Arial, le tag
  `medi` occupe les indices 67 à 71, tous pointant sur la table 13.
- **LookupList** → `Lookup { lookupType, lookupFlag, subTableCount, offsets[], markFilteringSet }`.

Deux structures reviennent partout :

- **Coverage** : l'ensemble des glyphes concernés et leur **rang**. Format 1 = liste triée de
  GID ; format 2 = plages triées `(startGlyphID, endGlyphID, startCoverageIndex)`. Le rang sert
  d'indice dans les tableaux parallèles des sous-tables.
- **ClassDef** : glyphe → classe (0 par défaut). Format 1 = plage contiguë ; format 2 = plages.
  C'est ce qui rend le crénage compact : 1 200 glyphes se rangent en 80 classes.

Exemple d'octets réels, la couverture de la table `fina` d'Arial :

```text
00 02        format 2
00 DB        219 plages
03 08 03 08 00 00     glyphe 776 seul       → rang 0
03 1F 03 1F 00 01     glyphe 799 seul       → rang 1
…
03 8D 03 8D 00 0C     glyphe 909 (alef)     → rang 12
03 8F 03 8F 00 0D     glyphe 911 (beh)      → rang 13
```

et le tableau des substituts, lu à l'offset 6 : `subst[12] = 910`, donc l'alef isolé (909)
devient l'alef final (910).

### GSUB (`opentype/gsub.rs`)

| Type | Nom                           | Formats | Ce qu'on en fait |
|------|-------------------------------|---------|------------------|
| 1    | Substitution simple           | 1, 2    | un glyphe → un glyphe (formes arabes, petites capitales) |
| 2    | Substitution multiple         | 1       | un glyphe → plusieurs (décomposition) |
| 3    | Substitution alternative      | 1       | on prend **la première** variante (voir Refusé) |
| 4    | Substitution de ligature      | 1       | `f` + `f` + `i` → `ffi` |
| 5    | Contextuelle                  | 1, 2, 3 | un motif déclenche d'autres tables |
| 6    | Contextuelle enchaînée        | 1, 2, 3 | idem, avec contexte avant et arrière |
| 7    | Extension                     | 1       | résolue à l'analyse, invisible ensuite |
| 8    | Contextuelle inverse simple   | 1       | appliquée de la fin vers le début |

### GPOS (`opentype/gpos.rs`)

| Type | Nom                           | Formats | Ce qu'on en fait |
|------|-------------------------------|---------|------------------|
| 1    | Ajustement simple             | 1, 2    | décalage ou avance d'un glyphe |
| 2    | Ajustement de paire           | 1, 2    | **le crénage** : paires explicites (1) ou matrice de classes (2) |
| 3    | Attachement cursif            | 1       | ancre de sortie ↔ ancre d'entrée (horizontal) |
| 4    | Marque vers base              | 1       | l'accent sur la lettre |
| 5    | Marque vers ligature          | 1       | l'accent sur **le bon composant** de la ligature |
| 6    | Marque vers marque            | 1       | l'empilement des diacritiques |
| 7, 8 | Contextuelle (enchaînée)      | 1, 2, 3 | même reconnaissance que GSUB 5 et 6 |
| 9    | Extension                     | 1       | résolue à l'analyse |

Un `ValueRecord` n'a que les champs que son `valueFormat` déclare : deux octets par bit à 1.
Un crénage latin typique vaut `valueFormat1 = 0x0004` (XAdvance seul), `valueFormat2 = 0`, soit
deux octets par paire.

### GDEF (`opentype/common.rs`)

Classe de chaque glyphe (1 base, 2 ligature, 3 marque, 4 composant), classe d'attachement de
marque, jeux de marques filtrants. C'est elle qui donne son sens à `lookupFlag` : « ignorer les
marques », « ignorer les bases », « ne traiter que les marques du jeu *n* ». Sans `GDEF`, rien
n'est sauté — le comportement sûr.

### kern (`opentype/kern.rs`)

La table d'avant `GPOS`. Nous ne lisons que le **format 0** (liste de paires triée), en-tête
Microsoft (`version` sur 16 bits) ou Apple (`version` en 16.16). Elle ne sert qu'en **repli**,
quand `GPOS` n'offre pas de fonctionnalité `kern` pour le script demandé. Vérifié : sur Arial,
`kern` et `GPOS` donnent la même valeur pour `AV` (−152) et `To` (−227).

### vhea / vmtx / VORG (`opentype/vertical.rs`)

Avance verticale et origine verticale par glyphe, avec la même compression que `hmtx`. En mode
vertical, le shaper active `vert` (ou `vrt2` si la police l'a), prend l'avance de `vmtx` et
décale le dessin de `(−avance_horizontale / 2, −origine_verticale)`.

## L'application (`shape.rs`)

Un **plan** est construit pour (script, langue) : chaque fonctionnalité demandée donne ses
tables de recherche, chacune avec un **masque**. Les tables sont ensuite appliquées dans
l'ordre de leur indice, comme l'exige la spécification, et ne touchent que les glyphes dont le
masque recouvre le leur.

Le masque, c'est ce qui permet à `init` de ne s'appliquer qu'à la première lettre d'un mot
arabe et à `fina` qu'à la dernière : les fonctionnalités globales (`liga`, `kern`, `ccmp`…)
prennent le bit 0, que tous les glyphes portent ; `isol`, `init`, `medi` et `fina` prennent un
bit chacune, posé par `unicode::joining::joining_forms`.

Les marques sont d'abord **triées canoniquement** dans leur grappe (classes combinatoires
croissantes) : une police qui ancre une chadda puis une fatha ne reconnaîtrait pas l'ordre
inverse.

## Les pièges rencontrés dans les polices réelles

- **Arial confond l'initiale et la médiane du beh.** Les tables `init` (14) et `medi` (13)
  envoient toutes deux le glyphe 911 sur le glyphe 913. Ce n'est pas un défaut : les contours
  de `uniFE91` (initiale) et `uniFE92` (médiane) sont **identiques octet pour octet**, et la
  police n'en garde qu'un. Times New Roman, Tahoma et Calibri, eux, donnent bien quatre formes
  distinctes. Le test `arial_shares_initial_and_medial_beh` documente le cas pour qu'on ne le
  prenne pas pour un bogue du compositeur.
- **Georgia ne crène presque pas le latin en `GPOS`.** Son unique table de paires ne contient,
  pour `A`, que des seconds glyphes au-delà du rang 660 (accentués et cyrilliques). « AV » n'est
  donc pas resserré — et c'est la police, pas nous. Les tests de crénage utilisent Arial, Times,
  Calibri, Cambria, Segoe UI et Tahoma.
- **Georgia range `fi` dans `dlig`**, pas dans `liga` : la ligature est *discrétionnaire* et ne
  doit apparaître que si on la demande. Calibri, elle, la met dans `liga`.
- **Le même tag de fonctionnalité apparaît des dizaines de fois** dans la `FeatureList` (une
  entrée par système de langue). Il faut passer par les indices du `LangSys`, jamais chercher un
  tag dans la liste.
- **Les sous-tables d'extension** (GSUB 7, GPOS 9) sont la règle et non l'exception dans les
  polices Microsoft : elles sont résolues à l'analyse, une fois pour toutes.
- **Offsets relatifs à des bases différentes** : une couverture est relative à sa sous-table,
  une ancre de `MarkArray` à son tableau, un `LangSys` à sa table `Script`. Une seule confusion
  et l'on lit du bruit — ce qui, chez nous, donne `None` et non une panique.

## Ce que nous refusons, et pourquoi

- **Les écritures syllabiques** : indiennes (devanagari, bengali, tamoul, télougou…), khmère,
  birmane, thaïe, tibétaine. Elles demandent un moteur qui réordonne les voyelles autour de la
  consonne, découpe en syllabes et applique les fonctionnalités par position dans la syllabe.
  Nous ne l'écrivons pas. Leur texte est composé « à plat » : les substitutions et le crénage de
  la police s'appliquent, mais l'ordre des voyelles n'est pas corrigé. C'est lisible pour du
  texte simple et faux pour du texte complexe ; il vaut mieux le dire que le promettre.
- **Le syriaque** (logique de l'alaph) et le **mongol** : traités comme non cursifs, donc rendus
  en formes isolées.
- **La règle N0 de l'algorithme bidirectionnel** (paires de parenthèses appariées,
  `BidiBrackets.txt`) : une parenthèse dans un passage de droite à gauche est traitée par N1 et
  N2 comme un neutre ordinaire.
- **Les tables `Device`** (corrections par taille de rendu) et l'`ItemVariationStore` des
  polices variables : lues et sautées. Nous rendons sans hinting, avec de l'anti-aliasing ; ces
  corrections n'auraient aucun effet.
- **La variante choisie par le type GSUB 3** : la spécification laisse l'application décider
  (c'est le menu « variantes de glyphe » d'un traitement de texte). Faute d'interface, nous
  prenons systématiquement la première, qui est la variante par défaut.
- **Les formats 1, 2 et 3 de la table `kern`** (machines à états Apple) : rarissimes, et le
  format 2 est de toute façon supplanté par `GPOS` type 2 format 2.
- **`AttachList`, `LigCaretList`** de `GDEF` : la première sert au hinting, la seconde au
  placement du curseur dans une ligature — que nous faisons par les grappes.

## Où c'est implémenté

| Fichier | Rôle |
|---|---|
| `acrux_fonts::opentype::common` | `Coverage`, `ClassDef`, `ValueRecord`, `Anchor`, `LayoutTable`, `Lookup`, `Gdef` |
| `acrux_fonts::opentype::buffer` | `GlyphInfo`, `SkipList` (saut des glyphes ignorés) |
| `acrux_fonts::opentype::context` | motifs contextuels et enchaînés, formats 1 à 3 |
| `acrux_fonts::opentype::gsub` | `Gsub::parse`, `Gsub::apply` |
| `acrux_fonts::opentype::gpos` | `Gpos::parse`, `Gpos::apply`, `resolve_attachments` |
| `acrux_fonts::opentype::kern` | `KernTable::parse`, `KernTable::kerning` |
| `acrux_fonts::opentype::vertical` | `VerticalMetrics::read` |
| `acrux_fonts::unicode::bidi` | UAX #9, `reorder_visual` |
| `acrux_fonts::unicode::grapheme` | UAX #29, `cluster_boundaries` |
| `acrux_fonts::unicode::joining` | `joining_forms` (init/medi/fina/isol) |
| `acrux_fonts::unicode::ccc` | classes combinatoires, tri canonique |
| `acrux_fonts::shape` | `Shaper`, `shape`, `ShapeOptions`, `ShapedGlyph` |
| `acrux_features::fontembed` | `EmbeddedFont::shape_line`, `shaped_width` |

## Les tests qui le couvrent

- `crates/acrux-fonts/src/opentype/*.rs` : sous-tables fabriquées à la main, un test par format,
  plus un test « une sous-table corrompue ne panique pas » par module.
- `crates/acrux-fonts/src/unicode/*.rs` : tables triées, formes de jointure, grappes, et une
  dizaine de cas d'UAX #9 (forçages, isolats, chiffres arabes, blancs de fin).
- `crates/acrux-fonts/tests/shaping_system_fonts.rs` : **vraies polices de la machine** —
  ligatures de Calibri, `dlig` de Georgia, crénage de six polices, quatre formes arabes,
  marques ancrées, bidi latin/hébreu, CJC vertical, non-régression de la composition nue, et un
  test qui compose des textes hostiles sans paniquer. Chaque test se termine par un `return`
  silencieux si la police manque.
- `crates/acrux-features/tests/shaping_visual.rs` : fabrique le PDF de démonstration qui écrit
  chaque ligne deux fois, brute puis composée.
