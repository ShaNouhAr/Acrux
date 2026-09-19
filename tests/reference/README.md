# Images de référence

Une image PNG par page de test, même chemin relatif que dans `tests/corpus/`,
suffixée du numéro de page : `syntaxe/xref-stream.pdf` → `syntaxe/xref-stream-p001.png`.

Résolution de référence : 72 dpi (1 pt = 1 px) sauf mention contraire dans le nom.

Le comparateur (`tools/compare-render`) échoue si plus de N pixels diffèrent au-delà d'un
seuil de tolérance d'anti-aliasing. Les rendus obtenus sont écrits dans `reference/actual/`
(ignoré par git) pour inspection visuelle.
