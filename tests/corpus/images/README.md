# Images à importer

Des fichiers **image**, et non des PDF : ils éprouvent les décodeurs BMP, GIF
et TIFF d'`acrux-features/stamp/image/`, qui servent à faire un PDF à partir
d'un scan ou d'une photo.

Ils sont écrits par **GDI+**, l'encodeur d'images de Windows : ce sont donc
des fichiers d'un autre programme que le nôtre, ce qui est tout l'intérêt —
un décodeur éprouvé sur les octets de son propre encodeur ne prouve rien.

| Fichier | Ce qu'il éprouve |
| --- | --- |
| `motif.png` | la référence : huit couleurs franches, lues par notre décodeur PNG |
| `motif-24.bmp` | BMP 24 bits, lignes de bas en haut |
| `motif.gif` | GIF : palette, LZW à codes de longueur variable |
| `motif-none.tif` | TIFF sans compression |
| `motif-lzw.tif` | TIFF LZW |
| `motif-packbits.tif` | TIFF PackBits |
| `damier.png` | la référence noir et blanc |
| `damier-24.bmp` | BMP d'une image sans couleur (sortie en niveaux de gris) |
| `damier-g4.tif` | TIFF CCITT Groupe 4, 1 bit par pixel — ce que produit un scanner |
| `deux-pages.tif` | TIFF **multipage** : une page par feuille numérisée |

Les huit couleurs du motif traversent sans perte la palettisation d'un GIF
comme celle d'un BMP : la comparaison au PNG de référence peut donc être
exacte, au pixel près.
