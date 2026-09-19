# Charte du projet Acrux

> Objectif : un logiciel PDF complet, open source, équivalent puis supérieur à Adobe Acrobat,
> écrit entièrement par nous, sans réutiliser de logiciel PDF existant.

## 1. Règles non négociables

### 1.1 Tout est écrit de zéro
- **Aucune bibliothèque PDF existante** (ni ouverte ni propriétaire) : pas de PDF.js, MuPDF, Poppler,
  pdfium, PDFBox, iText, pdf-lib, qpdf, Ghostscript, ni aucun fork ou copie de leur code.
- Le **parseur PDF**, le **moteur de rendu**, le **moteur d'édition**, le **rasteriseur de polices**,
  les **décodeurs d'images et de compression** et la **couche formulaires / annotations / signatures**
  sont développés dans ce dépôt.
- Ce qui est autorisé : le langage de programmation, sa bibliothèque standard, et les API du
  système d'exploitation (fenêtrage, fichiers, GPU/canvas, presse-papiers, imprimante, scanner).
- Interdit de « s'inspirer en copiant » : on lit les **spécifications** (ISO 32000, TrueType, CFF,
  JPEG, Deflate…), pas le code source d'autres lecteurs PDF.

### 1.2 Exceptions à valider explicitement (décision du porteur du projet)
Ces briques sont en dehors du domaine PDF et leur réécriture pose des questions de sécurité ou de
temps. Par défaut la charte impose de les écrire nous-mêmes, mais chaque exception doit être
tranchée et notée ici :
| Brique | Enjeu si écrite par nous | Décision |
|---|---|---|
| Cryptographie (AES, SHA-2, RSA, ECDSA, X.509) | Une crypto « maison » est un risque de sécurité reconnu ; les auditeurs le refusent | À trancher |
| Décodeurs JPEG / JPEG 2000 / JBIG2 / CCITT | Faisable, plusieurs semaines chacun, mais dans le domaine PDF | Écrit par nous |
| Deflate / LZW / RunLength / ASCII85 | Simple, quelques jours | Écrit par nous |
| Rasterisation de polices (TrueType, CFF, Type1, OpenType) | Cœur de la qualité de rendu | Écrit par nous |
| Moteur de rendu 2D (chemins, anti-aliasing, dégradés, transparence) | Cœur du projet | Écrit par nous |
| Fenêtrage / GUI de base | API OS autorisées | API OS ou toolkit générique non-PDF |
| Appels au système (`unsafe` FFI) | Rust impose `unsafe` pour appeler Win32 / Cocoa / X11 ; le workspace interdit `unsafe` partout ailleurs | Autorisé **uniquement** dans `crates/acrux-app/src/platform/` et dans `crates/acrux-setup/` (un installateur *est* du code système : fenêtre, registre, raccourcis COM), chaque bloc `unsafe` commenté avec l'invariant qu'il garantit, revue obligatoire. Le moteur PDF, lui, n'en contient pas une ligne |

### 1.3 Application native, pas web
- Le produit est un **exécutable natif** (`.exe` sur Windows, binaire sur macOS et Linux).
- **Interdit** : Electron, Tauri, WebView, application web, navigateur embarqué, HTML/CSS/JS
  comme technologie d'interface.
- Le rendu se fait directement sur la surface de la fenêtre (GPU ou CPU), pas dans un canvas web.
- Installateur léger, exécution hors ligne complète, aucune dépendance runtime à installer
  séparément (pas de « installez d'abord Node / .NET / Java »).
- Cibles : Windows 10/11 (x64, ARM64) en priorité, puis macOS et Linux.

### 1.4 Niveau de qualité attendu
- **Rendu** : identique au pixel près (à l'anti-aliasing près) à ce qu'Acrobat affiche, sur un
  corpus de test de plusieurs milliers de PDF réels, y compris les PDF cassés ou mal formés.
- **Édition** : modification de texte avec reflow de paragraphe, conservation exacte des polices
  (y compris incorporées et sous-ensembles), des couleurs, des espacements, des ligatures, des
  langues à écriture complexe (arabe, devanagari, CJK vertical). Aucune dégradation du fichier :
  ce qui n'est pas touché est réécrit octet pour octet ou structurellement à l'identique.
- **Robustesse** : ne jamais planter sur un fichier corrompu ; réparer ce qui peut l'être
  (table xref reconstruite, flux tronqués, polices manquantes substituées proprement).
- **Performance** : ouverture d'un PDF de 1 000 pages en moins d'une seconde, rendu d'une page
  standard en moins de 16 ms sur machine moyenne, fichiers de plusieurs Go sans charger tout en mémoire.
- **Un « truc bancal » n'est pas livré** : une fonctionnalité incomplète reste derrière un drapeau
  expérimental et n'est pas listée comme disponible.

### 1.5 Open source et contribuable par n'importe qui
- Licence libre permissive ou copyleft faible (recommandation : **Apache-2.0** ou **MPL-2.0**),
  choisie avant la première publication.
- **Un module = un dossier = un objectif**, avec un `README.md` par module expliquant : ce qu'il
  fait, la partie de la spécification qu'il implémente, comment le tester.
- **Code lisible avant code malin** : nommage explicite, fonctions courtes, commentaires qui
  renvoient au numéro de section de la spec (ex. `// ISO 32000-2 §7.5.4 Cross-reference table`).
- **Tests obligatoires** : chaque fonctionnalité arrive avec ses tests unitaires et un ou plusieurs
  PDF de test dans `tests/corpus/`. Tests de rendu par comparaison d'images de référence.
- **Zéro dépendance surprise** : la liste des dépendances (hors bibliothèque standard) tient sur
  une page et chaque entrée est justifiée.
- **Documentation d'architecture vivante** (`ARCHITECTURE.md`) mise à jour à chaque changement
  structurel, avec schémas.
- **Guide de contribution** (`CONTRIBUTING.md`) : installation en 3 commandes, conventions,
  comment ajouter une fonctionnalité, comment ajouter un test, comment lancer le corpus.
- **Issues balisées « bon premier ticket »** pour accueillir les débutants.
- Compilation en une commande, sur Windows, macOS et Linux, sans étape manuelle.
- Pas de service cloud obligatoire, pas de télémétrie, pas de compte requis.

## 2. Ce que « mieux qu'Acrobat » veut dire
Cibles concrètes à atteindre en plus de la parité (voir `FONCTIONNALITES_ADOBE_ACROBAT.txt`) :
- Démarrage instantané, empreinte mémoire faible, installateur léger.
- Linux supporté en première classe.
- IA locale et hors ligne (résumé, questions, OCR, suggestion d'alt-text) sans envoi de documents.
- OCR de qualité supérieure, y compris manuscrit.
- Historique de versions et comparaison sémantique intégrés (type Git pour PDF).
- API de scripting moderne et documentée (pas un JavaScript de 2005).
- Format de plug-in ouvert.
- Interface stable, cohérente, entièrement pilotable au clavier, accessible.
- Gratuit, sans abonnement, sans fonctionnalités verrouillées.

## 3. Ce que le projet n'est pas
- Pas un « wrapper » joli autour d'un moteur existant.
- Pas un prototype : chaque brique livrée est de qualité production.
- Pas dépendant d'Adobe (pas d'Adobe Fonts, pas de Document Cloud, pas de Sign).
