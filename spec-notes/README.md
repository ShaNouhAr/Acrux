# Notes de spécification

Résumés courts, en français, des sections de spécification implémentées, avec renvois vers
le code. But : qu'un contributeur comprenne un mécanisme en cinq minutes sans ouvrir les
1 000 pages de l'ISO 32000-2.

Une note par sujet : `pdf-7.5-xref.md`, `truetype-glyf.md`, `jpeg-huffman.md`…

Structure d'une note :
1. Ce que ça fait, en deux phrases.
2. Le format, avec un exemple d'octets réels.
3. Les pièges (fichiers réels non conformes que l'on doit accepter).
4. Où c'est implémenté (`crate::module::fonction`).
5. Les tests qui le couvrent.

Spécifications de référence :
- PDF : ISO 32000-2:2020 (PDF 2.0), ISO 32000-1:2008 (PDF 1.7, disponible gratuitement chez Adobe)
- PDF/A : ISO 19005 · PDF/X : ISO 15930 · PDF/UA : ISO 14289
- TrueType / OpenType : spécification OpenType (Microsoft), Apple TrueType Reference
- CFF : Adobe Technical Note #5176 · Type2 charstrings : #5177 · Type1 : « Adobe Type 1 Font Format »
- Deflate : RFC 1951 · zlib : RFC 1950 · JPEG : ITU-T T.81 · JPEG 2000 : ISO 15444-1
- JBIG2 : ITU-T T.88 · CCITT : ITU-T T.4 et T.6 · ICC : ICC.1:2022
- Signatures : ETSI EN 319 142 (PAdES), RFC 5652 (CMS), RFC 3161 (horodatage)
