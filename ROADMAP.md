# Feuille de route Acrux

Chaque phase se termine par une version utilisable et testée. Une fonctionnalité passe en
« disponible » uniquement quand elle est complète, testée sur le corpus et documentée.
Les durées sont des ordres de grandeur pour une petite équipe ; le projet est de longue haleine.

## Phase 0 — Fondations (décisions et outillage)
- [x] Choisir le langage (**Rust**, décidé le 18/09/2026) et la licence (**Apache-2.0**)
- [x] Squelette du dépôt (workspace Cargo, 9 crates), build, tests, Clippy et rustfmt validés le 18/09/2026 (MSVC)
- [~] Intégration continue : tests, formatage, lint (`.github/workflows/ci.yml`) ; fuzzing par mutation (`tests/mutations.rs`) ; comparaison d'images à venir
- [ ] Constitution du corpus de test (PDF variés, y compris corrompus), outil de comparaison de rendu
- [~] `CONTRIBUTING.md` écrit ; modèle d'issue et étiquettes à créer sur la forge

## Phase 1 — Lire un PDF (moteur de document)
- [x] Lexer, parseur d'objets, flux, xref classique et flux xref, hybrides, mises à jour incrémentales, flux d'objets
- [x] Réparation de fichiers corrompus (balayage, reconstruction du trailer, objets compressés)
- [x] Filtres : Flate, LZW, ASCII85, ASCIIHex, RunLength, prédicteurs PNG et TIFF (77 tests)
- [x] Chiffrement standard R2–R6 (RC4, AES-128, AES-256 ; MD5, SHA-2 ; mots de passe utilisateur et propriétaire)
- [x] Arbre des pages, héritage, boîtes de page, rotation, repli par balayage
- [x] Outil en ligne de commande `acr info / pages / dump / check`, validé sur un PDF réel (Chrome/Skia)
Livrable : inspecteur de PDF fiable sur 100 % du corpus. **Reste pour clore la phase 1** : élargir le corpus (fichiers d'autres générateurs), `structure` et `metadata` (XMP). Fait en plus : fuzzing par mutation, corpus de fichiers cassés.

## Phase 2 — Afficher une page (rendu)
- [x] Rasteriseur 2D : chemins, remplissage, contours, clipping, anti-aliasing (acrux-graphics, 49 tests, 3,7 ms/page A4) ; les masques de découpe retiennent la boîte de leur contenu, ce qui rend l'intersection proportionnelle à la découpe et non à la page
- [~] Espaces colorimétriques : Device*, Lab, Indexed, Separation, DeviceN, ICC via alternatif (parseur ICC réel à faire)
- [x] Polices TrueType, CFF, Type1, Type3, encodages, CMaps (acrux-fonts, 160 tests avec la composition) ; polices de secours via les polices système (CMaps CJK prédéfinies à faire)
- [x] Interpréteur de contenu : chemins, texte, images, XObjects, formulaires, motifs de pavage et d'ombrage, contenu optionnel, ExtGState
- [x] Décodeurs d'images : DCT (JPEG de base et progressif), CCITT G3/G4, JBIG2 (générique, symboles/texte, raffinement, demi-teintes, globaux), JPX (JPEG 2000 partie 1, conteneur JP2, `/SMaskInData`) — tous branchés dans `acrux-render/image.rs`
- [x] Ombrages types 1 à 7 (maillages en Gouraud, patches subdivisés), motifs
- [~] Transparence : alpha constant, 16 modes de fusion, masques souples (luminosité/alpha), groupes hors écran (knockout/isolated à affiner)
- [x] Apparences d'annotations (/AP /N, /AS, algorithme 8.1)
- [x] Extraction de texte structurée (`acrux-features/text.rs`, `acr text`, recherche par boîtes) ; mise en page reconstruite en phase 5
Livrable : rendu conforme à Acrobat sur le corpus, mesuré par comparaison d'images. **État** : premier PDF réel rendu à l'identique (`acr render`, test `render_corpus` avec images de référence). Reste : élargir le corpus (scans JPEG, CJK, Type 3, transparence complexe) et corriger les écarts.

## Phase 3 — Application native de lecture
- [~] Fenêtre native Win32 (`acrux-app/platform`), tampon de pixels avec sous-vues, DPI par moniteur (dès la création), glisser-déposer, presse-papiers, curseurs, ouverture d'URL ; toolkit interne (`ui/text` texte d'interface via acrux-fonts + acrux-graphics, `ui/theme` sombre/clair, `ui/input` champ de saisie, `ui/icons` icônes vectorielles, `ui/toolbar` barre d'outils avec champ de page et zoom, `ui/palette` palette de commandes, barre d'état, info-bulles des boutons — libellé et raccourci repris de la palette, source unique)
- [~] Ouverture (dialogue, ligne de commande, dépôt, mot de passe demandé pour les documents chiffrés), défilement continu, zoom, ajustement largeur, rendu sur fil séparé (interface jamais figée) ; rotation faite ; plein écran (F11), dispositions continu / page unique / deux pages (`ui/prefs.rs`), onglets (un par document, `ui/tabs.rs`, plusieurs fichiers en ligne de commande, Ctrl+Tab, Ctrl+W) et écran d'accueil avec les documents récents faits ; mode lecture à faire
- [~] Navigation : liens cliquables (destinations explicites et nommées, actions nommées, URI ouvertes dans le navigateur, curseur main), signets (`acrux-features/navigation.rs`, `acr links/outline`) ; panneau latéral (F4 ou bouton) avec vignettes rendues par le fil de rendu (réordonnables par glisser-déposer) arbre de signets dépliable, liste des commentaires (clic = aller au commentaire) et calques cochables (`ui/panel.rs`, `acrux_render::layers` + `RenderOptions::layers`) ; onglet « Fichiers » des **pièces jointes** (clic = enregistrer par le dialogue système, bouton = joindre un fichier) ; **étiquettes de page** `/PageLabels` affichées et acceptées en saisie dans le champ de page et la barre d'état (`acrux-features/pagelabels.rs`, Ctrl+G) ; portfolios (`/Collection`) à faire
- [x] Recherche : moteur (`text::find`) et interface (Ctrl+F, champ de saisie interne, surlignage des occurrences, Entrée / Maj+Entrée, compteur, parcours incrémental par tranches de 12 ms qui ne fige jamais la fenêtre — 210 pages et 2 790 occurrences restent fluides)
- [x] Sélection de texte (`acrux-app/selection.rs` : curseurs entre glyphes, glisser, double / triple clic, Maj+clic, multi-pages, curseur en I) et copie dans le presse-papiers (Ctrl+C, Ctrl+A)
- [~] Impression (dialogue système, plage de pages, ajustement à la zone imprimable, rendu à la résolution de l'imprimante), plein écran (F11), navigation des champs au clavier (Tab / Entrée), préférences persistées (thème, disposition, zoom, panneau, fenêtre, fichiers récents) ; raccourcis configurables et localisation à faire
- [x] Palette de commandes (Ctrl+Maj+P, `ui/palette.rs`) : toutes les commandes de l'application avec leur raccourci, filtrage par fragment, par sous-séquence et par synonyme, accents optionnels dans les deux sens ; tient lieu de barre de menus et de documentation vivante des raccourcis
- [x] Export depuis l'application (Ctrl+E ou palette) : format déduit de l'extension, conversion faite par le fil de rendu, compte rendu dans la barre d'état
- [x] Édition du texte dans l'application (sélectionner, `E`, invite pré-remplie) : `EditOp::EditText` passe par `acrux-features::edit_text`, la police et la couleur sont conservées, la suite de la ligne se décale ou se rapproche sans jamais se recouvrir
- [x] Organisation des pages dans l'application : pivoter, supprimer, réordonner par glisser-déposer, dupliquer, insérer les pages d'un autre fichier (Ctrl+I), extraire une page
- [x] Identité de l'exécutable (`crates/acrux-winres`, `build.rs` de `acrux-app` et `acrux-cli`) : icône Windows (`RT_GROUP_ICON` + `RT_ICON`, les sept tailles de `branding/acrux.ico`) et bloc `VS_VERSIONINFO` (nom du produit, description, version, licence) écrits par nos soins, sans `rc.exe` ni bibliothèque externe ; la fenêtre charge la ressource avec `LoadImageW` aux tailles du système, pour la barre de titre et la barre des tâches. Installateur fait (`acrux-setup`) ; reste la signature Authenticode
- [~] Accessibilité de l'interface : tout est atteignable au clavier (F6 change de zone, flèches et Entrée dans la barre d'outils et le panneau, anneau de focus visible), mode lecture (F5) ; lecteurs d'écran (UI Automation) à faire
- [x] Tests d'interface sans rien afficher : `ACRUX_HEADLESS` cache la fenêtre et remplace les dialogues système par des variables d'environnement, le pilotage passe par `PostMessage` (harnais `tools/headless.ps1`)
Livrable : lecteur PDF complet, rapide, installable, version 0.1 publique.

