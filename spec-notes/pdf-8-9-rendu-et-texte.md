# Rendu d'une page et texte (ISO 32000-2 §8 et §9)

## En deux phrases
Une page est un **flux de contenu** : une suite d'opérateurs (`m l c re f S BT Tj Do sh …`)
avec leurs opérandes, qui construit des chemins, les peint, place du texte et des images dans
un **espace utilisateur** en points (1/72 pouce), transformé vers l'écran par la **CTM**.
Le moteur (`acrux-render`) interprète ces opérateurs et délègue le dessin au rasteriseur
(`acrux-graphics`) et les contours de glyphes aux parseurs de polices (`acrux-fonts`).

## Le pipeline
```
flux de contenu ──ContentLexer──▶ opérations ──Renderer::execute──▶ chemins/texte/images
                                                    │
        état graphique (CTM, couleurs, trait, alpha, clip, texte)
                                                    ▼
   Path (espace utilisateur) ──× CTM──▶ Path device ──Rasterizer::fill_path──▶ Bitmap
   glyphe (unités/em) ──× [Tfs·Th 0 0 Tfs 0 Ts] × Tm × CTM──▶ Path device
   image (carré unité) ──× [1 0 0 -1 0 1] × CTM──▶ Paint::Image
```
- **Matrice de base** (`page::base_matrix`) : retourne l'axe Y (le PDF a l'origine en bas),
  met à l'échelle (dpi/72), applique `/Rotate` et l'origine de la `CropBox`.
- **q/Q** empilent tout l'état graphique, y compris la découpe. La découpe (`W n`) est un
  masque de couverture intersecté ; elle ne se « désintersecte » qu'au `Q`.
- **Couleurs** : l'espace courant (`cs`/`CS`) donne le nombre de composantes ; `sc`/`scn`
  les fixent ; `colorspace::to_rgb` convertit. Un espace `/Pattern` remplace la couleur par un
  motif (de pavage : on exécute la cellule à chaque position ; d'ombrage : un `Paint` dégradé).
- **Texte** : `Tf` charge une `LoadedFont` (dictionnaire PDF + programme incorporé ou police
  système de substitution) ; `Tj` décode les octets en glyphes (1 octet ou CMap), calcule pour
  chacun la matrice de rendu, remplit/trace/découpe selon `Tr`, puis avance
  `tx = (w0·Tfs + Tc + Tw·[espace]) · Th`.
- **XObjects** : `Do` dessine une image (décodée par `image.rs`, filtre d'image par
  `acrux-codecs`) ou un formulaire (flux imbriqué avec sa matrice, sa BBox comme découpe et ses
  ressources). Un formulaire avec `/Group` et alpha < 1 ou mode de fusion ≠ Normal est rendu
  hors écran puis composé (groupe de transparence).
- **ExtGState** (`gs`) : trait, alpha `CA`/`ca`, mode de fusion `BM`, masque souple `SMask`
  (le groupe est rendu hors écran, sa luminosité ou son alpha devient un masque).
- **Contenu optionnel** : `BDC /OC /nom … EMC` et `/OC` des XObjects sont ignorés si le groupe
  est OFF dans `/OCProperties /D`.

## Les pièges rencontrés dans les fichiers réels
- Polices non incorporées : substitution par une police système (Arial/Times/Courier/Symbol)
  choisie d'après le nom et les drapeaux ; les largeurs viennent de `/Widths` quand il existe.
- Polices symboliques TrueType sans `cmap (3,1)` : essayer `(3,0)` avec les codes `0xF0xx`,
  puis `(1,0)`, puis le code comme indice de glyphe (§9.6.6.4).
- `/Length` de flux faux, `endobj` manquants : gérés par `acrux-document`.
- Images JPEG CMJN de Photoshop stockées inversées (APP14) : inversion sauf si `/Decode`
  le fait déjà.
- Opérateurs avec trop peu d'opérandes : ignorés, on continue (Acrobat fait pareil).
- Budgets : 5 M d'opérations et 24 niveaux d'imbrication par page, délai optionnel.

## Extraction de texte (`acrux-features/text.rs`)
Même modèle de positionnement, sans dessiner : chaque glyphe reçoit une boîte et un texte
Unicode (`/ToUnicode`, sinon le nom de glyphe via la liste Adobe, sinon le code). Les glyphes
sont regroupés par ligne de base (dans la direction du texte) puis en mots (espace explicite ou
écart > 0,15 em). Le texte invisible (mode 3, couches OCR) **est** extrait, comme dans Acrobat.

## Où c'est implémenté
`acrux_render::interpreter::Renderer`, `acrux_render::page::render_page`, `acrux_render::font`,
`acrux_render::image`, `acrux_render::shading`, `acrux_features::text`.

## Tests
`crates/acrux-render/tests/render_corpus.rs` compare chaque page du corpus à son image de
référence (`tests/reference/`) ; en cas d'écart, le rendu obtenu et une image des différences
sont écrits dans `tests/reference/actual/`.
