# acrux-codecs

Décodeurs de flux PDF écrits de zéro d'après les spécifications (ISO 32000-2 §7.4,
RFC 1950/1951, TIFF 6.0, PNG). Aucune dépendance externe, pas de `unsafe`, jamais de
panique sur un flux corrompu.

Voir `ARCHITECTURE.md` à la racine pour la place de ce crate dans l'ensemble,
et `src/lib.rs` pour la liste des modules.

## Ce qui est implémenté

| Module         | Filtre PDF (abréviation image en ligne) | Spécification              | État |
|----------------|------------------------------------------|----------------------------|------|
| `flate`        | FlateDecode (`Fl`)                       | RFC 1950 (zlib), RFC 1951 (deflate) | Fait |
| `predictor`    | /Predictor 2 (TIFF) et 10-15 (PNG)       | ISO 32000-2 §7.4.4.4, TIFF 6.0 §14, PNG §9 | Fait |
| `lzw`          | LZWDecode (`LZW`)                        | ISO 32000-2 §7.4.4.2       | Fait |
| `ascii`        | ASCIIHexDecode (`AHx`), ASCII85Decode (`A85`) | ISO 32000-2 §7.4.2, §7.4.3 | Fait |
| `runlength`    | RunLengthDecode (`RL`)                   | ISO 32000-2 §7.4.5         | Fait |
| `ccitt`        | CCITTFaxDecode (`CCF`)                   | ITU-T T.4 / T.6, ISO 32000-2 §7.4.6 | Fait (décodeur, voir ci-dessous) |
| `dct`          | DCTDecode (`DCT`)                        | ITU-T T.81 (JPEG), ISO 32000-2 §7.4.8 | Fait (décodeur, voir ci-dessous) |
| `jpx`          | JPXDecode                                | ISO 15444-1 (JPEG 2000), ISO 32000-2 §7.4.9 | Fait (décodeur, voir ci-dessous) |
| `jbig2`        | JBIG2Decode                              | ITU-T T.88, ISO 32000-2 §7.4.7 | Fait (décodeur, voir ci-dessous) |

Point d'entrée générique : `apply_filter(name, data, &DecodeParms)` choisit le codec
d'après le nom du filtre et applique le prédicteur de `/DecodeParms` après Flate et LZW.
`DecodeParms::default()` reprend les défauts de la spécification (Predictor 1, Colors 1,
BitsPerComponent 8, Columns 1, EarlyChange 1). Les filtres d'image renvoient
`Error::Unsupported` par ce point d'entrée : la couche image du document appelle
directement `dct`, `jpx`, `ccitt` et `jbig2` avec leurs paramètres propres.

### Détails par module

- **flate** : blocs stored, Huffman fixe et dynamique, fenêtre 32 Ko, en-tête zlib
  détecté et facultatif (repli sur deflate brut), blancs parasites ignorés avant
  l'en-tête, Adler-32 vérifié sans bloquer. Un flux tronqué ou corrompu rend les
  données décodées jusqu'à l'erreur (`decode_detailed` indique `complete = false`) ;
  `Err(Corrupt)` seulement si rien n'a pu être décodé. Limite de sortie configurable
  (`decode_with_limit`, 256 Mo par défaut) contre les bombes de décompression.
  Encodeur `encode` en blocs stored uniquement (flux zlib valide, sans compression),
  utile pour écrire des PDF de test.
- **predictor** : PNG None/Sub/Up/Average/Paeth (octet de filtre par ligne), TIFF 2
  pour 1, 2, 4, 8 et 16 bits par composante. Une dernière ligne incomplète est ignorée.
- **lzw** : codes de 9 à 12 bits, ClearTable (256), EOD (257), EarlyChange 0 et 1,
  cas KwKwK, table pleine sans ClearTable tolérée, flux tronqué rendu partiellement.
- **ascii** : blancs ignorés, marqueur de fin facultatif, `<~` accepté, `z`, groupe
  final partiel, chiffre hexadécimal isolé complété par 0. Les caractères étrangers
  sont ignorés.
- **runlength** : plages littérales et répétées, EOD (128) facultatif, plage tronquée
  rendue partiellement.
