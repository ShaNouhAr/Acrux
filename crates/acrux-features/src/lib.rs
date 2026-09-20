//! # acrux-features
//!
//! Fonctionnalités métier construites au-dessus du moteur : tout ce que
//! l'utilisateur voit comme un « outil » dans l'application.
//!
//! | Module   | Rôle                                                   | Inventaire Acrobat |
//! |----------|--------------------------------------------------------|--------------------|
//! | `accessibility` | arbre de structure (§14.7), vérificateur PDF/UA, balisage automatique | §10 |
//! | `annotations` | inventaire, création (carré, surlignage, note, lien) avec apparences, suppression | §8 |
//! | `attach` | pièces jointes : arbre `/Names /EmbeddedFiles`, annotations `/FileAttachment`, ajout, extraction, retrait | §11 |
//! | `docinfo` | propriétés du document : `/Info`, paquet XMP, propriétés d'ouverture, assainissement | §12.2, §14.3 |
//! | `create` | création de documents : page blanche, images, texte, Markdown, combinaison | §1 |
//! | `compare` | comparaison de deux documents : appariement des pages, différences de texte, différences de pixels, rapport PDF côte à côte | §8 |
//! | `export` | conversion : pages en PNG / JPEG, images incorporées, HTML, DOCX, XLSX, texte, Markdown | §6 |
//! | `fillsign` | remplir et signer : signature tracée, tapée ou importée, texte et marques, aplatissement | §9 |
//! | `edit_objects` | édition des objets : inventaire, déplacer, redimensionner, pivoter, recadrer, réordonner, aligner, remplacer une image | §3 |
//! | `edit_text` | édition de texte in place : réécriture chirurgicale du flux, remplacement, recomposition de paragraphe, sous-ensembles de polices | §3 |
//! | `forms`  | formulaires AcroForm : inventaire, remplissage avec apparences, aplatissement, FDF | §9 |
//! | `linkedit` | écriture des liens : pose, retrait, détection automatique des adresses | §1 |
//! | `media`  | multimédia : vidéos et sons du document, extraction du fichier incorporé | §13 |
//! | `navigation` | destinations, liens, actions, signets                   | §1            |
//! | `ocr`    | lecture du texte d'une image : seuillage, découpe en lignes et en lettres, reconnaissance par gabarits | §2 |
//! | `outline_edit` | écriture des signets : arbre complet, ajout, retrait, déplacement, signets automatiques | §1 |
//! | `pages`  | pivoter, supprimer, réordonner, extraire, insérer, fusionner | §5            |
//! | `pagelabels` | étiquettes de page `/PageLabels` (i, ii, 1, 2, Annexe-A…), lecture, écriture, recherche par étiquette | §1 |
//! | `preflight` | contrôle en amont PDF/A, PDF/X, PDF/UA, correctifs, aperçu de sortie | §10 |
//! | `signature` | signatures numériques (§12.8) : inventaire, vérification (condensé, RSA, chaîne X.509, couverture, modifications), pose d'une signature CMS détachée | §9 |
//! | `redact` | biffure définitive (réécriture du flux, images, tracés), recherche par motif, nettoyage des données cachées | §7 |
//! | `stamp`  | filigranes, arrière-plans, en-têtes et pieds de page, numérotation Bates | §4 |
//! | `three_d` | modèles 3D (§13.6) : annotations `/3D`, lecture du format U3D (ECMA-363), rendu et manipulation | §13 |
//! | `sysfonts` | catalogue des polices installées : familles et leurs quatre dessins | §3 |
//! | `text`   | extraction de texte structurée (paragraphes, colonnes, tableaux, styles), recherche, export texte / Markdown / HTML | §2, §6 |
//! | `zip`    | écrivain et lecteur ZIP minimal (conteneur des `.docx` et `.xlsx`) | —      |
//!
//! Modules prévus : `security`, `optimize`, `scripting`, `ai`.

pub mod accessibility;
pub mod annotations;
pub mod attach;
pub mod compare;
pub mod create;
pub mod docinfo;
pub mod edit_objects;
pub mod edit_text;
pub mod export;
pub mod fillsign;
pub mod fontembed;
pub mod forms;
pub mod linkedit;
pub mod media;
pub mod navigation;
pub mod ocr;
pub mod outline_edit;
pub mod pagelabels;
pub mod pages;
pub mod preflight;
pub mod redact;
pub mod signature;
pub mod stamp;
pub mod sysfonts;
pub mod text;
pub mod three_d;
pub mod zip;
