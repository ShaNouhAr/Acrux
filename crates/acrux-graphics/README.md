# acrux-graphics

Rasteriseur 2D générique écrit de zéro d'après les spécifications (ISO 32000-2 §8.4 contours,
§8.5 chemins et clipping, §8.7.4 ombrages, §11.3 transparence et modes de fusion ; PNG
ISO/IEC 15948 ; zlib RFC 1950/1951 ; profils ICC ICC.1:2022 et ICC.1:2001-04). Indépendant du format PDF : il ne connaît que des chemins,
des matrices, des couleurs et des pixels. Aucune dépendance externe (seulement `acrux-core` et la
bibliothèque standard), pas de `unsafe`, jamais de panique quelle que soit l'entrée.

Voir `ARCHITECTURE.md` à la racine pour la place de ce crate dans l'ensemble.

Tests : `cargo test -p acrux-graphics` — benchmark : `cargo test -p acrux-graphics --release -- --ignored --nocapture`.

## Ce qui est implémenté

| Module   | Contenu | Spécification | État |
|----------|---------|---------------|------|
| `bitmap` | `Bitmap` RGBA 8 bits **prémultiplié** (origine en haut à gauche), `Mask` de couverture 0..255, intersection de masques, export RGB sur blanc | — | Fait |
| `color`  | `Color` flottante non prémultipliée, 16 `BlendMode` (séparables et non séparables), composition « source over » avec alpha constant et couverture | §11.3.5, §11.3.8 | Fait |
| `raster` | Rasteriseur scanline anti-aliasé, règles non-zero et even-odd, clip par masque, `Rasterizer` réutilisable | §8.5.3.3, §8.5.4 | Fait |
| `paint`  | Couleur unie, dégradé axial (type 2), dégradé radial (type 3, cercles interpolés, extension), image (plus proche voisin, bilinéaire, réduction par moyenne + mipmap), fonction arbitraire | §8.7.4.5.3, §8.7.4.5.4, §8.9.4 | Fait |
| `stroke` | Contour → chemin de remplissage : largeur (y compris anisotrope), extrémités Butt/Round/Square, joints Miter/Round/Bevel, limite d'onglet, tirets et phase, largeur 0, sous-chemins dégénérés | §8.4.3 | Fait |
| `png`    | Encodeur PNG RGBA / RGB sans compression (zlib « stored »), CRC-32 et Adler-32 maison | ISO/IEC 15948, RFC 1950/1951 | Fait |
| `icc`    | `IccProfile` : profils ICC v2/v4 matrice/TRC, gris et LUT (`lut8Type`, `lut16Type`, `lutAToBType`) → sRGB ; `IccCache` quantifié pour les images | ICC.1:2022, ICC.1:2001-04 ; ISO 32000-2 §8.6.5.5 | Fait (voir ci-dessous) |
| —        | Ombrages types 4 à 7 (maillages), groupes de transparence, masques souples | §8.7.4.5.5+, §11.4, §11.6 | À venir |

## Conventions

- Le rasteriseur travaille en **pixels**, origine en haut à gauche, `y` vers le bas. Le centre du
  pixel `(i, j)` est `(i + 0.5, j + 0.5)`. L'appelant fournit la matrice qui retourne l'axe `y`
  du PDF (par exemple `Matrix::new(s, 0, 0, -s, 0, hauteur)`).
- Les chemins (`acrux_core::Path`) sont transformés puis aplatis avec une tolérance de 0,1 px.
- `Bitmap` est prémultiplié ; `Color` et `Paint` sont non prémultipliés.
- Un contour se dessine en deux temps : `stroke_path` produit un chemin dans l'espace device,
  que `fill_path` remplit en `NonZero` avec la matrice identité.
- Espace image de `ImagePaint` : `(0, 0)` = coin supérieur gauche, `(1, 1)` = coin inférieur
  droit ; l'appelant inclut le retournement du carré unité PDF dans `transform`.

## Méthode de rasterisation

Chaque ligne de pixels est découpée en 16 sous-lignes. Pour chaque sous-ligne, les croisements
des arêtes actives sont triés en `x` avec leur direction ; la règle de remplissage en déduit des
intervalles intérieurs. Chaque intervalle contribue à la couverture des pixels de façon **exacte
horizontalement** (fractions exactes aux extrémités, tampon différentiel pour l'intérieur,
intégré une fois par ligne). Le même code traite donc non-zero et even-odd sans approximation
autre que l'échantillonnage vertical.

Précision : aire d'un cercle de rayon 40 px à mieux de 1 % de πr² ; bords verticaux exacts ;
bords horizontaux quantifiés au 1/16 de pixel. Les chemins hors image ne coûtent que le tri de
leurs arêtes ; les coordonnées non finies ou supérieures à 10⁹ en valeur absolue font ignorer le
chemin.

Performance (rectangle plein A4 à 150 dpi, 1240 × 1754, couleur unie) : voir le benchmark
`bench_a4_150dpi_rectangle_fill` ; le chemin rapide couleur unie + `Normal` évite tout calcul
flottant de fusion et le `Rasterizer` réutilise ses tampons entre deux appels.

## Profils ICC (`icc`)

Le module lit un profil ICC tel qu'incorporé dans un flux `/ICCBased` et convertit les couleurs
de l'appareil vers sRGB, avec l'intention de rendu qu'utilise Acrobat : **perceptuelle** (`A2B0`)
quand le profil en a une, sinon **colorimétrique relative** (profils matriciels/TRC, `A2B1`).
Chaîne : appareil → PCS (D50) → XYZ D65 (Bradford, ICC.1:2022 annexe E) → sRGB linéaire
(matrice dérivée des chromaticités IEC 61966-2-1) → courbe sRGB.

