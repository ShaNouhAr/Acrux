# Syntaxe lexicale et objets (ISO 32000-2 §7.2 et §7.3)

## En deux phrases
Un PDF est une suite d'**objets** (nombres, chaînes, noms, tableaux, dictionnaires, flux)
écrits en texte, sauf les données des flux qui sont binaires. Le découpage en tokens ne
dépend que de trois classes de caractères : espacements, délimiteurs, réguliers.

## Le format
```
%PDF-1.7
1 0 obj                          ← objet indirect numéro 1, génération 0
<< /Type /Catalog /Pages 2 0 R >>← dictionnaire ; `2 0 R` est une référence
endobj
4 0 obj
<< /Length 44 >>
stream                           ← puis CRLF ou LF, puis exactement /Length octets
BT /F1 12 Tf 72 712 Td (Bonjour) Tj ET
endstream
endobj
```
- Espacements (table 1) : `\0 \t \n \f \r ` `.
- Délimiteurs (table 2) : `( ) < > [ ] { } / %`.
- Nombres : `34`, `-3.62`, `.5`, `4.` ; **pas** d'exposant.
- Chaînes littérales `(…)` : parenthèses équilibrées, échappements `\n \r \t \b \f \( \) \\`,
  octal `\ddd`, `\` en fin de ligne = continuation. Hexadécimales `<4E6F>`.
- Noms `/Nom`, avec `#20` pour les caractères spéciaux.
- `null`, `true`, `false`.

## Les pièges rencontrés dans les fichiers réels
- Nombres malformés : `--5` (lu −5 par Acrobat), `3.4.5` (lu 3.4), `6-2` (lu 6), `.` seul (0).
- `/Length` faux ou référence vers un objet inexistant : on cherche `endstream` et on retire
  l'EOL qui le précède. Si `/Length` est juste, on l'utilise (les données peuvent contenir
  le mot `endstream`).
- `endobj` manquant, `]` ou `>>` manquants : les mots-clés `endobj`, `stream`, `obj`, `xref`
  ferment implicitement les conteneurs.
- Mots-clés parasites dans un tableau (`[1 foo 2]`) : ignorés, le reste est conservé.
- Une clé sans valeur juste avant `>>` vaut `null` ; une valeur `null` équivaut à une clé absente.
- Références : deux entiers **positifs** suivis de `R` ; `-1 0 R` n'en est pas une.
- Imbrication : limitée à 512 niveaux (fichiers hostiles).

## Où c'est implémenté
- `acrux_document::lexer::Lexer` — tokens ; sert aussi aux flux de contenu (mêmes règles).
- `acrux_document::parser::Parser` — `parse_object`, `parse_indirect` (avec résolution de
  `/Length` par rappel), fermeture implicite des conteneurs.
- `acrux_document::writer` — sérialisation inverse, déterministe.

## Tests
`lexer::tests`, `parser::tests`, `writer::tests::roundtrip_through_parser`, et le fuzzing
`tests/mutations.rs::random_bytes_never_panic_lexer_and_parser`.