- **ccitt** : décodeur Groupe 3 (T.4, 1-D « MH » et 2-D « MR ») et Groupe 4 (T.6,
  « MMR ») écrit d'après les recommandations. API
  `ccitt::decode(&[u8], &CcittParams) -> Result<Vec<u8>>`, `CcittParams` reprenant
  le tableau 11 de §7.4.6 (`k`, `columns` = 1728, `rows` = 0 si inconnu, `black_is_1`,
  `byte_align`, `end_of_line`, `end_of_block`) avec `Default`. Sortie : 1 bit par
  pixel, lignes alignées sur l'octet, `Rows` lignes (ou autant que décodées),
  **noir = 0** sauf `BlackIs1` (tableau 11).
  - Couvert : tables de terminaison et de composition blanches et noires (tableaux
    2/T.4, 3a/T.4), codes étendus 1792–2560 (3b/T.4), plages > 2560 par codes de
    composition répétés ; modes P, H, V0, VR1–3, VL1–3 ; EOL avec bits de
    remplissage, bit d'étiquette 1-D/2-D (K > 0), RTC et EOFB, `EncodedByteAlign`
    pour K < 0, K = 0 et K > 0 ; `DamagedRowsBeforeError` ignoré (toujours tolérant).
  - Tolérance (comportement d'Acrobat) : EOL absents, RTC/EOFB absent, ligne
    tronquée complétée dans la couleur courante, plage dépassant la largeur coupée,
    erreur après quelques lignes → lignes décodées rendues et lignes manquantes
    blanches quand `Rows` est connu ; `Err(Corrupt)` seulement si aucune ligne n'est
    décodable. Limites : `MAX_COLUMNS` (2²⁰), `MAX_OUTPUT_BYTES` (1 Go).
  - Le décodeur T.6 est réutilisé par `jbig2` (`MmrDecoder`, interne) pour les
    régions MMR, y compris les plans successifs des demi-teintes sans réinitialisation.
- **dct** : décodeur JPEG complet écrit d'après ITU-T T.81, API
  `dct::decode(&[u8]) -> Result<JpegImage>` (pixels entrelacés après transformation de
  couleur, drapeaux `adobe_inverted` et `progressive`) et
  `dct::read_header(&[u8]) -> Result<(largeur, hauteur, composantes)>` sans décoder.
  Il n'est pas branché sur `apply_filter` : la couche image du document interprète
  `/ColorSpace`, `/Decode` et l'inversion Adobe à partir de `JpegImage`.
  - Couvert : SOF0 (baseline), SOF1 (séquentiel étendu, Huffman), SOF2 (progressif,
    annexe G : DC first/refine, AC first/refine, EOBRUN, bandes spectrales) ; précision
    8 bits, 12 bits ramenés à 8 ; 1, 3 ou 4 composantes ; facteurs d'échantillonnage
    1 à 4 (4:4:4, 4:2:2, 4:2:0, 4:1:1 et rapports non entiers) ; DQT 8 et 16 bits ;
    tables DHT incomplètes ; DRI/RSTn avec resynchronisation sur le numéro du marqueur ;
    DNL ; APP0 JFIF ; APP14 Adobe (`transform` 0 = aucune, 1 = YCbCr, 2 = YCCK) ;
    APPn/COM inconnus ignorés ; sans APP14, 3 composantes = YCbCr sauf identifiants
    'R' 'G' 'B', 4 composantes = CMYK brut. Les CMYK Adobe ne sont **pas** inversés :
    `adobe_inverted` le signale à la couche PDF (/Decode).
  - Tolérance : octets parasites avant SOI, EOI manquant, données tronquées ou
    corrompues → image partielle (blocs manquants gris), jamais de panique.
  - Limites : `MAX_PIXELS` (2³¹) et `MAX_BUFFER_BYTES` (1 Go, sortie et coefficients)
    → `Err(Corrupt)` au-delà.
  - IDCT séparable en virgule flottante avec tables précalculées (erreur mesurée
    ≤ 0,5 LSB avant arrondi contre l'IDCT directe de A.3.3) ; sur-échantillonnage
    des chromas par interpolation triangulaire (3/4-1/4, biais d'arrondi alterné) pour
    les rapports 2:1, réplication pour les autres.
  - Non couvert (`Err(Unsupported)`) : codage arithmétique (SOF9-11, SOF13-15),
    mode hiérarchique (SOF5-7, DHP, EXP), mode sans perte (SOF3).
- **jpx** : décodeur JPEG 2000 partie 1 écrit d'après ISO/IEC 15444-1, organisé en
  sous-modules (`src/jpx/`) : `boxes` (fichier JP2, annexe I), `codestream`
  (marqueurs, annexe A), `structure` (tuiles, résolutions, sous-bandes, précincts,
  blocs, annexe B), `tier2` (progressions, arbres d'étiquettes, en-têtes de paquets),
  `mq` (décodeur MQ, annexe C), `tier1` (EBCOT, annexe D), `dwt` (ondelettes inverses,
  annexe F). API `jpx::decode(&[u8]) -> Result<JpxImage>` (pixels 8 bits entrelacés
  après MCT, `colorspace`, `has_alpha`, `palette_applied`),
  `jpx::decode_with_options(&[u8], &JpxOptions)` (`max_resolution_reduction`,
  `smask_in_data` pour garder le canal alpha du JPX) et
  `jpx::read_header(&[u8]) -> Result<(largeur, hauteur, canaux)>`. Non branché sur
  `apply_filter`, comme `dct`.
  - Couvert : flux J2K brut et fichier JP2 (`jP  `, `ftyp`, `jp2h` avec `ihdr`,
    `colr` méthode 1 (sRGB 16, gris 17, sYCC 18 converti en RVB, CMYK 12) et
    méthode 2 (profil ICC rendu tel quel dans `JpxColorSpace::Icc`), `pclr` + `cmap`
    (palette appliquée), `cdef` (canal alpha), `jp2c`) ; marqueurs SIZ, COD, COC,
    QCD, QCC, RGN (Maxshift appliqué), POC (appliqué), PPM/PPT (en-têtes de paquets
    déportés), SOT/SOD/EOC, SOP/EPH, TLM/PLM/PLT/CRG/COM/CAP ignorés ; tuiles et
    tuiles-parties multiples ; composantes sous-échantillonnées (répliquées sur la
    grille de la composante 0) ; précisions 1 à 16 bits, signées ou non ; cinq
    progressions, couches multiples, précincts personnalisés, arbres d'étiquettes ;
    tier-1 complet (MQ, trois passes, 19 contextes, mode plage, bypass, reset,
    termall, causal vertical, terminaison prédictible, symboles de segmentation) ;
    déquantification nulle / dérivée / explicite ; ondelettes 5-3 réversible (entiers)
    et 9-7 irréversible (lifting flottant, extension symétrique) ; RCT et ICT
    inverses ; décalage DC ; décodage à résolution réduite.
  - Sortie 8 bits : >8 bits tronqués par décalage, <8 bits étirés sur 0..255, signés
    décalés de 2^(p−1). Le canal alpha n'est conservé qu'avec `smask_in_data`.
  - Tolérance : troncature ou corruption → image partielle (blocs non reçus à
    mi-échelle), EOC manquant, Psot nul, boîtes inconnues ou tronquées, SOP/EPH
    reconnus même non annoncés. Jamais de panique ni de boucle infinie.
  - Limites : `MAX_PIXELS` (2³¹), `MAX_BUFFER_BYTES` (1 Go pour la sortie et,
    séparément, pour les coefficients d'une tuile), 2²⁰ précincts et 2²² blocs par
    tuile → `Err(Corrupt)` au-delà.
  - Non couvert (`Err(Unsupported)`) : partie 2 (ISO 15444-2), précisions au-delà
    de 16 bits, palettes de plus de 16 bits. Écarts assumés : un flux 5-3 annonçant
    une quantification scalaire (non conforme) est décodé sans déquantification ; les
    échantillons sous-échantillonnés sont répliqués (pas d'interpolation) ; la
    reconstruction des plans de bits non reçus se fait au milieu de l'intervalle.

- **jbig2** : décodeur JBIG2 (T.88) pour le profil PDF, organisé en sous-modules
  (`src/jbig2/`) : `mq` (décodeur MQ de l'annexe E et procédures entières de
  l'annexe A), `generic` (régions génériques et de raffinement), `huffman` (tables
  B.1–B.15, tables personnalisées, codes des symboles), `symbol` (dictionnaires de
  symboles et régions de texte), `halftone` (motifs et demi-teintes), `bitmap`. API
  `jbig2::decode(data, globals: Option<&[u8]>, width, height) -> Result<Vec<u8>>` :
  `data` est le flux embarqué (annexe D.3, sans en-tête de fichier), `globals` le flux
  `/JBIG2Globals`, `width` × `height` viennent du dictionnaire d'image et fixent la
  page (hauteur JBIG2 0xFFFFFFFF avec bandes acceptée). Sortie : 1 bit par pixel,
  lignes alignées sur l'octet, **noir = 0** : JBIG2 code 1 = noir (T.88 §2.4) et
  §7.4.7 demande au filtre de livrer l'image inversée pour DeviceGray 1 bit / /Decode
  [0 1] par défaut (voir la note de polarité en tête de `src/jbig2.rs`).
  - Couvert : en-têtes de segment (§7.2) avec longueur inconnue 0xFFFFFFFF (§7.2.7),
    informations de page (valeur de pixel par défaut), fin de bande / page / fichier,
    profils et extensions ignorés ; région générique immédiate et intermédiaire
    (36, 38, 39) en codage arithmétique (gabarits 0–3, pixels adaptatifs, TPGDON)
    et MMR ; dictionnaire de symboles (0) et région de texte (4, 6, 7) en
    arithmétique (IADH/IADW/IAEX/IAAI/IADT/IAFS/IADS/IAIT/IAID/IARI/IARDW/IARDH/
    IARDX/IARDY, raffinement, agrégation, contextes conservés / réutilisés, tous les
    coins de référence, transposition, SBDSOFFSET, bandes) et en Huffman (tables
    standard B.1–B.15, tables personnalisées (53), codes de symboles à plages,
    bitmaps collectifs non compressés et MMR) ; région de raffinement (40, 42, 43),
    gabarits 0–1, TPGRON, référence intermédiaire ou page ; dictionnaire de motifs
    (16) et région de demi-teintes (20, 22, 23), arithmétique et MMR, HENSKIP,
    grille tournée ; opérateurs OR/AND/XOR/XNOR/REPLACE ; composition de toutes les
    pages.
  - Tolérance : segment tronqué ou corrompu ignoré (la page garde ce qui a été
    composé), segment référencé manquant → région ignorée, régions hors page
    coupées, données MQ épuisées lues comme 0xFF (annexe E), aucune panique ni
    boucle infinie (toutes les boucles sont bornées par les compteurs d'en-tête).
  - Limites : `MAX_BITMAP_PIXELS` (2³⁰), `MAX_SYMBOLS` (2²⁰), `MAX_SYMBOL_PIXELS`
    (2²⁶ par dictionnaire), `MAX_PATTERNS` (2¹⁶), `MAX_HALFTONE_CELLS` (2²⁴), largeur
    de région ≤ 4 × page + `MAX_REGION_OVERHANG`.
  - Non couvert : dictionnaire de symboles Huffman avec raffinement / agrégation
    (SDHUFF = 1 et SDREFAGG = 1) et raffinement Huffman de taille inconnue
    (BMSIZE = 0) → `Err(Unsupported)` ; gabarit étendu EXTTEMPLATE (amendement 2) ;
    extension couleur ; longueur de code de symbole prise à `max(1, ⌈log2 n⌉)`
    (version amendée de la norme).

## Tester

```bash
cargo test -p acrux-codecs
cargo clippy -p acrux-codecs --all-targets
```

Chaque module contient ses tests unitaires avec des vecteurs connus : exemple LZW de
la spécification, zlib de « hello », bloc stored, bloc Huffman dynamique produit par
zlib, vecteurs ASCII85 classiques (« Man is distinguished »), Adler-32 de « Wikipedia ».

Le module `dct` embarque dans ses tests (`src/dct/tests.rs`) un encodeur JPEG minimal
(baseline et progressif, tables de l'annexe K) qui produit les images de test, trois
vrais JPEG écrits par GDI+ avec les pixels que GDI+ en relit
(`src/dct/tests/vectors.rs`, générés par `src/dct/tests/genjpeg.ps1`), une IDCT de
référence O(n⁴), et des tests de robustesse (troncature à toutes les longueurs,
mutations aléatoires). La mesure de performance se lance en release :

```bash
cargo test --release -p acrux-codecs a4_300dpi -- --ignored --nocapture
```

Le module `ccitt` embarque dans ses tests (`src/ccitt/tests.rs`) un encodeur T.4 /
T.6 de test (1-D, 2-D avec modes passe / horizontal / vertical, EOL, remplissage,
RTC, EOFB) qui sert aussi aux régions MMR de `jbig2`. Tests : vecteurs des tableaux
de T.4 (blanc 2 = 0111, noir 2 = 11, composition blanche 64 = 11011, ligne blanche de
1728), un flux G4 codé à la main, allers-retours G4, G3 1-D et G3 2-D (K = 1, 2, 4)
avec toutes les options (EOL, alignement, RTC) sur bandes, damiers, glyphes dessinés à
la main, lignes blanches, lignes noires, largeurs non multiples de 8, plages > 2560,
troncature à toutes les longueurs, mutations et octets aléatoires.

Le module `jbig2` embarque (`src/jbig2/tests/encoder.rs`) un encodeur JBIG2 de test
écrit d'après T.88 : codeur MQ de l'annexe E (il reproduit octet pour octet la
séquence de test de l'annexe H.2, que le décodeur relit aussi), procédures entières de
l'annexe A, région générique (contexte formé pixel par pixel d'après les figures 4 à
8, indépendamment du décodeur), raffinement, dictionnaire de symboles et région de
texte arithmétiques et Huffman, dictionnaire de motifs, demi-teintes, tables
personnalisées, en-têtes de segment. Les tests vérifient le décodage exact de chaque
type de segment (tous gabarits, TPGDON, pixels adaptatifs, MMR, longueur inconnue,
tous coins de référence et transposition, raffinement, globaux, dictionnaires
chaînés, toutes les tables standard) et la robustesse (troncature à toutes les
longueurs, mutations, octets aléatoires, dimensions absurdes).

Performances mesurées en release (`a4_300dpi`, page A4 à 300 dpi synthétique,
2480 × 3508) : CCITT G4 ≈ 8 ms, G3 1-D ≈ 9 ms ; JBIG2 région générique (gabarit 0,
TPGDON) ≈ 27 ms, région de texte de 115 000 instances ≈ 19 ms.

Le module `jpx` embarque de même (`src/jpx/tests/encoder.rs`) un encodeur JPEG 2000
minimal écrit d'après 15444-1 : encodeur MQ (il reproduit octet pour octet le vecteur
de test de l'annexe H.2 de T.88), codage EBCOT des trois passes (via le moteur de
contexte partagé avec le décodeur, dans tous les modes de bloc), ondelettes directes
5-3 et 9-7, RCT/ICT, arbres d'étiquettes d'encodage, en-têtes de paquets, toutes les
progressions et POC, tuiles, tuiles-parties, PPM/PPT, SOP/EPH, ROI, COC, JP2. Les
tests vérifient l'aller-retour **sans perte** (5-3 + RCT) sur des images synthétiques
(dégradés, damiers, bruit, RVB, tailles impaires dont 37×29 et 1×1, origine non
nulle, 0 à 5 niveaux, blocs de 4×4 à 64×64, tuiles, 12 et 16 bits ramenés à 8,
1 et 4 bits, signé, palette, cdef, sYCC, sous-échantillonnage), l'aller-retour 9-7 +
ICT à ±3 niveaux, la réduction de résolution, et la robustesse (troncature à toutes
les longueurs, mutations aléatoires, en-têtes absurdes). Aucun outil de la machine ne
sait écrire un vrai .jp2 (WIC, GDI+ et PowerShell ne le font pas) : la conformité est
établie par ces allers-retours et par les tests de composants (vecteur MQ de la norme,
filtres 5-3 / 9-7 contre leurs gains nominaux, géométrie de l'annexe B).