## Phase 4 — Écrire et organiser
- [x] Sérialisation et écriture incrémentale sans dégrader le fichier (`Document::save_incremental` / `save_full`, chiffrement conservé)
- [~] Organisation des pages : insérer, supprimer, extraire, réordonner, pivoter, fusionner (faits, `acr rotate/delete/reorder/extract/merge`) ; dans l'application : pivoter et supprimer la page courante, réordonner par glisser-déposer dans les vignettes (repère d'insertion), Enregistrer / Enregistrer sous, confirmation avant perte de modifications ; fractionner et recadrer à faire
- [x] Pièces jointes (`acrux-features/attach.rs`, `acr attachments/attach/detach`) : lecture de l'arbre de noms `/Names /EmbeddedFiles` **et** des annotations `/FileAttachment` (une pièce présente des deux côtés n'est listée qu'une fois), extraction avec vérification de la somme MD5 `/Params /CheckSum`, ajout d'un flux `/EmbeddedFile` compressé en Flate avec `/Size`, `/CreationDate`, `/ModDate`, `/CheckSum` et `/Subtype` MIME, arbre de noms **reconstruit trié et équilibré** à chaque écriture comme l'exige §7.9.6, icône de trombone dessinée sur la page demandée, retrait complet (entrée, objets et annotations) ; corpus `synthese/pieces-jointes-etiquettes.pdf`. À faire : portfolios (`/Collection`), `/AF` (fichiers associés PDF 2.0), pièces jointes dans un document chiffré autrement que par le chiffrement global
- [x] Étiquettes de page (`acrux-features/pagelabels.rs`, `acr labels/set-labels`) : lecture de l'arbre de nombres `/PageLabels` (`/S` parmi `D r R a A`, `/P`, `/St`), rendu des chiffres romains et des étiquettes alphabétiques **répétées** comme l'exige §12.4.2 (z puis aa, bb), repli décimal, écriture d'un arbre trié et équilibré, recherche d'une page par son étiquette ; affichées par l'application
- [~] Annotations : inventaire, création (carré, surlignage, note, lien) avec apparences, suppression (`acr annots/annotate`) ; dans l'application : surligner la sélection (`H`), poser une note (`N`) ; apparence par défaut synthétisée pour les annotations sans `/AP` (carré, cercle, ligne, encre, polygone, surlignage, soulignement, barré, texte libre) ; les autres types, réponses, statuts, sélection / suppression d'annotations à la souris, FDF/XFDF à faire
- [x] Filigranes, en-têtes / pieds de page, numérotation Bates, arrière-plans (`acrux-features/stamp.rs`, `acr watermark/background/header-footer/bates/unstamp`) : texte ou image (PNG, JPEG incorporé tel quel), neuf ancrages avec décalage, rotation et échelle absolue, relative à la page ou étirée, opacité par `/ExtGState`, devant ou derrière le contenu, plage de pages ; le dessin est un XObject de formulaire **partagé par toutes les pages** et le contenu d'origine n'est jamais réécrit (encadrement `q`/`Q`), donc la pose est réversible au pixel près ; en-têtes et pieds à six zones avec jetons `{page} {pages} {date} {time} {datetime} {filename}`, numéro de départ et formats 1, i, I, a, A ; numérotation Bates continue d'un fichier au suivant ; texte posé avec une police standard et un `/ToUnicode`, donc extractible
- [~] Annuler / rétablir dans l'application (`Ctrl+Z` / `Ctrl+Y`) : l'historique des modifications est rejoué depuis le fichier, ce qui évite d'avoir à inverser chaque opération ; historique de versions et annulation dans la bibliothèque à faire
- [~] Compression / optimisation, aplatissement : `acr rewrite` réécrit proprement (flux d'objets dépliés), `--compress` recompresse les flux non filtrés (compresseur DEFLATE maison, `acrux-codecs/flate/compress.rs`), `--compact` écrit les objets dans des flux d'objets avec une table xref compressée et retire les objets inatteignables (`SaveOptions::{object_streams, drop_unreferenced}`, `Document::reachable_objects`) ; recompression d'images, linéarisation à faire
Livrable : version 0.2, parité avec Acrobat Standard hors édition de texte.

## Multimédia

- [x] **Vidéo et son dans le document** (`acrux-features/media.rs`, `acrux-app/src/platform/media/`,
  `acr media`) : les trois formes du multimédia PDF sont reconnues — `/Screen` avec action
  `/Rendition` (§13.2), `/RichMedia` (§13.7) et `/Movie` (PDF 1.1) —, en descendant action →
  rendition → clip → spécification de fichier, sélecteurs et sections de clip compris. Extraction du
  fichier incorporé sous un nom **choisi par nous** ; un média extérieur au document n'est jamais
  ouvert de lui-même. Lecture par Media Foundation (`IMFSourceReader`, sortie BGRA) et `waveOut`,
  avec **le son pour horloge** : la position vient de ce que la carte son a réellement joué. Les
  images sont composées dans la page — la vidéo suit le défilement, le zoom et la découpe, sans
  fenêtre flottante — avec mise à l'échelle bilinéaire, barre de commandes et déplacement sur la
  ligne de temps. Règle de contrôle en amont ajoutée : PDF/A et PDF/X refusent le multimédia.
  À faire : volume et sourdine, plein écran, `/RichMedia` à configurations multiples, lecture
  automatique quand le document la demande (`/AA /PV`), sous-titres.
- [x] **Le son seul, y compris sans image** (`/Sound`, §13.3) : une annotation `/Sound` ne porte pas
  un fichier mais des échantillons bruts — fréquence, voies, bits, codage — posés tels quels dans
  un flux. Ils sont maintenant reconnus, listés avec leurs paramètres, et extraits sous forme de
  WAV : octets retournés (le PDF est gros-boutien, le WAV petit-boutien), non signé recentré, lois
  µ et A de la téléphonie décodées en PCM 16 bits. Côté lecture, un média sans piste vidéo ne se
  terminait jamais — la fin se jugeait à la seule vidéo — et le reliquat de son en fin de flux
  n'était jamais confié à la carte, donc la dernière fraction de seconde manquait. Les deux sont
  corrigés, et un test le vérifie sur un fichier **silencieux fabriqué à la volée** : une suite de
  tests qui se met à chanter est une suite qu'on finit par ne plus lancer.

## Distribution

- [x] **Installateur Windows** (`crates/acrux-setup`, `acrux-setup.exe`) : exécutable
  auto-extractible écrit de zéro — aucun WiX, NSIS ni Inno Setup. Archive collée en fin de fichier
  (compressée par notre Flate), fenêtre en contrôles Windows standard, installation dans le compte
  de l'utilisateur **sans droits d'administrateur**, raccourcis menu Démarrer et Bureau par COM
  `IShellLink`, inscription dans « Applications installées », association `.pdf` proposée sans être
  imposée, désinstallation complète (ce qui résiste est confié à `MoveFileEx` pour le prochain
  démarrage). Modes `--silent`, `--uninstall`, `--pack`. Un nom de fichier d'archive ne peut jamais
  écrire hors du dossier choisi (test dédié).
- [x] **Mises à jour** (`acrux-app/src/update/`) : interrogation de la page des versions du dépôt
  par WinHTTP, au plus une fois par jour, désactivable par `mises-a-jour=0` ; lecteur JSON complet
  écrit pour l'occasion ; téléchargement restreint au domaine de publication de GitHub ; installation
  silencieuse par l'installateur, jamais sans un oui explicite. Remplacer un exécutable en cours
  d'exécution est possible : l'ancien est renommé de côté, ce que Windows autorise.
- [x] **Publication automatique** (`.github/workflows/release.yml`, `tools/release.ps1`) : une
  étiquette `vX.Y.Z` déclenche la construction, l'installateur, l'archive portable, les sommes
  SHA-256 et la page de version. Le workflow refuse de publier si l'étiquette ne correspond pas à la
  version de `Cargo.toml`. Aucune action tierce : le jeton fourni par GitHub et `gh` suffisent.
- [x] **Métriques des quatorze polices standard, sans police système** (`acrux-fonts/standard.rs`)
  — les tables AFM sont descendues sous le rendu comme sous l'écriture, une seule fois pour tout
  le projet. Un `/BaseFont /Helvetica` sans `/Widths` ni descripteur (§9.6.2.2) compose désormais
  aux largeurs d'Adobe, qu'il y ait ou non une police installée, et les noms courants (`ArialMT`,
  `TimesNewRomanPSMT`, `CourierNew`) sont reconnus comme leurs équivalents. Symbol a sa table et
  **son encodage** : le code 0x61 y est `alpha`, plus `a`. Les tables sont recoupées par un test
  avec les polices métriquement compatibles du système, ce qui a déjà rattrapé trois erreurs (le
  `i` accentué se bâtit sur `dotlessi`, `quotedblbase` de Times-Italic, `oe` d'Helvetica-Bold) et
  comblé huit manques (`Ð Þ ð þ × ÷ ø ß`). Reste : les largeurs de ZapfDingbats, volontairement
  absentes plutôt qu'inventées.
- [ ] Signature Authenticode des exécutables (certificat commercial), paquet `winget`
- [ ] Portages macOS et Linux, et leurs formats de paquet

## Fidélité d'affichage

- [x] **Un glyphe de substitution occupe la largeur déclarée** (`acrux-render/font.rs`,
  `substitution_fit`) : quand la police n'est pas incorporée, celle qu'on lui substitue n'a pas ses
  chasses. Dessinée telle quelle, chaque lettre flotte dans la place que le document lui réserve ou
  en déborde, et la ligne se disloque. Le contour est donc étiré horizontalement jusqu'à l'avance
  déclarée — ce que fait Acrobat avec ses polices à axes variables, en mieux dessiné. Les bornes
  0,2 à 5 évitent d'étirer un glyphe qui n'a manifestement rien à voir.
- [x] **Index des polices installées, par leur nom** (`acrux-render/font/system.rs`,
  `acrux-fonts/names.rs`) : un document qui réclame Calibri, Georgia ou Consolas sur une machine
  où elles sont installées recevait Arial. On lit maintenant la table `name` de chaque police —
  quelques kilo-octets par fichier, jamais le fichier entier — et on cherche par nom PostScript,
  nom complet, puis famille et style. 364 polices indexées en 16 ms sur une machine d'essai,
  recherche en 78 ns, index construit seulement à la première police absente rencontrée. C'est la
  seule méthode portable : deviner `calibrib.ttf` ne marche que sur Windows. Les recours
  descendent ensuite du plus fidèle au moins, et **notre propre dessin ne sert que si la machine
  n'a aucune police** — un test le vérifie.
- [x] **Police de secours dessinée par nous** (`acrux-fonts/fallback.rs`) : sur une machine sans
  aucune police utilisable — conteneur, image de compilation minimale — la page restait blanche
  alors que le fichier était valide. Acrobat embarque pour ce cas Adobe Sans MM ; voici la nôtre.
  Une linéale géométrique couvrant tout WinAnsiEncoding : les 95 caractères ASCII imprimables, les
  accents composés à la volée sur leur lettre, les ligatures et les signes courants. Chaque lettre
  est un **squelette** — quelques traits et quelques arcs — épaissi ensuite d'une plume constante,
  ce qui la rend courte à écrire et impossible à remplir à l'envers ; quelqu'un qui veut corriger
  un `g` déplace un nombre et regarde. `substitution_fit` la met ensuite à la chasse déclarée, si
  bien que les lignes tombent juste. Vérifié par `crates/acrux-render/tests/fallback_font.rs`, qui
  coupe l'accès aux polices du système avec `ACRUX_FONT_DIR`.
  Reste : l'alphabet grec de Symbol et les fleurons de ZapfDingbats, qui restent vides plutôt que
  de sortir une lettre latine à leur place.

## Interface

- [x] **Barre des outils à droite** (`acrux-app/src/ui/tools.rs`) : une barre en haut ne tient que ce
  qui sert à lire ; les outils de travail finissaient derrière des raccourcis que personne ne
  devine. Quinze outils groupés par métier (modifier, commenter, signer, pages, protéger,
  document), l'outil en cours allumé, `F3` pour l'ouvrir et la fermer, effacement automatique sur
  une fenêtre étroite. La colonne rend une `Command` — la même que la palette et le clavier —,
  donc un outil ne peut pas se comporter autrement ici qu'ailleurs.
- [x] **Zoom d'ouverture automatique** : ajuster à la largeur sur un écran large donnait 212 %.
  Trois modes désormais — automatique (largeur, plafonnée à 100 %), largeur, page entière —, le
  bouton les parcourt et la barre d'état dit lequel est actif.
- [x] **Mode « Modifier le PDF »**, comme dans Acrobat (`acrux-app/src/viewer/editmode.rs`,
  `acrux-app/src/ui/editpdf.rs`, `acrux-features/edit_text/reflow.rs`) : on n'a plus à sélectionner
  d'abord. On entre dans le mode, **tout le texte s'encadre**, un clic dans un bloc y pose le curseur
  et l'on tape, efface, se déplace (flèches, Ctrl+flèches, Début/Fin, Maj pour sélectionner,
  double-clic sur un mot, triple-clic sur le bloc, Ctrl+A/C/X/V). Le bloc se recompose **à chaque
  frappe, dans sa vraie police et sur son vrai fond** — environ 25 ms sur une vraie page, rendu
  compris — donc ce qu'on voit est ce qui sera enregistré. Entrée coupe la ligne. Cliquer dans un
  bloc ne déplace rien : le curseur se pose sur le dessin d'origine, la recomposition n'arrive
  qu'à la première frappe. **Ajouter du texte** pose une zone neuve où l'on clique, qui grandit en
  largeur jusqu'à la marge puis coule à la ligne ; rien n'est écrit tant qu'aucune lettre n'est
  tapée. Barre du mode : outil, taille (du bloc en cours ou du texte ajouté), couleur du texte
  ajouté, Terminer. **Annuler défait toute une saisie d'un coup** : l'historique ne garde qu'une
  opération par bloc, rejouable sur le fichier d'origine (vérifié par test). Les blocs sont ceux
  d'Acrobat et non les paragraphes de l'extraction : un paragraphe qui se poursuit dans la colonne
  suivante fait deux blocs, un en-tête « titre à gauche, date à droite » aussi — les recomposer d'un
  seul tenant coulerait le texte par-dessus l'autre colonne. Sûreté : un bloc n'est recomposé que
  s'il porte exactement le texte attendu ; s'il s'est mêlé à un voisin, la frappe est refusée
  plutôt que d'écraser ce voisin.
  À faire : gras, italique, famille et couleur d'un bloc existant ; images dans le même mode
  (« Modifier les objets » reste un outil à part) ; texte ajouté hors WinAnsi (police standard) ;
  poignées pour élargir une zone.
- [x] **Outils d'annotation en mode**, eux aussi : « Surligner », « Poser une note » et « Biffer »
  s'allument dans la colonne et agissent directement sur la page — glisser surligne ou marque, un
  clic pose une note — avec une barre qui dit l'outil en cours et comment en sortir. Du texte déjà
  sélectionné est traité dès qu'on choisit l'outil.
- [ ] Menus classiques, info-bulles détaillées, personnalisation de la colonne d'outils.

## Phase 5 — Édition de niveau Acrobat (le cœur du « mieux qu'Acrobat »)
- [x] Reconstruction de paragraphes depuis le contenu, détection des colonnes et des styles (`acrux-features/text/` : blocs, paragraphes avec alignement, retrait et interligne, césures réparées, colonnes par découpe XY, en-têtes et pieds de page, listes, titres, tableaux à filets ou à colonnes alignées, styles gras / italique / couleur au glyphe près ; sorties `to_plain`, `to_markdown`, `to_html`, `to_layout`, `acr text --markdown|--html|--layout`) ; reste : ordre de lecture depuis le balisage (`/StructTreeRoot`), tableaux à cellules fusionnées, texte vertical CJK
- [~] **Édition de texte in-place avec reflow**, changement de police / taille / couleur / alignement
  (`acrux-features/edit_text.rs`, `acr edit-text/reflow`) : `apply_edits` remplace une plage de
  glyphes de `PageText` (même repérage que l'extraction) en retrouvant le `Tj`/`TJ`/`'`/`"` qui l'a
  dessinée, en découpant la chaîne au bon octet et en réencodant avec la police en place (simple ou
  composite) ; le positionnement est conservé par des ajustements `TJ` calculés pour que la matrice
  texte soit, à la fin de chaque opération réécrite, **exactement** celle d'avant l'édition — le
  texte qui suit ne bouge pas d'un point. `reflow_paragraph` recompose un paragraphe entier dans sa
  boîte (retour à la ligne aux espaces, interligne, alignement `Left/Right/Center/Justify` par
  ajustements `TJ`, retrait de première ligne, réduction de taille facultative pour tenir).
  Corpus : `synthese/texte-edite-remplacements.pdf`, tests sur les deux PDF Chrome réels (autres
  lignes immobiles à 0,01 pt près, rendu identique au pixel près hors de la ligne éditée).
  Reste : édition interactive dans l'application, texte des XObjects de formulaire, polices Type 3,
  écritures complexes
- [~] **Gestion des sous-ensembles de polices** (ajout de glyphes, substitution propre)
  (`acrux-fonts/subset.rs`) : production d'une police TrueType valide à partir d'une liste de glyphes
  (tables `head hhea maxp hmtx loca glyf cmap name post` recalculées, `glyf` recopiée octet pour
  octet avec renumérotation récursive des composantes, fusion de plusieurs polices sources). Quand
  un caractère manque au sous-ensemble incorporé, `edit_text` fusionne la police système de la même
  famille en gardant les indices de glyphes existants (`/CIDToGIDMap /Identity`), complète `/W` et
  régénère `/ToUnicode` ; sinon il replie sur la police standard la plus proche avec un
  avertissement. Reste : sous-ensembles CFF, réduction d'un sous-ensemble aux glyphes réellement
  utilisés
- [~] Écriture hors WinAnsiEncoding : le texte que nous ajoutons (filigrane, en-tête, Bates) incorpore une police système en sous-ensemble `/Type0` quand il le faut (`acrux-features/fontembed.rs`), donc japonais, chinois, coréen, grec et cyrillique fonctionnent ; `EmbeddedFont::shape_line` compose la ligne quand on le demande
- [~] **Shaping : ligatures, crénage, écritures cursives, bidirectionnel, CJC vertical**
  (`acrux-fonts/opentype/` et `acrux-fonts/shape.rs`, note `spec-notes/opentype-gsub-gpos.md`) :
  lecture de `GSUB` (types 1 à 8, formats 1 à 3 des contextuelles, extensions), de `GPOS`
  (types 1 à 9 : crénage par paires explicites et par matrice de classes, cursif, marque vers
  base, vers ligature et vers marque), de `GDEF` (classes, jeux de marques filtrants) et de la
  table `kern` ancienne en repli ; données Unicode écrites à la main (`unicode/`) : algorithme
  bidirectionnel UAX #9 (règles P, X, W, N, I, L), grappes de graphèmes UAX #29, classes de
  jointure arabes, classes combinatoires canoniques. API `shape(font, texte, &ShapeOptions)
  -> Vec<ShapedGlyph { gid, cluster, advance, offset_x, offset_y }>`, où `cluster` est un
  **indice d'octet** dans la chaîne d'origine (curseur et sélection justes). Mode vertical
  (`vert`/`vrt2`, `vhea`/`vmtx`/`VORG`). Vérifié sur les polices de la machine : ligature `fi`
  de Calibri, `dlig` de Georgia, crénage de `AV` et `To` sur six polices, quatre formes arabes
  distinctes, marques ancrées, bidi latin/hébreu, CJC vertical, et non-régression de la
  composition nue. Reste : les écritures **syllabiques** (indiennes, khmère, birmane, thaïe,
  tibétaine), le syriaque, la règle N0 du bidi (parenthèses appariées), les polices variables
- [x] **Édition d'images et d'objets** (`acrux-features/edit_objects/`, `acr objects` /
  `edit-object`, touche `O` dans l'application) : inventaire des objets dessinés par une page —
  images externes et en ligne, XObjects de formulaire, tracés vectoriels, dégradés, blocs de texte —
  avec pour chacun sa nature, sa boîte en espace page, la matrice du tracé, la **plage d'octets** qui
  le dessine et la découpe en vigueur. Déplacer, redimensionner autour du centre, pivoter, amener
  dans un rectangle, recadrer, réordonner (devant / derrière / d'un cran), supprimer, remplacer
  l'image, aligner plusieurs objets sur leur enveloppe.
  **Tout passe par la réécriture chirurgicale** : la plage de l'objet est encadrée, le reste du flux
  recopié octet pour octet. La matrice demandée est exprimée en espace page et devient
  `N = C · M · C⁻¹` ; un bloc de texte est encadré par `N cm … N⁻¹ cm` et non par `q … Q`, qui
  reprendrait la police qu'il laisse derrière lui ; une découpe est posée dans l'espace de l'objet
  pour ne jamais toucher à sa matrice. La boîte d'un bloc de texte vient de l'extraction de texte,
  appariée **par indice d'opération**. Vérifié sur tout le corpus : une transformation identité ne
  change pas un pixel, un déplacement ne touche à rien hors de sa zone, l'aplatissement d'un
  recadrage ne déborde pas, les octets d'avant et d'après la plage éditée sont identiques.
  Interface : sélection au clic, glisser pour déplacer, huit poignées, Maj pour les proportions,
  flèches au point près, `[` `]` pour l'ordre, Suppr pour retirer.
  À faire : sélection multiple à la souris, rotation à la poignée, répartition (« distribuer »),
  recadrage destructif qui réduit vraiment les pixels, objets à l'intérieur d'un XObject de formulaire
- [~] Liens, signets, articles, métadonnées, propriétés d'ouverture
  - **Métadonnées** (`acrux-features/docinfo.rs`, `acr metadata` / `set-metadata`) : lecture
    conjointe de `/Info` (§14.3.3) et du paquet XMP (§14.3.2), les **divergences entre les deux
    étant signalées** au lieu d'être tranchées en silence ; écriture des deux d'un seul tenant,
    le XMP existant étant modifié **propriété par propriété** (`docinfo/xmp.rs`, analyseur
    RDF/XML minimal écrit pour l'occasion) afin de conserver les droits d'usage, l'historique
    des révisions et les schémas métier qu'on ne connaît pas ; dates PDF `D:…` et ISO 8601
    converties dans les deux sens ; propriétés personnalisées de `/Info` préservées ;
    `remove_all_metadata` assainit avant publication (`/Info`, XMP du catalogue et des pages,
    `/PieceInfo`) en disant ce qui a été retiré
  - **Propriétés d'ouverture** (§12.2, `acr view-prefs`) : `/PageMode`, `/PageLayout`,
    `/OpenAction` (page de départ, zoom, cadrage) et `/ViewerPreferences` (titre en barre de
    fenêtre, barres masquées, fenêtre ajustée ou centrée, recto verso, plage d'impression,
    nombre de copies)
  - **Signets** (`acrux-features/outline_edit.rs`, `acr bookmarks` / `set-bookmarks` /
    `auto-bookmarks`) : écriture d'un arbre complet avec style (gras, italique, couleur), état
    ouvert ou fermé, chaînage `/First` `/Last` `/Prev` `/Next` `/Parent` et `/Count` **signé**
    reconstruits d'un bloc ; ajout, retrait (la descendance part avec le nœud) et déplacement
    par chemin ; `outline_from_headings` déduit l'arbre de la structure balisée quand elle
    existe — la mise en page lui rendant ses espaces — et de la taille des titres sinon
  - **Liens** (`acrux-features/linkedit.rs`, `acr link` / `autolink`) : pose et retrait
    d'annotations `/Link` invisibles vers une adresse, une page interne, un fichier externe ou
    une action nommée ; `autolink` détecte http, https, mailto et `www.` dans le texte extrait
    et pose les cadres sur les **rectangles réels des mots** (ponctuation finale et parenthèse
    non appariée exclues), une adresse coupée en fin de ligne n'étant recollée que derrière une
    barre oblique ; relancer la commande ne crée pas de doublons. Vérifié sur tout le corpus :
    après écriture le document se relit, `acr check` ne signale rien et le rendu est identique
    **au pixel près** (`crates/acrux-features/tests/docinfo_corpus.rs`)
  - Reste : **articles** (`/Threads`, §12.4.3), destinations nommées éditables, signets déduits
    d'un tableau de matières composé à la main, panneaux Signets et Liens éditables dans
    l'application
- [x] **Réécriture chirurgicale du flux de contenu** (rien de non touché ne change) :
  `edit_text::rewrite_content` relit le flux en jetons (`ContentLexer`) et recopie **octet pour
  octet** toutes les opérations qui ne sont pas remplacées — mêmes opérateurs, mêmes nombres, mêmes
  espacements, mêmes commentaires. Vérifié sur tout le corpus : une réécriture sans modification
  redonne exactement les octets décodés d'origine
Livrable : version 0.3, édition au moins équivalente à Acrobat Pro sur le corpus d'édition.

## Phase 6 — Formulaires et signatures
- [~] Remplissage AcroForm : inventaire (héritage, noms qualifiés), remplissage (texte, cases, radios, listes) avec régénération des apparences (§12.7.4.3 : fond / bordure `/MK`, `/DA`, taille auto, multiligne, peigne, mot de passe, coches vectorielles), aplatissement, import / export FDF (`acrux-features/forms.rs`, `acr fields/fill/fdf-export/fdf-import`, corpus `formulaire-acroform-champs.pdf`) ; remplissage interactif dans l'application (clic sur les widgets, `EditOp::SetField`) ; calculs, validation, actions JavaScript, XFA, texte enrichi et navigation par Tab à faire
- [ ] Préparation de formulaire avec détection automatique des champs
- [x] Remplir et signer (`acrux-features/fillsign/`, `acr fillsign`, touche `S` dans l'application) :
  signature **tracée** au pointeur — relevé nettoyé, lissé sans retard, largeur déduite de la vitesse
  ou de la pression, ré-échantillonnage à pas constant, effilage des bouts, éventail dans les virages
  serrés, calottes rondes, contour en cubiques de Bézier rempli en règle non nulle, plume
  proportionnée au dessin (`Pen::for_extent`) ; signature **tapée** dans une police manuscrite du
  système incorporée en sous-ensemble ; signature **importée** avec détourage du papier par
  estimation **locale** de l'éclairage (maximum glissant puis flou en caisson, tous deux linéaires),
  rognage à l'encre, recoloration facultative ; texte libre et cinq marques (coche, croix, rond,
  trait, point) dessinées par la même chaîne que l'encre. Chaque élément est une annotation
  `/Stamp` ou `/FreeText` avec son apparence, repérée par la clé privée `/AKFillSign` : inventaire,
  retrait et **aplatissement** qui ne déplace pas un pixel. Interface : barre des éléments, fenêtre
  de capture à trois onglets, signature et paraphe conservés entre deux sessions.
  À faire : déplacer un élément déjà posé, redimensionner au glisser, plusieurs signatures au choix
- [~] Signatures numériques (`acrux-features/signature/`, `acr signatures/verify/sign`,
  note `spec-notes/pdf-12.8-signatures.md`) : **toute la cryptographie est écrite ici**
  — entiers de grande taille avec réduction de Montgomery et échelle à temps constant
  (`crypt/bigint.rs`), RSA `RSASSA-PKCS1-v1_5` (signature et vérification) et
  `RSASSA-PSS` (vérification), EMSA, MGF1, génération de clés par Miller-Rabin
  (`crypt/rsa.rs`), SHA-1 en lecture seule (`crypt/sha1.rs`), lecteur DER **strict**
  qui refuse le BER à longueur indéfinie et les longueurs non minimales, plus un
  écrivain DER (`acrux-document/asn1.rs`), certificats X.509 lus **et émis**
  (`signature/x509.rs`), enveloppe CMS `SignedData` détachée lue et fabriquée
  (`signature/cms.rs`).
  **Vérification** : cinq verdicts indépendants — condensé des octets du `/ByteRange`
  contre le `messageDigest` signé, signature RSA contre la clé du certificat, chaîne
  jusqu'à une racine fournie (dates, `basicConstraints`, `keyUsage`, `extKeyUsage`,
  refus de toute extension critique inconnue), **couverture de tout le fichier**
  (l'intervalle libre doit être exactement la chaîne `/Contents`, la dernière plage
  doit finir sur le dernier octet) et **modifications postérieures** (le fichier
  tronqué à la fin de la zone signée est relu et comparé objet par objet).
  **Signature** : champ visible ou invisible avec apparence, `/Contents` réservé,
  `/ByteRange` écrit après coup à largeur fixe, sous-filtre `adbe.pkcs7.detached`,
  attributs signés `contentType`, `signingTime` et `messageDigest`.
  Corpus `synthese/signature-attestation-signee.pdf` avec son autorité de test.
  Contrôlé par un tiers : `System.Security.Cryptography.Pkcs.SignedCms` de .NET
  accepte notre enveloppe et `X509Chain` valide nos certificats sans erreur.
  À faire : horodatage RFC 3161, révocation OCSP / CRL, LTV (`/DSS`, PAdES B-LT et
  B-LTA), `/DocMDP` en écriture et évaluation des modifications autorisées, ECDSA,
  PKCS#12, signature d'un document chiffré, aléa du système pour les vraies clés
- [ ] Certification de document, sceaux
Livrable : version 0.4.

## Phase 7 — Sécurité, conformité, prépresse
- [~] Mots de passe et permissions (écriture) : `Document::protect/unprotect` (AES-256 R6, `Permissions`), `acr protect/unprotect`, `--password` ; chiffrement par certificat à faire
- [x] Biffure définitive (`acrux-features/redact.rs`, `acr redact/sanitize`) : marques `/Redact` (§12.5.6.23) avec `/QuadPoints`, `/IC`, `/OverlayText`, `/RO` et aperçu en cadre rouge ; recherche par motif écrite à la main (littéral, courriel, téléphone, IBAN modulo 97, carte bancaire Luhn, sécurité sociale, date, IP) ; application par **réécriture du flux de contenu** (codes des glyphes couverts retirés des `Tj`/`TJ` avec correction du positionnement, XObjects de formulaire réécrits récursivement, pixels couverts des images XObject et en ligne mis à zéro, tracés entièrement couverts supprimés, annotations touchées retirées) ; nettoyage des données cachées (métadonnées, pièces jointes, JavaScript, calques désactivés, commentaires, formulaires, texte invisible, objets inatteignables, révisions antérieures) ; corpus `synthese/biffure-avant-application.pdf`. À faire : polices à sous-ensemble réduites aux glyphes restants, biffure de motifs sur un corpus réel, aperçu interactif dans l'application
- [~] Accessibilité (`acrux-features/accessibility.rs`, `acr check-a11y/autotag`) : lecture de l'arbre de structure (`/StructTreeRoot` §14.7 : types après `/RoleMap`, texte retrouvé par les `/MCID` des marques `BDC … EMC`, `/Alt`, `/ActualText`, `/Lang`, `/T`, attributs `/A`, `/OBJR`, rattachement page par page via `/Pg` et `/ParentTree`) ; **vérificateur PDF/UA-1** avec 17 règles testées (document balisé, langue du document et des passages, titre et `/DisplayDocTitle`, figures sans `/Alt`, liens sans texte de substitution, ordre de lecture comparé à la découpe XY, titres sans saut de niveau, tableaux sans `TH` / `/Scope` / `/Headers`, listes `L > LI > LBody`, contraste WCAG 4,5:1 et 3:1 calculé contre les aplats du contenu, page image sans texte extractible, polices sans équivalent Unicode, champs sans `/TU`, contenu non balisé) ; **balisage automatique** : arbre construit depuis la mise en page de `text` (titres renumérotés, paragraphes, listes, tableaux, figures), marques `BDC /P <</MCID n>> … EMC` insérées dans le flux réécrit octet pour octet, `/Artifact` sur le décor et les en-têtes courants, `/ParentTree`, `/MarkInfo`, `/Lang` déduit du texte ; corpus `synthese/accessibilite-problemes.pdf`, note `spec-notes/pdf-14.7-structure-et-balisage.md`. À faire : `Lbl` séparé du `LBody`, alt-text suggéré par l'IA locale, panneau de balises dans l'application
- [~] Contrôle en amont (`acrux-features/preflight.rs`, `acr preflight/separations`) : profils PDF/A-1b, A-2b, A-3b, PDF/X-1a, PDF/X-4 et PDF/UA-1 ; règles vérifiées (version du fichier, chiffrement, polices incorporées et programme lisible, `/OutputIntent` présent et cohérent, transparence, JavaScript et `/Launch`, pièces jointes, flux externes `/F`, espaces dépendants du périphérique sans intention de sortie, `/Interpolate`, `/TrimBox`, surimpression, XMP cohérent avec `/Info`, accessibilité pour PDF/UA) ; **correctifs** : générateur XMP maison (RDF/XML, `pdfaid`/`pdfuaid`/`pdfxid`), intention de sortie sRVB avec un **profil ICC matriciel/TRC engendré par nous** et relu par `acrux-graphics`, incorporation des polices depuis les polices système, retrait du JavaScript et des pièces jointes, `/DisplayDocTitle`, `/Interpolate false`, `/TrimBox`, balisage automatique pour PDF/UA ; **aperçu de sortie** : `separations` (quatre plaques CMJN issues du rendu, plaques CMJN directes pour les couleurs posées en `k`, plaques nommées des tons directs) et `ink_coverage` (taux d'encre total maximal, alerte au-delà de 300 %) ; corpus `synthese/pdfa-1b-conforme.pdf`. À faire : aplatissement de la transparence, conversion colorimétrique, PDF/A niveau A, profils de destination autres que sRVB
- [x] Comparaison de documents (`acrux-features/compare.rs`, `acr compare`) : appariement des pages par similarité de contenu (alignement global Needleman-Wunsch : page insérée, supprimée ou déplacée reconnue), alignement des mots par plus longue sous-suite commune écrite à la main avec repli linéaire par ancres uniques sur les pages énormes, différences situées par page et par rectangle dans les deux documents (ajout, suppression, remplacement, déplacement d'un bloc d'une page à l'autre), comparaison des pixels des deux rendus avec tolérance d'anticrénelage et fusion en zones, rapport PDF côte à côte (pages source reprises en XObjects de formulaire, surlignages vert / rouge / bleu, page de résumé) ; corpus `synthese/comparaison-avant.pdf` et `-apres.pdf`. À faire : comparaison des annotations et des champs de formulaire, diff des images incorporées, vue synchronisée dans l'application
Livrable : version 0.5, parité Acrobat Pro.

## Phase 8 — Créer, convertir, reconnaître
- [~] **Création depuis images, texte et Markdown** (`acrux-features/create/`, `acr create`) :
  document vierge aux formats nommés (A0 à A6, lettre, légal, tabloïd, enveloppes DL / C4 / C5 /
  C6 / Monarch) ou libre (`210x297mm`, `8.5x11in`, points), orientation et marges ; **depuis des
  images** — une par page ou en grille, ajustement contenir / remplir / taille réelle avec
  résolution supposée, le JPEG incorporé **octet pour octet** sous `/DCTDecode` et le PNG
  recompressé en Flate avec son `/SMask` ; **depuis du texte** — metteur en page complet (coupure
  aux espaces, césure simple d'un mot plus long que la ligne, paragraphes, interligne, alignements
  gauche / droite / centré / justifié dont la dernière ligne d'un paragraphe reste libre,
  pagination, en-tête et pied avec jetons `{page}` / `{pages}`) ; **depuis du Markdown** — analyseur
  écrit ici (titres 1 à 6, gras, italique, code incorporé et en bloc, listes à puces et numérotées
  sur trois niveaux, citations, règles, tableaux à barres verticales avec alignement de colonne,
  liens posés en annotations `/Link`, titres convertis en signets). La police est choisie seule :
  une des quatorze polices standard tant que le texte tient en WinAnsiEncoding, une police système
  incorporée en sous-ensemble `/Type0` avec `/ToUnicode` dès qu'il en sort, ce qui accepte le grec,
  le cyrillique et le CJK sans perdre l'extraction. Corpus `synthese/cree-depuis-markdown.pdf` et
  `cree-depuis-images.pdf`.
  **À faire** : presse-papiers, scanner (TWAIN / WIA), capture d'écran, import d'un document
  bureautique, colonnes, images dans le Markdown, arbre de signets à plusieurs niveaux.
- [~] **Export vers images, texte, HTML, DOCX, XLSX** (`acrux-features/export.rs`, `acr export`) :
  pages en PNG (lignes filtrées, flux compressé par le DEFLATE maison : 60 Ko au lieu de 8 Mo
  pour une page A4 à 150 dpi) et en JPEG (encodeur JPEG de base maison, `acrux-codecs/dct/encode.rs` : DCT 8 × 8,
  tables de l'annexe K mises à l'échelle par la qualité, 4:2:0 optionnel) ; extraction des images
  incorporées dans leur format d'origine (les `DCTDecode` ressortent octet pour octet, le reste
  en PNG avec alpha si `/SMask`) ; HTML autonome, texte positionné au point près ou reflué
  (`--flow`), images en `data:` et polices incorporées converties en WOFF 1.0 ; DOCX
  (WordprocessingML : paragraphes, alignement, retraits, styles de caractère, titres, listes,
  tableaux à bordures, images) et XLSX (SpreadsheetML : une feuille par page, les tableaux
  détectés, chaînes partagées) via un écrivain ZIP maison (`acrux-features/zip.rs`) ; texte et
  Markdown via `PageText`. Vérifiés : ouverture dans Word et Excel (automation COM), rendu HTML
  comparé à `acr render` dans Chrome. **Reste** : PPTX, graphiques vectoriels dans le HTML,
  en-têtes et pieds Word natifs, colonnes et zones de texte.
- [ ] Imprimante virtuelle système
- [ ] OCR maison (segmentation, reconnaissance, langues, manuscrit), PDF recherchable
- [~] **Combinaison** (`acrux-features/create/combine.rs`, `acr combine`) : PDF, images et fichiers
  texte ou Markdown réunis en un document, nature devinée sur les octets de tête puis l'extension ;
  un signet par fichier portant son nom ; **sommaire** engendré en tête, une ligne par fichier avec
  ligne de points, numéro de page et lien cliquable, sa longueur étant recalculée jusqu'à se
  stabiliser puisqu'elle décale ce qu'elle annonce ; numérotation continue traversant les fichiers,
  posée par `stamp::add_header_footer`.
  **À faire** : portfolios (`/Collection`), entrelacement des pages, tri et rotation par fichier,
  fusion des formulaires et des signets d'origine.
Livrable : version 0.6.

## Phase 9 — Dépasser Acrobat
- [ ] Automatisation : API de script moderne, actions par lots, ligne de commande complète
- [ ] Système de plug-ins ouvert
- [ ] IA locale hors ligne : résumé, questions-réponses, alt-text, extraction de tableaux
- [ ] Historique de versions type Git et comparaison sémantique
- [ ] Collaboration hors ligne (fusion de commentaires sans serveur)
- [ ] Support Linux et macOS au même niveau que Windows
Livrable : version 1.0.

## Suivi
Tenir ce fichier à jour : cocher les cases, dater les jalons, lier les issues.
