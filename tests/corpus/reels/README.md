# Fichiers réels

PDF produits par des générateurs courants, avec leur source quand elle existe.
Les fichiers dont le nom contient `casse` sont volontairement corrompus : la
réparation automatique doit s'y déclencher (le test d'intégration l'autorise
uniquement pour eux).

| Fichier | Générateur | Ce qu'il exerce |
|---|---|---|
| `chrome-skia-2pages-texte-tableau-svg.pdf` | Chrome 153 (Skia/PDF), impression headless de `.source.html` | table xref classique, FlateDecode, polices TrueType incorporées (sous-ensembles), motifs (dégradé), ExtGState, arbre de structure, annotation lien, 2 pages |
| `chrome-skia-images-jpeg-png-tirets-opacite-cjk.pdf` | Chrome 153 (Skia/PDF) | images PNG (Flate, ICCBased) et JPEG (DCTDecode, 4:2:0), image réduite, tirets et pointillés, `ca` 0,5, ombre portée par SMask (luminosité), texte tourné, polices CJK/grec/cyrillique/arabe incorporées, dégradé radial, polyligne à joints ronds |
| `chrome-skia-stress-polices-degrades-fusion-colonnes.pdf` | Chrome 153 (Skia/PDF), impression headless de `.source.html` | 6 polices système (Segoe UI, Georgia, Courier New, Calibri, Times New Roman, Arial) en gras/italique, texte à 6 pt et espacé, texte en contour (mode de rendu 1), bloc pivoté et étiré, dégradés linéaire/radial/conique (fonctions et motifs d'ombrage), bordures tiretées et pointillées, ombre portée (SMask), `ca` 0,35, `BM /Multiply`, découpe, motif de pavage SVG, tracé avec dégradé et opacité, tableau, image SVG rastérisée, texte sur deux colonnes justifié avec puces et césure ; rendu comparé à la capture Chrome du même HTML : identique |
| `chrome-skia-rotate90-mise-a-jour-incrementale.pdf` | `acr rotate` sur le premier fichier | mise à jour incrémentale (`/Prev`), page 1 avec `/Rotate 90` (rendu paysage, sens horaire), page 2 inchangée |
| `chrome-skia-deux-colonnes-entete-pied-cesure.pdf` | Chrome 153 (Skia/PDF), impression headless de `.source.html` | extraction de texte : titre, deux colonnes CSS justifiées avec retraits de première ligne, paragraphe continué d'une colonne à l'autre, césure « docu-mentation », gras / italique, sous-titre, liste numérotée (étiquettes `/Lbl`), lignes alignée à droite et centrée, en-tête et pied de page (« Page 1 ») dans les marges (`@page`), A4, 1 page |
