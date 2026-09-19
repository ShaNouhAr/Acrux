# Fonctions interactives : annotations, formulaires, navigation (ISO 32000-2 §12)

## En deux phrases
Tout ce qui est « par-dessus » la page vit dans `/Annots` : commentaires, surlignages, liens
et champs de formulaire (les *widgets*). Chaque annotation a un rectangle `/Rect`, un
sous-type, et **presque toujours** un flux d'apparence `/AP /N` qui est un XObject de
formulaire dessiné à la place de l'annotation — c'est ce flux, et non le sous-type, qui
décide de ce qu'on voit.

## Le format

### Une annotation
```
12 0 obj
<< /Type /Annot /Subtype /Highlight
   /Rect [120 100 300 120]
   /QuadPoints [120 120 300 120 120 100 300 100]   % haut-G, haut-D, bas-G, bas-D
   /C [1 1 0]                                       % couleur (gris, RVB ou CMJN)
   /CA 0.6                                          % opacité
   /F 4                                             % drapeaux (bit 3 = Print)
   /AP << /N 13 0 R >> >>
endobj
```
L'apparence est placée par l'**algorithme 8.1** (§12.5.5) : on transforme la `/BBox` du flux
par sa `/Matrix`, puis on met la boîte obtenue à l'échelle du `/Rect`. Ni la `/Matrix` ni la
`/BBox` ne sont donc « les coordonnées de la page ».

`/QuadPoints` donne quatre points **dans l'ordre haut-gauche, haut-droit, bas-gauche,
bas-droit** — pas dans l'ordre d'un quadrilatère à parcourir : un tracé naïf dessine un
papillon. Il faut lire les points 1, 2, 4, 3.

### Un champ de formulaire
Les champs forment un arbre sous `/AcroForm /Fields`, relié aux pages par les widgets :
```
<< /Type /Catalog /AcroForm << /Fields [20 0 R] /DR << /Font << /Helv 5 0 R >> >>
                               /DA (0 g /Helv 0 Tf) /NeedAppearances false >> >>

20 0 obj  << /T (adresse) /Kids [21 0 R 22 0 R] >> endobj          % nœud intermédiaire
21 0 obj  << /T (ville) /FT /Tx /V (Paris) /Parent 20 0 R
             /Type /Annot /Subtype /Widget /Rect [...] /P 3 0 R >> endobj
```
- Le **nom qualifié** est la suite des `/T` des ancêtres jointe par des points : `adresse.ville`.
- `/FT`, `/Ff`, `/V`, `/DV`, `/DA`, `/Q`, `/Opt`, `/MaxLen` **s'héritent** du parent ; `/DA` et
  `/Q` peuvent même venir de l'`/AcroForm`.
- Un champ **est souvent aussi son widget** (mêmes clés dans le même dictionnaire) quand il
  n'a qu'une apparition ; avec plusieurs, il a des `/Kids` widgets sans `/T`.
- Cases et boutons radio : `/V` est un **nom d'état** (`/Oui`, `/Off`), `/AS` choisit
  l'apparence dans `/AP /N << /Oui … /Off … >>`. Les états portent le nom d'export, pas
  l'étiquette affichée. Un groupe radio est un champ unique avec un widget par bouton.
- `/Opt` peut être `[(export) (affichage)]` par entrée : `/V` stocke l'export, l'utilisateur
  voit l'affichage.
- `/DA` est un mini flux de contenu (`0 g /Helv 12 Tf`) ; **taille 0 = taille automatique**,
  à calculer d'après la hauteur du widget puis à réduire si le texte déborde.

### Destinations et actions
```
/Dest [4 0 R /XYZ 0 200 0]        % explicite : page, cadrage, coordonnées
/A << /S /GoTo /D (chap2) >>      % nommée : résolue dans /Names /Dests (arbre de noms)
/A << /S /URI /URI (https://…) >>
```
Les cadrages : `/XYZ g h zoom` (valeurs absentes = inchangées, zoom 0 = inchangé), `/Fit`,
`/FitH h`, `/FitV g`, `/FitR x0 y0 x1 y1`, `/FitB`, `/FitBH`, `/FitBV`.
Les signets (`/Outlines`) sont une liste doublement chaînée `/First /Last /Next /Prev` avec
`/Count` signé : positif = nœud ouvert.

## Les pièges rencontrés dans les fichiers réels
- **Annotations sans `/AP`.** Les liens des documents LaTeX n'ont qu'un `/Border` et une `/C` :
  Acrobat trace lui-même le cadre. Les formes (`/Square`, `/Circle`, `/Line`, `/Ink`,
  `/Polygon`) et les marquages (`/Highlight`, `/Underline`, `/StrikeOut`) doivent aussi être
  synthétisés, sinon la page semble vide là où l'auteur a annoté.
- **`/Border [h v w]`** : la largeur est le **troisième** élément ; les deux premiers sont des
  rayons d'arrondi. Un quatrième élément optionnel est un motif de tirets.
- **Boucles**. `/Next` d'un signet peut pointer sur lui-même, `/Kids` d'un champ sur un
  ancêtre : tout parcours doit tenir un ensemble de visités et une profondeur maximale.
- **`/NeedAppearances true`** signifie « mes apparences sont fausses, regénère-les ». Beaucoup
  de fichiers l'oublient tout en ayant des apparences vides : régénérer à l'écriture est plus
  sûr que faire confiance au drapeau.
- **Polices absentes du `/DR`.** Un `/DA` peut nommer `/Helv` sans que `/DR /Font /Helv`
  existe : il faut ajouter la police standard soi-même.
- **Destinations nommées en deux endroits** : le dictionnaire `/Dests` du catalogue (PDF 1.1)
  *et* l'arbre de noms `/Names /Dests`. Les deux coexistent dans des fichiers réels.
- **Apparence d'un état absent** : `/AS /Oui` alors que `/AP /N` ne contient que `/Off`.
  On retombe sur l'unique entrée quand il n'y en a qu'une.

## Où c'est implémenté
| Sujet | Code |
|---|---|
| Inventaire, création, suppression d'annotations | `acrux_features::annotations` |
| Apparences par défaut (sans `/AP`) | `acrux_render::page::draw_default_appearance` |
| Placement de l'apparence (algorithme 8.1) | `acrux_render::page::draw_annotations` |
| Champs : inventaire, héritage, noms qualifiés | `acrux_features::forms::list_fields` |
| Remplissage et régénération des apparences | `acrux_features::forms::{set_field_value, regenerate_appearances}` |
| Aplatissement | `acrux_features::forms::flatten_fields` |
| Import / export FDF | `acrux_features::forms::{import_fdf, export_fdf}` |
| Destinations, actions, liens, signets | `acrux_features::navigation` |
| Interface : clic sur un lien, sur un champ, Tab | `acrux_app::viewer` |

## Les tests qui le couvrent
- `acrux_features::annotations` : création des quatre types avec apparence, relecture, suppression.
- `acrux_features::forms` : héritage et noms qualifiés, remplissage des cinq types de champs,
  refus des champs en lecture seule, aplatissement, aller-retour FDF, rendu (fond, coche).
- `acrux_features::navigation` : destination explicite, nommée par arbre de noms, action nommée,
  URI, arbre de signets avec `/Next` auto-référent (garde-fou).
- Corpus : `tests/corpus/synthese/annotations-sans-apparence.pdf` (neuf annotations sans
  `/AP`), `formulaire-acroform-champs.pdf` (champs remplis et rendus),
  `liens-destinations-signets.pdf` (liens, destinations, signets), chacun avec son image de
  référence dans `tests/reference/synthese/`.
