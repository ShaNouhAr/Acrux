# Corpus de test

Un sous-dossier par fonctionnalité, des noms de fichiers descriptifs.

```
corpus/
  syntaxe/        xref classique, flux xref, hybride, incrémental, corrompu, sans EOF…
  filtres/        flate, lzw, ascii85, runlength, prédicteurs
  images/         dct, jpx, jbig2, ccitt, masques, inline
  polices/        truetype, cff, type1, type3, cid, encodages, non incorporées
  graphisme/      chemins, dégradés (types 1-7), motifs, transparence, blend modes
  couleur/        icc, lab, separation, devicen, indexed
  annotations/    les 28 types, avec et sans apparence
  formulaires/    acroform, calculs, xfa (lecture seule)
  signatures/     pades b/t/lt/lta, certifié, invalide
  securite/       rc4, aes-128, aes-256, permissions
  structure/      balises, pdf/ua, ordre de lecture
  edition/        cas de reflow, sous-ensembles de polices, colonnes, écritures complexes
  private/        vos fichiers non partageables (ignoré par git)
```

Règles :
- Fichiers libres de droits ou créés par le contributeur.
- Taille raisonnable (< 2 Mo sauf exception documentée).
- Chaque fichier a une image de référence dans `tests/reference/` dès que le rendu existe.
