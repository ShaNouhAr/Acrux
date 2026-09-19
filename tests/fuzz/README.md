# Fuzzing

Cibles de fuzzing pour tout ce qui lit des octets non fiables : lexer, parseur d'objets,
xref, chaque codec, chaque parseur de police.

Objectif : aucune panique, aucune boucle infinie, aucune allocation démesurée, quelle que
soit l'entrée. Mis en place en phase 0/1 avec `cargo fuzz` (outil de développement,
pas une dépendance du produit).
