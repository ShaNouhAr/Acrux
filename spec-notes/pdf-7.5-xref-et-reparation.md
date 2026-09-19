# Tables de références croisées et réparation (ISO 32000-2 §7.5)

## En deux phrases
Un PDF se lit **par la fin** : le mot-clé `startxref` donne l'offset d'une table qui dit, pour
chaque numéro d'objet, où le trouver dans le fichier. Chaque enregistrement incrémental ajoute
une nouvelle table chaînée à la précédente par `/Prev` ; la plus récente l'emporte.

## Le format
Table classique (§7.5.4) :
```
xref
0 6                      ← sous-section : premier numéro, nombre d'entrées
0000000003 65535 f       ← entrée libre (f) : 20 octets exactement en théorie
0000000017 00000 n       ← objet 1 à l'offset 17, génération 0
...
trailer
<< /Size 6 /Root 1 0 R /Info 5 0 R /ID [<…> <…>] >>
startxref
1234
%%EOF
```
Flux xref (§7.5.8, PDF 1.5+) : un objet `<< /Type /XRef /W [1 4 2] /Index [0 6] /Size 6 … >>`
dont les données binaires contiennent les entrées, champ par champ, aux largeurs données par
`/W`. Le type 2 désigne un objet **compressé dans un flux d'objets** (`/Type /ObjStm`, §7.5.7).
Fichier hybride (§7.5.8.4) : une table classique dont le trailer a `/XRefStm` vers un flux xref
qui complète la table pour les lecteurs modernes.

## Les pièges rencontrés dans les fichiers réels
- Offsets faux (fichier édité à la main, transféré en mode texte, concaténé) : l'objet à
  l'offset n'a pas le bon numéro → on **reconstruit** en balayant le fichier (`repair.rs`).
- Octets parasites avant `%PDF-` : Acrobat compte alors les offsets à partir de `%PDF`.
  On essaie l'offset brut, puis l'offset décalé (`xref::header_offset`).
- Entrées de 19 ou 21 octets, `xref` sans `trailer`, sous-section plus courte qu'annoncée :
  on lit par tokens, pas par position fixe.
- Boucles `/Prev` : ensemble des offsets visités, limite de 512 sections.
- `startxref` absent ou hors du fichier, `/Root` manquant : reconstruction directe.
- Une entrée **libre** (`f`) signifie « objet supprimé » : la référence vaut `null`, on ne
  cherche pas à le ressusciter. Un numéro **inconnu** de la table déclenche en revanche la
  réparation (table incomplète).

## Reconstruction (`repair.rs`)
1. Balayage de `N G obj` (dernière occurrence de chaque numéro = la plus récente).
2. Trailers `trailer << … >>` fusionnés, puis clés des flux `/Type /XRef`.
3. Ouverture des flux d'objets pour y trouver les objets compressés (un objet trouvé
   directement dans le fichier garde la priorité).
4. `/Root` : celui du trailer s'il désigne un objet connu, sinon le premier `/Type /Catalog`.

## Où c'est implémenté
- `acrux_document::xref::read` — chaîne complète, `find_startxref`, `header_offset`.
- `acrux_document::repair::reconstruct` — balayage et reconstruction.
- `acrux_document::document::Document::get` — chargement avec vérification du numéro et
  bascule automatique vers la réparation.

## Tests
`xref::tests` (tables, flux, hybrides, boucles, sous-sections courtes), `repair::tests`,
`document::tests::broken_offsets_trigger_repair`, corpus `tests/corpus/reels/*casse*.pdf`,
et le fuzzing par mutation `crates/acrux-document/tests/mutations.rs`.
