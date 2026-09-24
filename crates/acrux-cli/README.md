# acrux-cli

Outil en ligne de commande acr (inspection, conversion, tests)

Voir `ARCHITECTURE.md` a la racine pour la place de ce crate dans l ensemble,
et `src/lib.rs` pour la liste des modules prevus.

## Annoter : `acr annotate`

```
acr annotate doc.pdf 1 arrow 50 50 200 120 --color 0,0,1 --width 3 -o a.pdf
acr annotate doc.pdf 1 circle 300 300 400 380 --fill 1,1,0 --opacity 0.5 -o a.pdf
acr annotate doc.pdf 1 ink --points "10,10 40,60 80,20;100,100 140,140" -o a.pdf
acr annotate doc.pdf 1 text 50 600 250 700 "Ligne 1\nLigne 2" --size 14 --border 1,0,0 -o a.pdf
acr annotate doc.pdf 1 callout 300 600 450 680 250 500 "Voir ici" -o a.pdf
acr annotate doc.pdf 1 polygon 100 100 150 180 200 100 -o a.pdf
```

Coordonnées en points de page, origine en bas à gauche. `line` et `arrow`
prennent `--head` et `--tail` (`open`, `closed`, `circle`, `square`, `butt`,
`none`) ; `text` et `callout`, `--font helvetica|times|courier[-bold|-italic]`,
`--size`, `--align left|center|right`, `--border r,g,b`, `--fill r,g,b`.
`acr annots` liste ce qui a été posé.

Tests : `cargo test -p acrux-cli`
