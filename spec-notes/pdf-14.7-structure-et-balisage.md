# Structure logique et balisage (ISO 32000-2 §14.6 à §14.8, PDF/UA ISO 14289-1)

## En deux phrases
Un PDF décrit *où* poser de l'encre, jamais *ce que le contenu veut dire* : « Chapitre 2 »
n'est qu'une suite de glyphes plus gros. Le **balisage** ajoute, à côté du contenu, un arbre
d'éléments (`Document`, `H1`, `P`, `Table`, `Figure`…) et des **marques** dans le flux de
contenu qui disent quel opérateur appartient à quel élément : c'est ce qui permet à une
synthèse vocale de lire dans le bon ordre, à un moteur de recherche de comprendre un tableau,
et à un lecteur de reflow de recomposer la page sur un petit écran.

## Le format

### Les deux moitiés, et le fil qui les relie
La structure vit dans le catalogue :

```
<< /Type /Catalog
   /MarkInfo << /Marked true >>          % « ce document est balisé »
   /Lang (fr-FR)                          % langue par défaut
   /StructTreeRoot 6 0 R >>

6 0 obj << /Type /StructTreeRoot
           /K [7 0 R]                     % l'arbre
           /ParentTree << /Nums [0 [8 0 R 9 0 R]] >>   % MCID → élément
           /ParentTreeNextKey 1 >> endobj

7 0 obj << /Type /StructElem /S /Document /P 6 0 R /K [8 0 R 9 0 R] >> endobj
8 0 obj << /Type /StructElem /S /H1 /P 7 0 R /Pg 3 0 R /K [0] >> endobj
9 0 obj << /Type /StructElem /S /P  /P 7 0 R /Pg 3 0 R /K [1] >> endobj
```

Le contenu, lui, porte des **marques** (§14.6) :

```
/P <</MCID 0>> BDC BT /F2 18 Tf 40 250 Td (Rapport annuel) Tj ET EMC
/P <</MCID 1>> BDC BT /F1 11 Tf 40 220 Td (Premier paragraphe.) Tj ET EMC
/Artifact BMC 0.5 G 40 40 m 260 40 l S EMC      % décor : hors de l'arbre
```

- `BDC` prend un **tag** et une **propriété** : soit un dictionnaire en ligne
  `<</MCID 0>>`, soit un nom à résoudre dans `/Properties` des ressources.
- Le `/MCID` est **unique dans son flux de contenu** : un élément qui couvre deux objets
  texte doit donc porter *deux* MCID, pas deux fois le même.
- `/K` d'un élément mélange des entiers (des MCID de la page `/Pg`), des dictionnaires
  `/MCR` (un MCID sur une *autre* page), des `/OBJR` (une annotation, un widget) et
  d'autres éléments.
- Le `/ParentTree` est un arbre de noms **numérique** : la clé est le `/StructParents` de la
  page, la valeur un tableau indexé par MCID qui donne l'élément propriétaire. C'est le
  chemin inverse (contenu → structure) dont un lecteur d'écran a besoin.
- `/Artifact` (§14.8.2.2) marque ce qui n'est **pas** du contenu : filets, fonds, numéros de
  page, en-têtes courants. Tout ce qui n'est ni un MCID ni un artéfact est du *contenu non
  balisé*, et PDF/UA le refuse.

### Les types normalisés
`Document Part Art Sect Div`, `P H H1…H6`, `L LI Lbl LBody`,
`Table TR TH TD THead TBody TFoot`, `Span Quote Note Reference BibEntry Code Link Annot`,
`Ruby RB RT RP Warichu WT WP`, `Figure Formula Form`, `Caption TOC TOCI Index NonStruct
Private BlockQuote`. Un producteur peut inventer son type à condition de le traduire dans
la `/RoleMap` du `/StructTreeRoot` (`/MonTitre /H2`).

Attributs utiles (`/A`, §14.7.6, dictionnaire ou tableau de dictionnaires, chacun avec son
propriétaire `/O`) : `/Scope /Column|/Row|/Both` sur un `TH`, `/Headers [(id1)]` sur un `TD`,
`/ColSpan` / `/RowSpan`, `/ListNumbering /Decimal|/Disc…` sur un `L`, `/BBox` et
`/Placement` pour la mise en page.

