# Architecture cible d'Acrux

Application native, monolithique mais découpée en modules indépendants et testables.
Chaque module correspond à un chapitre de la spécification PDF (ISO 32000-2) ou à une
brique technique clairement délimitée. Un contributeur doit pouvoir travailler sur un module
sans comprendre les autres.

## 1. Langage : Rust (décidé le 18/09/2026 ; tableau conservé pour mémoire)

| Critère | Rust (recommandé) | C++ | C# (.NET AOT) |
|---|---|---|---|
| Exécutable natif unique, sans runtime | Oui | Oui | Oui (Native AOT) |
| Sécurité mémoire face à des PDF malveillants | Garantie par le compilateur | À la charge du développeur | Garantie |
| Performance rasteriseur / décodeurs | Niveau C | Niveau C | Bonne, un cran en dessous |
| Facilité d'accès pour un contributeur débutant | Moyenne (courbe d'apprentissage) | Faible | Élevée |
| Outillage (build, tests, formatage en une commande) | Excellent (`cargo`) | Hétérogène | Bon |
| Multiplateforme Windows / macOS / Linux | Natif | Natif | Bon, GUI cross-platform moins mature |

**Recommandation : Rust.** Un moteur PDF écrit de zéro va parser des fichiers hostiles ; la
sécurité mémoire n'est pas négociable. La courbe d'apprentissage est compensée par une
structure de modules très simple et une documentation abondante.
Alternative acceptable si l'on privilégie l'accessibilité aux débutants : C# avec Native AOT.

## 2. Vue d'ensemble des couches

```
┌──────────────────────────────────────────────────────────────────┐
│  app/           Interface utilisateur native (fenêtres, outils,  │
│                 panneaux, raccourcis, préférences, thèmes)         │
├──────────────────────────────────────────────────────────────────┤
│  features/      Fonctionnalités métier : édition, organisation   │
│                 de pages, formulaires, annotations, signatures,   │
│                 biffure, accessibilité, comparaison, OCR, IA      │
├──────────────────────────────────────────────────────────────────┤
│  render/        Moteur de rendu : interprète le contenu de page   │
│                 et dessine (chemins, texte, images, ombrages,     │
│                 transparence) via le rasteriseur                  │
├──────────────────────────────────────────────────────────────────┤
│  graphics/      Rasteriseur 2D générique (anti-aliasing, clip,    │
│                 dégradés, blend modes), indépendant du PDF        │
├──────────────────────────────────────────────────────────────────┤
│  fonts/         Parseurs et rasteriseurs TrueType, CFF/Type2,     │
│                 Type1, OpenType, Type3 ; shaping ; encodages CMap  │
├──────────────────────────────────────────────────────────────────┤
│  codecs/        Flate, LZW, RunLength, ASCII85/Hex, CCITT G3/G4,  │
│                 DCT (JPEG), JPX (JPEG 2000), JBIG2                 │
├──────────────────────────────────────────────────────────────────┤
│  document/      Modèle objet PDF : objets, xref, flux, pages,     │
│                 ressources, écriture incrémentale, réparation     │
├──────────────────────────────────────────────────────────────────┤
│  core/          Types de base, erreurs, arène mémoire, logs,      │
│                 utilitaires, chargement paresseux (mmap)          │
└──────────────────────────────────────────────────────────────────┘
        ↕ dépendances toujours vers le bas, jamais vers le haut
```

## 3. Détail des modules

### core/ (`acrux-core`) — fait
- `geom` : `Point`, `Rect`, `Matrix` (convention PDF `[a b c d e f]`) ; `path` : `Path`
  partagé par les polices, le rasteriseur et l'interpréteur (quadratiques élevées en cubiques,
  aplatissement adaptatif) ; `error` : `Error` typée, jamais de panique.

### document/ (`acrux-document`) — fait, ISO 32000-2 chapitre 7
- `lexer`, `parser`, `objects`, `writer` : syntaxe complète, tolérante aux fichiers cassés.
- `xref` (tables, flux, hybrides, `/Prev`), `repair` (reconstruction par balayage, flux
  d'objets dépliés), `document` (cache, flux d'objets, réparation automatique, offsets décalés).
