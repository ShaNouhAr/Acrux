# Dépendances externes

Règle (CHARTE_PROJET.md §1.5) : cette liste doit tenir sur une page et chaque entrée est
justifiée. Toute dépendance dans le domaine PDF (parseur, rendu, polices, codecs) est interdite.

| Crate | Utilisé par | Justification | Décision |
|---|---|---|---|
| *(aucune pour l'instant)* | | | |

## Candidats à discuter

| Crate | Besoin | Alternative « maison » | Statut |
|---|---|---|---|
| `windows` (bindings officiels Microsoft) | Fenêtrage Win32, GDI/Direct2D, impression, WIA | Déclarations FFI écrites à la main (beaucoup de `unsafe`) | À trancher en phase 3 |
| Bindings Cocoa / X11 / Wayland | Idem pour macOS et Linux | Idem | À trancher en phase 3 / 9 |
| Crypto (AES, SHA-2, RSA, ECDSA) | Chiffrement et signatures | Implémentation maison, déconseillée pour la sécurité | À trancher (voir charte §1.2) |