Textes de substitution : `/Alt` (ce qu'il faut lire à la place — obligatoire sur une
`Figure`), `/ActualText` (le texte réel, pour une ligature ou un logo), `/E` (développement
d'une abréviation), `/Lang` (changement de langue au milieu du document), `/T` (titre).

## Les pièges rencontrés dans les fichiers réels
- **`/MarkInfo /Marked true` sans arbre**, ou arbre sans `/ParentTree` : le document se
  déclare balisé mais aucun lecteur ne peut remonter du contenu à la structure. Les deux
  moitiés se vérifient séparément.
- **`/K` en tout genre** : un entier seul (pas de tableau), un dictionnaire élément direct
  (pas de référence), une référence vers un tableau de références. Le lecteur doit accepter
  les quatre formes, et tenir un ensemble de visités : les `/K` réels contiennent des cycles.
- **`/Pg` hérité**. Un élément sans `/Pg` prend celui de son ancêtre ; un `/MCR` peut en
  redéfinir un autre. Sans cette règle, la moitié des éléments se retrouvent « sans page ».
- **Marques à cheval**. `BDC … EMC` doit être **correctement imbriqué** avec `q`/`Q` et
  `BT`/`ET`. Beaucoup de producteurs ouvrent la marque avant `BT` et la ferment après `ET`,
  d'autres la mettent à l'intérieur : les deux sont légales, un `BDC` qui traverse un `ET`
  ne l'est pas. C'est la raison pour laquelle notre balisage automatique **coupe** sa marque
  à chaque `q`, `Q`, `BT`, `ET` et en rouvre une nouvelle avec un nouveau MCID.
- **`LI` sans `LBody`** : Chrome écrit `L > LI > (contenu)` sans `LBody`. Ce n'est pas
  conforme, mais c'est très répandu ; on le signale en avertissement, pas en erreur.
- **`/Artifact` oublié sur les filets de tableau** : ils deviennent alors du contenu non
  balisé et la lecture à voix haute s'interrompt sur du vide.
- **Ordre de l'arbre ≠ ordre géométrique**. Sur une page en deux colonnes, un ordre
  « de haut en bas » naïf est faux : c'est la découpe XY de `acrux_features::text` qui donne
  l'ordre de référence auquel on compare l'arbre.
- **Un paragraphe qui change de colonne** a une boîte englobante qui couvre toute la page.
  Le rattachement d'une tranche de flux à un élément se fait donc ligne par ligne, jamais
  sur la boîte du paragraphe.

## Ce que le balisage automatique fait, et ce qu'il ne peut pas faire
`acrux_features::accessibility::autotag` part de la mise en page reconstruite
(`acrux_features::text` : blocs, colonnes, paragraphes, titres par taille, listes à puces ou
numérotées, tableaux à filets ou à colonnes alignées) et :

1. rattache chaque tranche d'octets du flux (texte, image, tracé) à l'élément dont une des
   lignes la recouvre le mieux ;
2. réécrit le flux **octet pour octet** en n'y insérant que `BDC`/`EMC` ; les anciennes
   marques de balisage sont retirées, `/OC` (contenu optionnel) est préservé parce qu'il
   pilote la visibilité ;
3. renumérote les niveaux de titre pour qu'ils ne sautent pas (les niveaux de `text` sont
   des rangs de taille : deux tailles peuvent donner 1 et 3) ;
4. classe en `/Artifact` les tracés, les en-têtes et les pieds de page courants ;
5. rattache chaque annotation `/Link` et chaque widget de formulaire à un élément `Link` ou
   `Form` par un `/OBJR`, avec son `/StructParent` dans le `/ParentTree` et, pour un lien, le
   texte que son rectangle recouvre comme `/Alt` ;
6. écrit `/StructTreeRoot`, `/ParentTree`, `/StructParents`, `/MarkInfo` et `/Lang`.

Il **ne devine pas** : le texte de remplacement d'une figure, la portée réelle des en-têtes
d'un tableau au-delà de la première ligne, le découpage en sections (`Sect`), les notes, les
citations, ni le `Lbl` séparé d'un élément de liste (le marqueur reste dans le `LBody`), ni
l'info-bulle `/TU` d'un champ de formulaire. Sur
`tests/corpus/synthese/accessibilite-problemes.pdf`, il ramène le vérificateur de dix erreurs à
quatre : celles que seul l'auteur peut lever (`/Alt` d'une figure, `/TU` d'un champ) et celles
que `acr preflight --fix` écrit (`/Info /Title`, `/DisplayDocTitle`).

La langue est déduite d'un comptage de mots outils français et anglais ; en cas d'égalité la
fonction rend `None` plutôt qu'une langue fausse, car déclarer une mauvaise langue est pire
pour une synthèse vocale que de n'en déclarer aucune.

## Où c'est implémenté
| Fichier | Rôle |
|---|---|
| `acrux_features::accessibility` | types `StructTree` / `StructElement`, `read_structure` |
| `acrux_features::accessibility::marked` | parcours du flux : `/MCID`, `/Artifact`, tranches d'octets, aplats pour le contraste |
| `acrux_features::accessibility::check` | les 17 règles PDF/UA et WCAG, `check_accessibility` |
| `acrux_features::accessibility::autotag` | `autotag`, `detect_language` |
| `acrux_features::preflight` | profil `PdfUa1` : délègue au vérificateur ci-dessus |
| `acr check-a11y`, `acr autotag` | ligne de commande |

## Les tests qui le couvrent
- `acrux-features`, module `accessibility` : lecture d'un arbre correct (types, texte, page,
  boîte), documents fautifs pour chaque règle (figure sans `/Alt`, saut de niveau de titre,
  tableau sans `TH`, liste mal formée, contraste, police sans équivalent Unicode), et
  l'**aller-retour** `autotag` → `check_accessibility` sans erreur, y compris après
  enregistrement et relecture.
- `accessibility_corpus_triggers_its_documented_rules` : le fichier
  `tests/corpus/synthese/accessibilite-problemes.pdf` doit continuer à déclencher les dix
  règles annoncées dans son README.
- `cargo test -p acrux-render --test render_corpus` compare le rendu des deux fichiers de
  synthèse à leur image de référence.