Couvert :

- en-tête et table des tags, toutes lectures bornées (profil tronqué, aléatoire ou muté : erreur
  typée, jamais de panique ; vérifié par test sur toutes les troncatures et des milliers de mutations) ;
- **matrice/TRC** RVB (`rXYZ gXYZ bXYZ rTRC gTRC bTRC`) : `curveType` (identité, gamma, table
  interpolée) et `parametricCurveType` (5 fonctions) ; profils v2 dont les colorants ne somment
  pas au D50 : adaptés par `chad` s'il existe, sinon par Bradford depuis leur somme ;
- **gris** (`kTRC`) : luminance relative → gris sRGB ;
- **LUT** `A2B0` / `A2B1` / `A2B2` : `lut8Type`, `lut16Type` (matrice appliquée seulement si
  l'entrée est XYZ, tables d'entrée, CLUT, tables de sortie) et `lutAToBType` v4 (A → CLUT →
  M → matrice → B, CLUT 8 ou 16 bits, grilles non uniformes) ; 1 à 15 canaux d'entrée
  (`GRAY`, `RGB `, `CMYK`, `Lab `, `nCLR`…) ; PCS Lab (codage 16 bits hérité du `lut16Type`
  v2, 8 bits, 16 bits v4) ou XYZ (u1Fixed15) ;
- espace de données `Lab ` sans table : entrées lues au codage Lab v4 ;
- description `desc` (`textDescriptionType` v2, `multiLocalizedUnicodeType` v4 avec préférence
  « en », `textType`) et `is_srgb_like()` (description contenant « sRGB », ou colorants et courbes
  à moins de 1 % de sRGB) pour court-circuiter la conversion ;
- `IccCache` : table n-D quantifiée (256 / 33 / 17 niveaux pour 1 / 3 / 4 composantes, réduite
  au-delà) construite paresseusement, `to_srgb` et `to_srgb_u8`.

Interpolation des CLUT : **tétraédrique** sur les trois dernières dimensions (six tétraèdres par
cube, exacte aux nœuds et pour toute fonction affine), **multilinéaire** sur les dimensions
précédentes (donc bilinéaire × tétraédrique pour le CMJN) ; le cache utilise la même méthode.

Précision et performance mesurées (tests `icc`) : profil sRGB matriciel synthétique construit sur
les colorants sRGB publiés → identité à **0,33/255** au pire sur une grille 9³ ; profil CMJN
`lut16Type` synthétique → primaires et nœuds exacts à mieux de 2/255 ; profils Lab 8 bits →
exacts à 1/255 au regard de la quantification (qui, elle, déplace jusqu'à 0,07 un canal nul d'une
couleur saturée). Cache CMJN 17⁴ : construction ≈ 11 ms, **1 M de conversions en ≈ 64 ms**
(release, `bench_one_million_cmyk_conversions_with_cache`).

Non couvert : profils de couleurs nommées (`nmcl`), `multiProcessElementsType` (v4.3+),
tables `B2A` (sRGB → appareil, inutile au rendu), intentions absolue et saturation (`A2B2` n'est
utilisée qu'en dernier recours), compensation du point noir, profils `link` / `abst`, espaces à
plus de 15 canaux.

## Écarts assumés et limites connues

- **ICC, blanc de l'appareil** : la conversion PCS → sRGB applique toujours Bradford D50 → D65,
  comme le fait un CMM avec un profil sRGB de destination (blanc du support → blanc sRGB, intention
  relative). Le tag `chad` n'intervient que pour ramener au D50 les colorants d'un profil v2 qui ne
  les a pas adaptés ; l'inverser pour « défaire » l'adaptation donnerait un blanc coloré pour tout
  appareil non D65.
- **ICC, précédence** : quand un profil a à la fois `A2B0` et des tags matrice/TRC, la table est
  utilisée (ICC.1:2022 §8.3.4) ; si elle est illisible, on retombe sur la matrice/TRC.
- **ICC, gamut** : les couleurs hors gamut sRGB sont simplement bornées à `[0, 1]` (pas de
  compression de gamut), ce qui correspond au comportement d'affichage d'Acrobat.

- **Largeur de trait minimale** : tout trait est rendu avec au moins 1 pixel device d'épaisseur
  (pas seulement la largeur 0 de §8.4.3.2), comme le fait Acrobat, pour que les filets fins
  restent visibles à faible zoom.
- **Sous-chemin dégénéré** (un seul point) : disque pour `Round`, carré pour `Square`, rien pour
  `Butt` (§8.4.3.3) — y compris pour un sous-chemin fermé.
- **Tirets sur un sous-chemin fermé** : le sous-chemin est traité comme ouvert (le joint au point
  de départ est remplacé par deux extrémités).
- **Réduction d'image très anisotrope** : la boîte de moyenne est bornée à 8 × 8 pixels du niveau
  de mipmap choisi ; au-delà, un léger aliasing peut subsister dans la direction la plus réduite.
- **Modes de fusion** appliqués pixel par pixel sur le fond immédiat (pas encore de groupes de
  transparence isolés / knockout, §11.4).
- `Bitmap::new` avec une taille impossible (dépassement ou plus de 1 Gio) rend un bitmap 0 × 0 ;
  `Bitmap::try_new` renvoie une erreur typée à la place.
- `encode_png` d'un bitmap 0 × 0 produit un fichier structurellement correct mais que les
  décodeurs stricts refusent (largeur nulle).

## Ajouter un test de rendu

Construire un `Bitmap`, dessiner, puis vérifier des pixels précis ou une aire de couverture
(`Mask::data()`), comme dans les modules. Pour inspecter visuellement un rendu, `encode_png`
écrit un PNG lisible par n'importe quel visualiseur.