- `filters` (chaînage `/Filter` + `/DecodeParms`, délègue à `acrux-codecs`).
- `crypt` : gestionnaire standard R2 à R6, RC4, AES-128/256, MD5, SHA-2 ; mots de passe
  utilisateur et propriétaire. Depuis les signatures, ce module abrite aussi `bigint`
  (entiers de grande taille, multiplication de Montgomery, échelle à temps constant pour les
  exposants secrets), `rsa` (EMSA, `RSASSA-PKCS1-v1_5` en signature et vérification,
  `RSASSA-PSS` en vérification, MGF1, lecture PKCS#1 / PKCS#8 / SPKI, génération de clés par
  Miller-Rabin) et `sha1` (marqué obsolète, jamais employé pour signer).
- `asn1` : codage DER **strict** (longueurs définies et minimales, `SET OF` trié, booléens
  canoniques) — lecteur et écrivain, base de X.509, du CMS, de PKCS#1 et de PKCS#8. La forme
  BER à longueur indéfinie est refusée : deux analyseurs qui lisent deux structures dans les
  mêmes octets, c'est la porte d'entrée des contournements de signature.
- `edit` : modifications (`set`/`add`/`delete`), enregistrement incrémental (fichier d'origine
  intact) et complet, rechiffrement.
- `pages` (arbre, héritage, repli par balayage), `text` (chaînes UTF-16/PDFDoc).
- À faire : `metadata` (XMP §14.3 ; en attendant, `acrux-features/preflight` écrit un paquet XMP
  minimal). Le balisage §14.7 est lu et écrit par `acrux-features/accessibility`, au-dessus de ce crate.

### codecs/ (`acrux-codecs`)
- Faits : `flate` (RFC 1950/1951), `lzw`, `ascii` (Hex, 85), `runlength`, `predictor`
  (PNG, TIFF), `dct` (JPEG baseline et progressif, T.81) ; en cours : `ccitt`, `jbig2`, `jpx`.
- Encodeurs : `flate::compress` (DEFLATE, Huffman dynamique) et `dct::encode`
  (JPEG de base : DCT 8 × 8, tables de quantification et de Huffman de l'annexe K
  mises à l'échelle par la qualité, 4:2:0 optionnel), relus par nos propres décodeurs.
- API uniforme `apply_filter(nom, données, DecodeParms)` ; les filtres d'image renvoient une
  image décodée (`JpegImage`…) consommée par `acrux-render::image`.

### fonts/ (`acrux-fonts`) — fait
- `truetype` (glyf/loca/cmap/hmtx/post, composites, TTC, OTTO), `cff` (charstrings Type 2,
  CID-keyed, charsets, encodages, 391 chaînes standard), `type1` (PFA/PFB, eexec,
  charstrings Type 1, flex, seac), `encodings` (annexe D + liste Adobe réduite), `cmap`
  (CMaps incorporées, Identity, ToUnicode), `glyph` (trait `GlyphProvider`).
- `subset` : production d'une police TrueType valide contenant un ensemble de glyphes choisi
  (tables `head hhea maxp hmtx loca glyf cmap name post` recalculées, descriptions `glyf`
  recopiées octet pour octet, composantes des glyphes composites renumérotées, fusion de
  plusieurs polices sources) ; sert au complément de police de l'édition de texte.
- `opentype` : `GSUB` (substitution simple, multiple, alternative, ligature, contextuelle et
  contextuelle enchaînée formats 1 à 3, extensions, inverse), `GPOS` (ajustement simple, paires
  — le crénage — formats 1 et 2, cursif, marque vers base / ligature / marque, contextuelles,
  extensions), `GDEF` (classes de glyphes, classes d'attachement, jeux de marques filtrants),
  table `kern` ancienne format 0 en repli, `vhea`/`vmtx`/`VORG` pour le vertical.
- `unicode` : données et algorithmes écrits ici, sans table copiée — bidirectionnel UAX #9
  (`reorder_visual`), grappes de graphèmes UAX #29, classes de jointure arabes, classes
  combinatoires canoniques, tags de script OpenType.
- `shape` : `Shaper` / `shape(font, texte, &ShapeOptions) -> Vec<ShapedGlyph>`. Réordonne
  (bidi), regroupe (grappes), substitue (`GSUB`), positionne (`GPOS`), résout les attachements,
  inverse les passages de droite à gauche. `cluster` est un indice d'octet dans la chaîne
  d'origine, ce qui rend le curseur et la sélection justes. Voir
  `spec-notes/opentype-gsub-gpos.md` pour ce qui est refusé.
- Sortie : contours `acrux_core::Path` en unités de police ; le rasteriseur fait le reste.
- À faire : CMaps CJK prédéfinies, hinting (non prévu), sous-ensembles CFF, écritures
  syllabiques (indiennes, khmère, thaïe) dans le compositeur.

### graphics/ (`acrux-graphics`) — fait
- `raster` : scanline anti-aliasé (16 sous-lignes, couverture horizontale exacte), non-zero
  et even-odd, `Rasterizer` réutilisable ; `stroke` : largeur, extrémités, joints, tirets,
  onglet, anisotropie ; `paint` : uni, dégradés axial/radial PDF, image (mipmap, bilinéaire),
  fonction ; `color` : 16 modes de fusion ; `bitmap` : RGBA prémultiplié et `Mask` de
  couverture ; `png` : encodeur (sans compression pour les images de référence, filtré et
  compressé via un `deflate` fourni par l'appelant pour l'export) ; `icc` (en cours) : profils ICC → sRGB.

### render/ (`acrux-render`) — fait
- `content` (opérations, images en ligne), `state` (état graphique), `colorspace`,
  `function` (types 0, 2, 3, 4), `font` (dictionnaires PDF → programmes, encodages,
  largeurs, substitution système), `image` (bpc 1-16, Decode, masques, SMask, JPEG),
  `shading` (types 1 à 7), `interpreter` (tous les opérateurs, XObjects, motifs, ExtGState,
  masques souples, groupes de transparence, contenu optionnel, budgets anti-hostilité),
  `page` (matrice de base, rotation, annotations). Un masque souple ou un groupe de
  transparence n'est rendu hors écran **que dans sa `/BBox`** ramenée en pixels, jamais en
  pleine page, et une découpe de `/BBox` qui couvre déjà toute la cible n'est pas posée :
  sur une page à huit masques imbriqués, cela fait passer le rendu de 259 à 170 ms (150 dpi,
  sortie identique au pixel près).
- À faire : `text_extract` vit pour l'instant dans `acrux-features::text` ; cache de rendu
  multi-thread ; surimpression réelle (l'aperçu de séparation vit dans
  `acrux-features/preflight/separations`, qui reconvertit le rendu RVB en CMJN et rasterise les
  tons directs).

### features/ (`acrux-features`)
- Faits : `pages` (pivoter, supprimer, réordonner, extraire, insérer, fusionner, import
  d'objets entre documents), `text` (extraction structurée : `text/extract` interprète le
  contenu avec les styles et collecte filets et puces, `text/tables` détecte les tableaux,
  `text/layout` reconstruit colonnes, blocs, paragraphes, en-têtes et pieds par découpe XY,
  `text/export` produit texte brut, Markdown, HTML et texte positionné ; recherche), `annotations` (inventaire, création avec apparences, suppression), `navigation`
  (destinations, liens, signets, et l'écriture des tableaux de destination et des dictionnaires
  d'action), `docinfo` (propriétés du document : lecture conjointe de `/Info` §14.3.3 et du
  paquet XMP §14.3.2 avec les divergences signalées, écriture des deux d'un seul tenant, le XMP
  étant modifié **élément par élément** par `docinfo/xmp` — un analyseur RDF/XML minimal écrit
  pour l'occasion — afin de garder les schémas inconnus ; dates PDF et ISO 8601 dans les deux
  sens ; propriétés d'ouverture §12.2 : `/PageMode`, `/PageLayout`, `/OpenAction`,
  `/ViewerPreferences` ; assainissement avant publication), `outline_edit` (écriture des signets
  §12.3.3 : arbre complet avec `/First`, `/Last`, `/Prev`, `/Next`, `/Parent` et `/Count`
  **signé**, ajout, retrait, déplacement par chemin, signets déduits des titres),
  `linkedit` (pose et retrait des annotations `/Link` §12.5.6.5, cibles URI, page interne,
  fichier externe ou action nommée, et détection automatique des adresses du texte sur le
  rectangle réel des mots), `attach` (pièces jointes §7.11 : arbre `/Names /EmbeddedFiles`
  reconstruit trié et équilibré, flux `/EmbeddedFile` compressés avec somme MD5, annotations
  `/FileAttachment` avec leur icône), `pagelabels` (étiquettes de page §12.4.2 : arbre de nombres,
  styles romains et alphabétiques, préfixes, recherche d'une page par son étiquette), `redact` (biffure définitive §12.5.6.23 : marques
  `/Redact`, recherche par motif écrite à la main, application par réécriture du flux — glyphes
  retirés des `Tj`/`TJ` avec correction `TJ`, formulaires réécrits récursivement, pixels des
  images mis à zéro, tracés couverts supprimés — et nettoyage des données cachées), `forms` (AcroForm §12.7 : inventaire avec héritage et noms
  qualifiés, remplissage avec régénération des apparences §12.7.4.3, aplatissement, FDF §12.7.8),
  `export` (conversion : pages en PNG et JPEG, extraction des images incorporées dans leur
  format d'origine, HTML positionné ou reflué avec images en `data:` et polices en WOFF,
  DOCX et XLSX en WordprocessingML / SpreadsheetML, texte et Markdown via `text::export`),
  `zip` (écrivain et lecteur ZIP minimal, conteneur des `.docx` et `.xlsx`),
  `fontembed` (police système choisie pour un texte donné, réduite à ses glyphes par
  `acrux_fonts::subset` et écrite en `/Type0` `Identity-H` avec `/ToUnicode` : c'est ce qui permet
  d'écrire ailleurs qu'en WinAnsiEncoding tout ce que **nous** ajoutons au document),
  `create` (**création de documents** : `create/paper` porte les formats — série A, lettre, légal,
  tabloïd, enveloppes, taille libre — l'orientation et les marges ; `create/builder` est la
  charpente commune (document neuf bâti sur un squelette minimal dont l'objet 1 devient le
  catalogue, pages à référence réservée d'avance pour que liens et signets puissent les citer,
  flux de contenu, jeu de polices partagé, arbre `/Outlines`) ; `create/wrap` découpe un paragraphe
  en lignes (coupure aux espaces, césure simple, coupure dure pour le code) ; `create/text` tient
  la colonne qui déborde de page en page et compose le texte brut ; `create/images` monte les
  images en grille — le JPEG passe tel quel, le PNG est recompressé avec son `/SMask`, par le
  décodeur partagé `stamp/image` ; `create/markdown` porte **notre** analyseur Markdown et son
  rendu ; `create/combine` réunit plusieurs fichiers, engendre signets, sommaire cliquable et
  numérotation continue. Le choix de police suit la même règle que `stamp` : police standard tant
  que le texte tient en WinAnsiEncoding, `fontembed` sinon),
  `stamp` (apports visuels sur un document existant : filigranes, arrière-plans, en-têtes et pieds
  de page, numérotation Bates ; le tampon est un flux ajouté au `/Contents` de la page — le contenu
  d'origine garde ses octets, encadré par `q`/`Q` quand le tampon passe devant — et le dessin est un
  XObject de formulaire unique partagé par toutes les pages, placé par une matrice calculée dans
  l'espace d'affichage `/Rotate` compris ; `stamp/metrics` porte les largeurs AFM des polices
  standard, `stamp/image` incorpore un PNG décodé ou un JPEG tel quel, `stamp/numbering` les formats
  de numéro et les jetons ; `remove_stamps` défait la pose),
  `edit_text` (édition de texte in place : `rewrite_content` réécrit le flux d'une page en
  recopiant octet pour octet tout ce qui n'est pas remplacé, `edit_text/scan` suit l'état texte
  pour retrouver l'opération et les octets qui ont dessiné une plage de glyphes de `PageText`,
  `edit_text/encode` réencode avec la police en place et la complète au besoin depuis la police
  système (via `acrux_fonts::subset`), `edit_text/reflow` recompose un paragraphe entier dans sa
  boîte ; le positionnement est conservé par des ajustements `TJ`),
  `accessibility` (structure logique §14.7 et PDF/UA : `accessibility/marked` parcourt le flux
  pour relier chaque tranche d'octets à sa marque `/MCID` ou `/Artifact` et collecte les aplats
  qui servent de fond au contraste, `read_structure` lit l'arbre avec texte, page, `/Alt`,
  `/ActualText`, `/Lang` et attributs `/A`, `accessibility/check` applique 17 règles PDF/UA-1 et
  WCAG, `accessibility/autotag` construit l'arbre depuis la mise en page de `text` et insère les
  marques dans le flux réécrit octet pour octet),
  `preflight` (conformité PDF/A-1b à -3b, PDF/X-1a et -4, PDF/UA-1 : `preflight/scan` inventorie
  polices, images, espaces colorimétriques, transparence, actions et métadonnées en une passe,
  `check_profile` applique les règles du profil, `preflight/fix` répare sans perte — générateur
  XMP et profil ICC sRVB matriciel/TRC écrits ici, incorporation des polices système, retraits —
  et `preflight/separations` rend l'aperçu de sortie : plaques CMJN, tons directs, taux d'encre),
  `compare` (comparaison de deux documents : `compare/align` apparie d'abord les pages sur la
  similarité de leur contenu (Needleman-Wunsch, pages insérées, supprimées et déplacées), puis
  les mots de chaque paire de pages par plus longue sous-suite commune avec un repli linéaire
  par ancres uniques sur les pages énormes ; `compare/visual` compare les pixels de deux rendus
  avec une tolérance d'anticrénelage et fusionne les différences en zones ; `compare/report`
  écrit un PDF côte à côte où chaque page source est reprise comme XObject de formulaire, les
  différences surlignées en vert, rouge et bleu, avec une page de résumé).
  `signature` (signatures numériques §12.8 : `signature.rs` inventorie les champs `/Sig`
  du formulaire et ce qu'ils déclarent, `signature/x509` lit **et émet** des certificats
  X.509, `signature/cms` lit et fabrique l'enveloppe `SignedData` détachée de la
  RFC 5652, `signature/verify` rend cinq verdicts indépendants — condensé, signature
  RSA, chaîne de certificats, couverture de tout le fichier, modifications postérieures
  — et `signature/sign` pose une signature par mise à jour incrémentale en réservant le
  `/Contents` puis en écrivant le `/ByteRange` à largeur fixe, sans décaler un octet).
- `media` : multimédia (§13.2, §13.7) — retrouve les vidéos et les sons derrière les annotations
  `/Screen`, `/RichMedia` et `/Movie`, en descendant action → rendition → clip → spécification de
  fichier, et en extrait les octets. Ne décode rien ; refuse d'ouvrir un média extérieur au
  document.
- `edit_objects` : édition des objets d'une page (images, dessins, groupes, dégradés, blocs de
  texte) — inventaire avec boîte, matrice et **plage d'octets** ; déplacer, redimensionner,
  pivoter, recadrer, réordonner, supprimer, remplacer une image, aligner. Tout passe par une
  réécriture chirurgicale : la plage de l'objet est encadrée par `q N cm … Q`, le reste du flux est
  recopié octet pour octet. La matrice demandée est en espace page et devient `N = C · M · C⁻¹`.
- `fillsign` : remplir et signer (l'outil courant d'Acrobat, à ne pas confondre avec `signature`) —
  signature **tracée** au pointeur (`ink` : lissage sans retard, largeur selon la vitesse ou la
  pression, effilage, éventail dans les virages, contour en cubiques rempli en règle non nulle),
  **tapée** (police manuscrite du système incorporée), **importée** (`cutout` : fond de papier
  retiré par estimation **locale** de l'éclairage), plus du texte libre et cinq marques (`marks`).
  Chaque élément est une annotation avec son apparence ; `flatten` les fond dans les pages.
- À faire : `edit_objects`, `security`,
  `ocr`, `optimize`, `scripting`, `ai`.

### app/ (`acrux-app`)
- `platform` : abstraction `App`/`Event`/`Frame`/`WindowHandle`/`Waker` ; `platform::win32` :
  fenêtre, boucle de messages, présentation du tampon (`StretchDIBits`), DPI par moniteur,
  dialogue d'ouverture, glisser-déposer, presse-papiers, curseurs, réveil inter-fils.
  **Seul module autorisé à utiliser `unsafe`** (FFI). Mode invisible (`ACRUX_HEADLESS`) : la fenêtre
  n'est jamais montrée, l'application peint dans son tampon à chaque demande de redessin, publie sa
  poignée dans `ACRUX_HWND` et remplace les dialogues système par des variables d'environnement — les
  tests d'interface n'affichent donc rien.
- `render_worker` : fil de rendu avec sa propre copie du document (l'interface ne se fige jamais) ;
  les modifications (`EditOp`) sont appliquées aux deux copies, avec un numéro de génération pour
  ignorer les rendus périmés ; les exports (`ExportFormat`) y sont faits aussi et leur compte rendu
  revient par un canal de messages affichés dans la barre d'état.
- `viewer` : défilement continu, zoom, ajustement largeur, navigation, cache de pages,
  recherche (Ctrl+F), sélection / copie de texte, invite de mot de passe, barre d'état.
- `selection` : modèle de sélection de texte (curseurs entre glyphes, plages, rectangles).
- `update` : recherche des mises à jour — interroge la page des versions du dépôt (une requête
  `GET`, au plus une fois par jour, désactivable), avec un **lecteur JSON écrit pour l'occasion**
  (`update/json.rs`) plutôt qu'une recherche de texte à l'aveugle, qui se ferait piéger par une
  accolade dans une note de version. Le téléchargement n'accepte qu'une adresse du domaine de
  publication de GitHub, et rien ne s'installe sans un oui explicite.
- `platform/media` : lecture des vidéos et des sons, par **Media Foundation** (`IMFSourceReader`)
  et `waveOut`. Décoder du H.264 et de l'AAC à la main n'aurait aucun sens : ce sont des normes
  immenses et le système en a déjà des décodeurs, souvent accélérés par la carte graphique. Ce qui
  nous appartient, c'est ce qu'on fait des images : elles arrivent en BGRA dans un tampon ordinaire
  et sont **composées dans la page**, si bien que la vidéo suit le défilement et le zoom sans
  fenêtre flottante. Le son donne l'heure — la position vient de ce que la carte son déclare avoir
  joué, pas de l'horloge de la machine.
- `ui/video` : image mise à l'échelle bilinéaire dans la page, barre de commandes (lecture, pause,
  ligne de temps, durée).
- `platform/http` : une requête HTTPS, par **WinHTTP** — le certificat, les redirections et le
  proxy de l'entreprise sont l'affaire du système, pas la nôtre.
- `ui/objects` : outil « modifier » — boîte de sélection, huit poignées, redimensionnement sans
  retournement, proportions gardées avec Maj ; la conversion page ↔ vue reste au viewer, qui seul
  connaît le zoom et le défilement.
- `ui/sign` : outil « remplir et signer » — barre des éléments posables et fenêtre de capture
  (tracer, taper, importer) ; l'aperçu est dessiné par le **même** code que le PDF
  (`acrux_features::fillsign::ink`), si bien que ce qu'on voit est ce qu'on pose. Signature et
  paraphe sont conservés dans les préférences.
- `ui` : toolkit interne — `text` (texte d'interface rendu par acrux-fonts + acrux-graphics),
  `theme`, `input` (champ de saisie), `icons` (icônes vectorielles), `toolbar`, `panel`
  (vignettes, signets, commentaires, calques), `tabs` (un onglet par document), `prefs` (réglages persistants et
  dispositions de pages), `palette` (palette de commandes Ctrl+Maj+P, qui tient lieu de barre de
  menus : chaque entrée appelle la même fonction que son raccourci). Toute la fenêtre est peinte
  dans un `Frame` ; les zones (barre, panneau, document) sont des sous-vues (`Frame::sub`) qui
  partagent le tampon sans copie.
- L'icône et les informations de version de `acrux.exe` viennent de `acrux-winres` (voir plus bas) ;
  la fenêtre charge la ressource ordinale 1 avec `LoadImageW`, aux tailles rendues par `GetSystemMetrics`.
- À faire : localisation, backends macOS et Linux.

### setup/ (`acrux-setup`, exécutable `acrux-setup.exe`)
L'installateur, écrit comme le reste : aucun outil tiers (ni WiX, ni NSIS, ni Inno Setup).
Un **exécutable auto-extractible** — le programme, puis une archive compressée par notre propre
Flate, puis sa longueur et une signature ; il se relit lui-même pour retrouver sa charge. Fenêtre
en contrôles Windows standard, installation dans `%LOCALAPPDATA%\Programs` et `HKEY_CURRENT_USER`
(**jamais de droits d'administrateur**), raccourcis par COM `IShellLink`, inscription dans
« Applications installées », association `.pdf` proposée sans être imposée, et désinstallation
complète. Modes `--silent` (utilisé par la mise à jour), `--uninstall` et `--pack` (fabrication de
la release). C'est, avec `acrux-app/src/platform/`, le seul endroit du dépôt où `unsafe` est
autorisé : un installateur est du code système par nature.

### winres/ (`acrux-winres`)
Ressources Windows des exécutables, écrites sans `rc.exe` ni bibliothèque externe : conteneur
`.res`, groupe d'icônes (`RT_GROUP_ICON` + `RT_ICON`, lus depuis `branding/acrux.ico`) et bloc
`VS_VERSIONINFO`. Utilisé en dépendance de compilation (`build.rs`) par `acrux-app` et
`acrux-cli` ; l'exécutable porte ainsi son icône et ses informations de version. Ne fait rien
hors cible Windows/MSVC. Voir `crates/acrux-winres/README.md`.

### cli/ (`acrux-cli`, exécutable `acr`)
`info`, `pages`, `dump`, `check`, `rotate`, `delete`, `reorder`, `extract`, `merge`,
`rewrite`, `render`, `export`, `text`, `bench`, `edit-text`, `reflow`, `annots`, `annotate`,
`links`, `outline`, `protect`, `unprotect`, `fields`, `fill`, `fdf-export`, `fdf-import`,
`redact`, `sanitize`, `check-a11y`, `autotag`, `preflight`, `separations`.

## 4. Organisation du dépôt

```
Acrux/
  README.md              présentation, captures, installation en 3 commandes
  CHARTE_PROJET.md       règles du projet
  ARCHITECTURE.md        ce fichier
  ROADMAP.md             phases et état d'avancement
  CONTRIBUTING.md        comment contribuer, conventions, revue
  LICENSE
  docs/                  documentation utilisateur et développeur, schémas
  spec-notes/            résumés par chapitre de la spec, avec renvois vers le code
  core/  document/  codecs/  fonts/  graphics/  render/  features/  app/
     └── chaque module : README.md, src/, tests/
  tests/
    corpus/              PDF de test classés par fonctionnalité (rendu, formulaires, polices…)
    reference/           images de référence pour la comparaison de rendu
    fuzz/                cibles de fuzzing (parseur, codecs, polices)
  tools/                 scripts de build, génération du corpus, benchmark, comparateur d'images
```

## 5. Principes techniques transverses
- **Chargement paresseux** : rien n'est parsé tant que ce n'est pas nécessaire.
- **Immutabilité + journal de modifications** : toute édition est une opération enregistrée
  (annuler / rétablir illimité, historique de versions).
- **Un seul chemin de rendu** pour l'écran, l'impression, l'export image (PNG, JPEG) et les
  vignettes ; **un seul chemin d'extraction** (`text`) pour la recherche, la copie, l'export
  texte, HTML, DOCX et XLSX.
- **Tolérance aux erreurs** partout : un objet illisible est signalé, jamais fatal.
- **Fuzzing continu** des parseurs et codecs.
- **Tests de rendu** : chaque PDF du corpus a une image de référence ; toute différence de plus
  de N pixels bloque la fusion.
- **Benchmarks** suivis dans le temps (ouverture, rendu, mémoire).
- **Aucun accès réseau** dans le cœur ; les fonctions cloud éventuelles sont des plug-ins.
