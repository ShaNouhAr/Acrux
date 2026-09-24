//! `acr` : outil en ligne de commande.
//!
//! Sert aux contributeurs (inspection d'un fichier, tests du corpus,
//! benchmarks) et aux utilisateurs avancés (conversion par lots).
//!
//! Commandes : inspection (`info`, `pages`, `dump`, `check`), modification
//! (`rotate`, `delete`, `reorder`, `extract`, `merge`, `rewrite`), création
//! (`create`, `combine`), rendu,
//! texte et conversion (`render`, `export`, `text`, `bench`), annotations (`annots`, `annotate`),
//! pièces jointes (`attachments`, `attach`, `detach`), étiquettes de page
//! (`labels`, `set-labels`), propriétés du document (`metadata`,
//! `set-metadata`, `view-prefs`), signets et liens (`bookmarks`,
//! `set-bookmarks`, `auto-bookmarks`, `autolink`, `link`),
//! apports visuels (`watermark`, `background`, `header-footer`, `bates`, `unstamp`),
//! formulaires (`fields`, `fill`, `fdf-export`, `fdf-import`), sécurité
//! (`protect`, `unprotect`, `redact`, `sanitize`), accessibilité et conformité
//! (`check-a11y`, `autotag`, `preflight`, `separations`), comparaison de deux
//! documents (`compare`). L'option globale
//! `--password <mdp>` ouvre un document chiffré.

use std::io::Write;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

use acrux_document::protect::{
    password_strength, password_warnings, Permissions, PrintLevel, Strength,
};
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, writer, Document, Name, Object, ObjectRef, XrefKind};
use acrux_features::navigation::{flatten_outline, outline, page_links, Action, PageIndex, View};

/// Mot de passe passé par `--password`, lu par `open`.
static PASSWORD: OnceLock<Vec<u8>> = OnceLock::new();

// Une ligne par commande : la découper rendrait l'aide plus difficile à relire
// que la fonction elle-même.
#[allow(clippy::too_many_lines)]
fn usage() {
    eprintln!("acr {}", env!("CARGO_PKG_VERSION"));
    eprintln!("usage : acr <commande> <fichier.pdf> [options]");
    eprintln!();
    eprintln!("  info  <fichier>                 version, pages, métadonnées, structure");
    eprintln!("  pages <fichier>                 liste des pages avec boîtes et rotation");
    eprintln!("  dump  <fichier> [n|trailer|catalog] [--data]");
    eprintln!("                                  affiche un objet en syntaxe PDF (--data : contenu décodé du flux)");
    eprintln!("  check <fichier>                 lit tous les objets et signale les problèmes");
    eprintln!();
    eprintln!(
        "  rotate  <fichier> <pages> <angle> -o <sortie>   pivote les pages (angle multiple de 90)"
    );
    eprintln!("  delete  <fichier> <pages> -o <sortie>           supprime des pages");
    eprintln!("  reorder <fichier> <pages> -o <sortie>           réordonne (les pages absentes sont supprimées)");
    eprintln!("  extract <fichier> <pages> -o <sortie>           nouveau document avec ces pages");
    eprintln!("  merge   <fichier>... -o <sortie>                fusionne plusieurs documents");
    eprintln!();
    eprintln!("  create --blank [--size A4|lettre|210x297mm] [--orientation portrait|paysage] [--pages N]");
    eprintln!("         [--margin N] -o <sortie>          document vierge (A0 à A6, lettre, légal, tabloïd,");
    eprintln!("                                  enveloppes DL/C4/C5/C6/Monarch, ou taille libre)");
    eprintln!("  create --images a.jpg b.png [--fit contain|cover|actual] [--dpi N] [--grid CxL]");
    eprintln!(
        "         [--size A4] [--orientation ...] [--margin N] [--background r,g,b] -o <sortie>"
    );
    eprintln!("                                  une image par page, ou une grille ; le JPEG est incorporé tel");
    eprintln!("                                  quel (aucune recompression), le PNG recompressé en Flate avec");
    eprintln!("                                  son /SMask si transparence");
    eprintln!("  create --text <fichier.txt> [--font <nom>] [--size N] [--margin N] [--align gauche|droite|");
    eprintln!("         centre|justifie] [--page-size A4] [--header <texte>] [--footer <texte>] -o <sortie>");
    eprintln!("                                  met le texte en page : coupure aux espaces, paragraphes,");
    eprintln!("                                  pagination, jetons {{page}} et {{pages}} dans l'en-tête et le pied ;");
    eprintln!("                                  une police système est incorporée dès que le texte sort de");
    eprintln!("                                  WinAnsiEncoding (grec, cyrillique, CJK…)");
    eprintln!(
        "  create --3d <modele.u3d> [--size A4] [--orientation ...] [--margin N] -o <sortie>"
    );
    eprintln!("                                  pose un modèle 3D sur une page, avec sa vue par défaut et");
    eprintln!("                                  l'affiche rendue par notre moteur 3D");
    eprintln!("  create --markdown <fichier.md> [mêmes options] -o <sortie>");
    eprintln!("                                  titres, gras, italique, code, listes, citations, règles,");
    eprintln!("                                  tableaux et liens cliquables ; les titres deviennent des signets");
    eprintln!("  combine <fichier>... [--bookmarks] [--toc] [--numbers] [--toc-title <titre>] -o <sortie>");
    eprintln!("                                  réunit PDF, images et fichiers texte ou Markdown ; --bookmarks :");
    eprintln!("                                  un signet par fichier, --toc : sommaire cliquable en tête,");
    eprintln!(
        "                                  --numbers : numérotation continue en pied de page"
    );
    eprintln!("  rewrite <fichier> [--compress] [--compact] -o <sortie>");
    eprintln!("                                  réécriture complète et propre ; --compress : flux Flate, --objstm : flux d'objets et xref");
    eprintln!("                                  compressé, --gc : objets inatteignables retirés, --compact : les trois");
    eprintln!("      <pages> : « 1,3-5,8 » (1 = première page) ; --full : réécriture au lieu d'un ajout incrémental");
    eprintln!();
    eprintln!("  3d      <fichier> [--extract <dossier>]");
    eprintln!("                                  les modèles 3D du document : format, rectangle, géométrie lue ;");
    eprintln!("                                  --extract écrit les fichiers U3D tels quels");
    eprintln!("  render  <fichier> [pages] [--dpi N] [--rotate deg] [--no-annots] -o <sortie.png>");
    eprintln!("                                  rend les pages en PNG (96 dpi par défaut ; plusieurs pages → sortie-N.png)");
    eprintln!("  export  <fichier> --format png|jpeg|images|html|docx|xlsx|md|txt [--dpi N] [--quality N]");
    eprintln!("          [--pages 1,3-5] [--flow] -o <sortie>");
    eprintln!("                                  convertit le document (150 dpi et qualité 85 par défaut ; le format");
    eprintln!("                                  peut être déduit de l'extension de <sortie> ; plusieurs pages →");
    eprintln!("                                  sortie-N.ext) ; images : extrait les images incorporées (JPEG tels");
    eprintln!("                                  quels) ; html : page fidèle, --flow pour du texte reflué ;");
    eprintln!(
        "                                  docx et xlsx portent toujours sur le document entier"
    );
    eprintln!("  text    <fichier> [pages] [--markdown|--html|--layout] [-o <sortie>]");
    eprintln!("                                  extrait le texte : paragraphes, colonnes, tableaux, en-têtes et pieds dans");
    eprintln!("                                  l'ordre de lecture ; --markdown / --html : titres, gras, listes, tableaux ;");
    eprintln!("                                  --layout : texte positionné (colonnes alignées par des espaces)");
    eprintln!("  find    <fichier> <texte> [--case] [--word] [--page N]");
    eprintln!("                                  cherche comme la carte de recherche : une ligne par occurrence");
    eprintln!("                                  (page, ligne, contexte), puis le total ; --case respecte la casse,");
    eprintln!("                                  --word ne prend que le mot entier ; une occurrence peut passer à la");
    eprintln!("                                  ligne dans un paragraphe (« documen- / tation »)");
    eprintln!("  bench   <fichier> [--dpi N]                    mesure ouverture, rendu et extraction de texte");
    eprintln!("  annots  <fichier> [pages] [-v]                  liste les annotations ; -v : identifiants (/NM) ;");
    eprintln!("                                  sous chaque commentaire : date, statut, case cochée et réponses (↳)");
    eprintln!(
        "  annot-set <fichier> <page> <n°> [--rect x0,y0,x1,y1] [--move dx,dy] [--color RRGGBB]"
    );
    eprintln!("          [--fill RRGGBB|none] [--opacity 0..1] [--width N] [--text \"…\"]");
    eprintln!("          [--state accepted|rejected|cancelled|completed|none] [--marked oui|non] [--author NOM] -o <sortie>");
    eprintln!("                                  modifie une annotation (n° comme dans annots) : place, couleur,");
    eprintln!("                                  fond, opacité, trait, texte ; l'apparence est redessinée ;");
    eprintln!(
        "                                  --state et --marked posent un statut de relecture"
    );
    eprintln!("  annot-remove <fichier> <page> <n°> -o <sortie> retire une annotation, avec ses réponses et sa fenêtre");
    eprintln!("  reply   <fichier> <page> <n°> <texte> [--author NOM] -o <sortie>");
    eprintln!("                                  répond à un commentaire (réponse /IRT, affichée dans son fil)");
    eprintln!("  links   <fichier> [pages]                       liste les liens et leurs cibles");
    eprintln!("  outline <fichier>                               affiche les signets (arbre)");
    eprintln!("  annotate <fichier> <page> <type> … -o <sortie>");
    eprintln!("          square|circle|highlight|underline|strikeout|squiggly|link <x0> <y0> <x1> <y1> [texte]");
    eprintln!("                                  underline, strikeout, squiggly : balise la zone (texte = commentaire) ;");
    eprintln!("          caret|replace <x0> <y0> <x1> <y1> <texte>");
    eprintln!(
        "                                  caret : signe d'insertion en x0, sur la ligne y0..y1 ;"
    );
    eprintln!("                                  replace : barre la zone et propose le texte, signe au bout");
    eprintln!("          line|arrow <x1> <y1> <x2> <y2> [--head open|closed|circle|square|butt|none] [--tail …]");
    eprintln!("          polygon|polyline <x> <y> <x> <y> <x> <y> …");
    eprintln!("          ink --points \"x,y x,y …;x,y …\"   dessin à main levée, un « ; » entre deux traits");
    eprintln!("                                  formes : --color r,g,b --fill r,g,b --width <pt> --opacity 0..1");
    eprintln!("          text <x0> <y0> <x1> <y1> <texte> [--font helvetica|times|courier[-bold|-italic]]");
    eprintln!("               [--size 12] [--align left|center|right] [--color r,g,b] [--border r,g,b] [--fill r,g,b]");
    eprintln!(
        "                                  zone de texte ; « \\n » dans le texte passe à la ligne"
    );
    eprintln!(
        "          callout <x0> <y0> <x1> <y1> <ax> <ay> <texte>   légende fléchée vers (ax, ay)"
    );
    eprintln!();
    eprintln!("  attachments <fichier>                           liste les pièces jointes (nom, taille, type, page)");
    eprintln!(
        "  attach  <fichier> <fichier-à-joindre> [--description <texte>] [--page N] -o <sortie>"
    );
    eprintln!("                                  incorpore un fichier (flux compressé, /Params avec somme MD5) ;");
    eprintln!("                                  --page pose en plus une icône de trombone sur cette page");
    eprintln!("  detach  <fichier> <nom> -o <fichier-extrait>    extrait une pièce jointe (somme de contrôle vérifiée)");
    eprintln!();
    eprintln!("  metadata     <fichier>                          propriétés du document : /Info, XMP, vue initiale");
    eprintln!("  set-metadata <fichier> [--title T] [--author A] [--subject S] [--keywords K]");
    eprintln!("               [--creator C] [--producer P] [--clear] -o <sortie>");
    eprintln!("                                  écrit /Info et le paquet XMP d'un seul tenant, d'accord entre eux ;");
    eprintln!("                                  une option absente ne touche pas le champ, une valeur vide l'efface ;");
    eprintln!(
        "                                  --clear assainit : toutes les métadonnées retirées"
    );
    eprintln!(
        "  view-prefs   <fichier> [--page-mode <mode>] [--layout <disposition>] [--open-page N]"
    );
    eprintln!("               [--open-zoom <zoom>] [-o <sortie>]");
    eprintln!("                                  sans option : affiche les propriétés d'ouverture (§12.2) ;");
    eprintln!("      <mode> : aucun, signets, vignettes, plein-ecran, calques, pieces-jointes");
    eprintln!(
        "      <disposition> : une-page, continu, deux-colonnes[-droite], deux-pages[-droite]"
    );
    eprintln!("      <zoom> : un pourcentage (150), ou fit, largeur, hauteur, contenu");
    eprintln!();
    eprintln!(
        "  bookmarks     <fichier>                         arbre des signets, avec style et cible"
    );
    eprintln!("  set-bookmarks <fichier> <signets.txt> -o <sortie>");
    eprintln!("                                  remplace l'arbre par celui du fichier texte : une ligne par signet,");
    eprintln!("                                  « titre<TAB>page », la profondeur donnée par les tabulations de");
    eprintln!("                                  début de ligne ; page omise = signet sans cible ; un fichier vide");
    eprintln!("                                  retire tous les signets");
    eprintln!("  auto-bookmarks <fichier> -o <sortie>            signets déduits des titres (structure balisée si");
    eprintln!("                                  elle existe, sinon la mise en page)");
    eprintln!();
    eprintln!("  autolink <fichier> -o <sortie>                  pose un lien sur chaque adresse du texte (http,");
    eprintln!("                                  https, mailto, www.) ; rien n'est dessiné, le rendu est inchangé");
    eprintln!("  link     <fichier> <page> <x0,y0,x1,y1> <cible> -o <sortie>");
    eprintln!("      <cible> : https://…, mailto:…, page:12, fichier.pdf#4, nommee:NextPage");
    eprintln!();
    eprintln!(
        "  labels     <fichier>                            étiquette de chaque page (/PageLabels)"
    );
    eprintln!("  set-labels <fichier> <plages> -o <sortie>       redéfinit les étiquettes");
    eprintln!("      <plages> : « 1:i,5:D:1 » — une plage par virgule, « page:style[:préfixe][:début] » ;");
    eprintln!("                 styles : D (1,2,3), r/i (i,ii), R/I (I,II), a (a,b… aa,bb), A ; vide = préfixe seul ;");
    eprintln!("                 le 3ᵉ champ est le début s'il est un nombre, sinon le préfixe (« 1:r:Annexe- »)");
    eprintln!();
    eprintln!("  watermark  <fichier> --text <texte> | --image <fichier.png|jpg> [--font <nom>] [--size N]");
    eprintln!("             [--color r,g,b] [--opacity N] [--rotation N] [--anchor <ancrage>] [--offset x,y]");
    eprintln!("             [--scale N | --fit N | --stretch] [--margin N] [--behind] [--pages 1,3-5] -o <sortie>");
    eprintln!("                                  filigrane : un XObject de formulaire partagé par les pages,");
    eprintln!("                                  devant le contenu (ou derrière avec --behind) ; le contenu");
    eprintln!("                                  d'origine n'est jamais réécrit ; --fit 0.8 = 80 % de la page");
    eprintln!("  background <fichier> [--color r,g,b] [--image <fichier>] [--text <texte>] [mêmes options] -o <sortie>");
    eprintln!("                                  arrière-plan, toujours derrière le contenu ; sans --image ni");
    eprintln!("                                  --text, --color remplit la page entière");
    eprintln!("  header-footer <fichier> [--header-left|-center|-right <texte>]");
    eprintln!("             [--footer-left|-center|-right <texte>] [--font <nom>] [--size N] [--color r,g,b]");
    eprintln!(
        "             [--margin-top|-bottom|-left|-right N] [--start N] [--format 1|i|I|a|A]"
    );
    eprintln!("             [--pages 1,3-5] [--utc-offset +HH:MM] -o <sortie>");
    eprintln!("                                  jetons remplacés page par page : {{page}}, {{pages}}, {{date}},");
    eprintln!("                                  {{time}}, {{datetime}}, {{filename}} ({{{{ pour une accolade) ;");
    eprintln!("                                  les dates sont en UTC sauf si --utc-offset donne le fuseau");
    eprintln!("  bates      <fichier>... [--prefix P] [--suffix S] [--digits N] [--start N] [--anchor <ancrage>]");
    eprintln!(
        "             [--margin-x N] [--margin-y N] [--font <nom>] [--size N] [--color r,g,b]"
    );
    eprintln!("             [--pages 1,3-5] -o <sortie>");
    eprintln!("                                  numérotation Bates ; plusieurs fichiers → la numérotation");
    eprintln!("                                  continue de l'un à l'autre, sorties sortie-1.pdf, sortie-2.pdf…");
    eprintln!(
        "  unstamp    <fichier> [--kind filigrane,arriere-plan,entete-pied,bates] -o <sortie>"
    );
    eprintln!(
        "                                  retire les tampons posés par les commandes ci-dessus"
    );
    eprintln!();
    eprintln!(
        "  compare <avant.pdf> <apres.pdf> [--pages] [--visual] [--tolerance N] [-o rapport.pdf]"
    );
    eprintln!("                                  compare deux documents : pages appariées par leur contenu (une page");
    eprintln!("                                  insérée, supprimée ou déplacée est reconnue), puis texte aligné mot à");
    eprintln!("                                  mot (ajouts, suppressions, remplacements, déplacements) ; --visual :");
    eprintln!("                                  compare aussi les pixels (--tolerance 0 à 255 pour l'anticrénelage) ;");
    eprintln!("                                  --pages : seulement le tableau d'appariement des pages ; -o : rapport");
    eprintln!("                                  PDF côte à côte (ajouts en vert, suppressions en rouge, déplacements");
    eprintln!("                                  en bleu), sinon un résumé sur la sortie standard");
    eprintln!();
    eprintln!(
        "  edit-text <fichier> --find <texte> --replace <texte> [--page N] [--all] [--size N]"
    );
    eprintln!("            [--color r,g,b] [--font <nom>] [--bold] [--italic] [--case] [--word] -o <sortie>");
    eprintln!("                                  remplace du texte dans la page sans rien déplacer d'autre :");
    eprintln!("                                  le flux de contenu est réécrit octet pour octet sauf le mot visé,");
    eprintln!(
        "                                  réencodé avec la police en place (complétée au besoin)"
    );
    eprintln!("  reflow  <fichier> --page N --paragraph K --text <texte> [--shrink] [--min-size N] -o <sortie>");
    eprintln!("                                  recompose un paragraphe entier dans sa boîte (retour à la ligne,");
    eprintln!("                                  interligne et alignement d'origine) ; --shrink : réduit la taille pour tenir");
    eprintln!();
    eprintln!("  fields  <fichier>                               liste les champs de formulaire (AcroForm)");
    eprintln!("  fill    <fichier> [<nom>=<valeur>...] [--reset] [--flatten] -o <sortie>");
    eprintln!("                                  remplit des champs (case : oui/non ; liste : a|b) ; --flatten : aplatit ensuite");
    eprintln!(
        "  fdf-export <fichier> -o <sortie.fdf>            exporte les valeurs des champs en FDF"
    );
    eprintln!("  fdf-import <fichier> <données.fdf> -o <sortie>  importe des valeurs FDF dans le formulaire");
    eprintln!();
    eprintln!("  check-a11y <fichier> [--json]                  vérifie l'accessibilité (PDF/UA-1) et liste les problèmes");
    eprintln!("  autotag   <fichier> -o <sortie>                 balise automatiquement le document (structure, /Lang, /MarkInfo)");
    eprintln!("  preflight <fichier> --profile pdfa-1b|pdfa-2b|pdfa-3b|pdfx-1a|pdfx-4|pdfua-1");
    eprintln!("            [--json] [--fix [--title <titre>] -o <sortie>]");
    eprintln!("                                  contrôle la conformité ; --fix répare ce qui l'est sans perte");
    eprintln!("  separations <fichier> [--page N] [--dpi N] -o <prefix>.png");
    eprintln!("                                  aperçu de sortie : une image par plaque (CMJN et tons directs) et le taux d'encre");
    eprintln!();
    eprintln!("  protect   <fichier> [--user <mdp>] [--owner <mdp>] [--print none|low|high]");
    eprintln!(
        "            [--no-modify] [--no-copy] [--no-annotate] [--no-fill] [--no-accessibility]"
    );
    eprintln!("            [--no-assemble] -o <sortie>");
    eprintln!("                                  chiffre en AES-256 : --user est demandé à l'ouverture (absent :");
    eprintln!("                                  ouverture libre), --owner donne tous les droits et seul il permet");
    eprintln!("                                  de changer la protection ; toute restriction exige --owner");
    eprintln!("  unprotect <fichier> -o <sortie>           retire le chiffrement (mot de passe des permissions via --password)");
    eprintln!();
    eprintln!("  redact <fichier> --page N --rect x0,y0,x1,y1 [--text <remplacement>] [--color r,g,b] -o <sortie>");
    eprintln!("  redact <fichier> --find <texte> [--page N] [--all-pages] [--text <remplacement>] -o <sortie>");
    eprintln!("  redact <fichier> --pattern email,iban,carte[,telephone,securite-sociale,date,ip]");
    eprintln!("         [--page N] [--all-pages] -o <sortie>");
    eprintln!("                                  biffure définitive : les glyphes couverts sortent du flux, les pixels");
    eprintln!("                                  couverts des images sont noircis, les annotations touchées supprimées ;");
    eprintln!(
        "                                  --mark-only pose les marques /Redact sans les appliquer"
    );
    eprintln!(
        "  sanitize <fichier> [--metadata] [--attachments] [--javascript] [--layers] [--comments]"
    );
    eprintln!("           [--forms] [--invisible-text] [--gc] [--all] -o <sortie>");
    eprintln!("                                  retire les données cachées et réécrit le fichier en entier");
    eprintln!();
    eprintln!();
    eprintln!(
        "  objects <fichier> [--page N]    inventaire des objets dessines (images, traces, texte)"
    );
    eprintln!("  edit-object <fichier> --page N --object n -o <sortie>");
    eprintln!("                                  --move dx,dy  --scale sx[,sy]  --rotate deg");
    eprintln!(
        "                                  --place x0,y0,x1,y1  --crop x0,y0,x1,y1  --delete"
    );
    eprintln!(
        "                                  --order devant|derriere|avancer|reculer  --image f.png"
    );
    eprintln!("                                  --object 1,3,5 --align gauche|centre-x|droite|haut|centre-y|bas");
    eprintln!("  fillsign <fichier> place --page N --rect x0,y0,x1,y1 <source> -o <sortie>");
    eprintln!(
        "                                  remplir et signer ; source = --draw <traits.txt>,"
    );
    eprintln!(
        "                                  --typed <nom>, --image <sig.png>, --text <texte>,"
    );
    eprintln!("                                  --mark check|cross|dot|circle|line ;");
    eprintln!(
        "                                  options --color r,g,b --pen <largeur> --author <nom>"
    );
    eprintln!(
        "                                  --stretch --no-cutout --keep-color --softness <0-1>"
    );
    eprintln!("  fillsign <fichier> list         inventaire des elements poses");
    eprintln!("  fillsign <fichier> remove <page> <index> -o <sortie>");
    eprintln!("  fillsign <fichier> flatten -o <sortie>   les fond dans le contenu des pages");
    eprintln!("  signatures <fichier>            champs de signature : signataire, date, motif, /ByteRange");
    eprintln!("  verify     <fichier> [--roots <dossier de certificats DER>] [--json]");
    eprintln!("                                  cinq verdicts par signature : condensé, signature RSA, chaîne,");
    eprintln!(
        "                                  couverture du fichier, modifications postérieures"
    );
    eprintln!("  sign       <fichier> --key <cle.der> --cert <cert.der> [--chain <chaine.der>]");
    eprintln!(
        "             [--name <nom>] [--reason <motif>] [--location <lieu>] [--contact <contact>]"
    );
    eprintln!("             [--field <nom de champ>] [--page N --rect x0,y0,x1,y1] -o <sortie>");
    eprintln!("                                  signature CMS détachée (adbe.pkcs7.detached, SHA-256) ajoutée");
    eprintln!("                                  par mise à jour incrémentale ; --page/--rect rend la signature");
    eprintln!(
        "                                  visible ; --field remplit un emplacement de signature"
    );
    eprintln!("                                  déjà préparé et vide ; clé PKCS#8 non chiffrée uniquement");
    eprintln!(
        "  --password <mdp>                mot de passe d'ouverture, pour toutes les commandes"
    );
    eprintln!("  --version                       version de l'outil");
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--password") {
        if i + 1 < args.len() {
            let pw = args.remove(i + 1);
            args.remove(i);
            let _ = PASSWORD.set(pw.into_bytes());
        }
    }
    let cmd = args.first().map(String::as_str);
    let file = args.get(1);
    let result = match (cmd, file) {
        (Some("--version" | "-V"), _) => {
            println!("acr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        (Some("info"), Some(f)) => cmd_info(f),
        (Some("pages"), Some(f)) => cmd_pages(f),
        (Some("dump"), Some(f)) => cmd_dump(f, &args[2..]),
        (Some("check"), Some(f)) => cmd_check(f),
        (Some("rotate"), Some(f)) => cmd_rotate(f, &args[2..]),
        (Some("delete"), Some(f)) => cmd_delete(f, &args[2..]),
        (Some("reorder"), Some(f)) => cmd_reorder(f, &args[2..]),
        (Some("extract"), Some(f)) => cmd_extract(f, &args[2..]),
        (Some("merge"), Some(_)) => cmd_merge(&args[1..]),
        (Some("create"), Some(_)) => cmd_create(&args[1..]),
        (Some("combine"), Some(_)) => cmd_combine(&args[1..]),
        (Some("rewrite"), Some(f)) => cmd_rewrite(f, &args[2..]),
        (Some("render"), Some(f)) => cmd_render(f, &args[2..]),
        (Some("export"), Some(f)) => cmd_export(f, &args[2..]),
        (Some("text"), Some(f)) => cmd_text(f, &args[2..]),
        (Some("find"), Some(f)) => cmd_find(f, &args[2..]),
        (Some("bench"), Some(f)) => cmd_bench(f, &args[2..]),
        (Some("annots"), Some(f)) => cmd_annots(f, &args[2..]),
        (Some("links"), Some(f)) => cmd_links(f, &args[2..]),
        (Some("outline"), Some(f)) => cmd_outline(f),
        (Some("annotate"), Some(f)) => cmd_annotate(f, &args[2..]),
        (Some("annot-set"), Some(f)) => cmd_annot_set(f, &args[2..]),
        (Some("annot-remove"), Some(f)) => cmd_annot_remove(f, &args[2..]),
        (Some("reply"), Some(f)) => cmd_reply(f, &args[2..]),
        (Some("attachments"), Some(f)) => cmd_attachments(f),
        (Some("attach"), Some(f)) => cmd_attach(f, &args[2..]),
        (Some("detach"), Some(f)) => cmd_detach(f, &args[2..]),
        (Some("metadata"), Some(f)) => cmd_metadata(f),
        (Some("set-metadata"), Some(f)) => cmd_set_metadata(f, &args[2..]),
        (Some("view-prefs"), Some(f)) => cmd_view_prefs(f, &args[2..]),
        (Some("bookmarks"), Some(f)) => cmd_bookmarks(f),
        (Some("set-bookmarks"), Some(f)) => cmd_set_bookmarks(f, &args[2..]),
        (Some("auto-bookmarks"), Some(f)) => cmd_auto_bookmarks(f, &args[2..]),
        (Some("autolink"), Some(f)) => cmd_autolink(f, &args[2..]),
        (Some("link"), Some(f)) => cmd_link(f, &args[2..]),
        (Some("labels"), Some(f)) => cmd_labels(f),
        (Some("set-labels"), Some(f)) => cmd_set_labels(f, &args[2..]),
        (Some("watermark"), Some(f)) => cmd_watermark(f, &args[2..]),
        (Some("background"), Some(f)) => cmd_background(f, &args[2..]),
        (Some("header-footer"), Some(f)) => cmd_header_footer(f, &args[2..]),
        (Some("bates"), Some(_)) => cmd_bates(&args[1..]),
        (Some("unstamp"), Some(f)) => cmd_unstamp(f, &args[2..]),
        (Some("edit-text"), Some(f)) => cmd_edit_text(f, &args[2..]),
        (Some("reflow"), Some(f)) => cmd_reflow(f, &args[2..]),
        (Some("protect"), Some(f)) => cmd_protect(f, &args[2..]),
        (Some("unprotect"), Some(f)) => cmd_unprotect(f, &args[2..]),
        (Some("redact"), Some(f)) => cmd_redact(f, &args[2..]),
        (Some("sanitize"), Some(f)) => cmd_sanitize(f, &args[2..]),
        (Some("fields"), Some(f)) => cmd_fields(f),
        (Some("fill"), Some(f)) => cmd_fill(f, &args[2..]),
        (Some("fdf-export"), Some(f)) => cmd_fdf_export(f, &args[2..]),
        (Some("fdf-import"), Some(f)) => cmd_fdf_import(f, &args[2..]),
        (Some("check-a11y"), Some(f)) => cmd_check_a11y(f, &args[2..]),
        (Some("autotag"), Some(f)) => cmd_autotag(f, &args[2..]),
        (Some("preflight"), Some(f)) => cmd_preflight(f, &args[2..]),
        (Some("separations"), Some(f)) => cmd_separations(f, &args[2..]),
        (Some("compare"), Some(f)) => cmd_compare(f, &args[2..]),
        (Some("media"), Some(f)) => cmd_media(f, &args[2..]),
        (Some("3d"), Some(f)) => cmd_3d(f, &args[2..]),
        (Some("objects"), Some(f)) => cmd_objects(f, &args[2..]),
        (Some("edit-object"), Some(f)) => cmd_edit_object(f, &args[2..]),
        (Some("fillsign"), Some(f)) => cmd_fillsign(f, &args[2..]),
        (Some("signatures"), Some(f)) => cmd_signatures(f),
        (Some("verify"), Some(f)) => cmd_verify(f, &args[2..]),
        (Some("sign"), Some(f)) => cmd_sign(f, &args[2..]),
        _ => {
            usage();
            return ExitCode::from(1);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erreur : {e}");
            ExitCode::from(1)
        }
    }
}

fn open(path: &str) -> acrux_core::Result<(Document, std::time::Duration)> {
    let t = Instant::now();
    let doc = Document::load(path)?;
    // Un document à ouverture libre s'ouvre tout seul, en simple
    // utilisateur : le mot de passe fourni est alors celui des permissions,
    // et il faut le présenter pour de bon (sans quoi `unprotect` refuse).
    if doc.is_encrypted() {
        if let Some(pw) = PASSWORD.get() {
            doc.authenticate(pw)?;
        }
    }
    if doc.needs_password() {
        match PASSWORD.get() {
            Some(pw) => doc.authenticate(pw)?,
            None => {
                return Err(acrux_core::Error::Unsupported(
                    "document chiffré : indiquer le mot de passe avec --password <mdp>".into(),
                ))
            }
        }
    }
    Ok((doc, t.elapsed()))
}

/// Valeur d'une option `--nom <valeur>`.
fn option_value<'a>(rest: &'a [String], name: &str) -> Option<&'a String> {
    rest.iter()
        .position(|a| a == name)
        .and_then(|i| rest.get(i + 1))
}

fn describe_action(action: &Action) -> String {
    match action {
        Action::GoTo(d) => {
            let view = match &d.view {
                View::Xyz { left, top, zoom } => format!(
                    "XYZ gauche={} haut={} zoom={}",
                    left.map_or("-".into(), |v| format!("{v:.1}")),
                    top.map_or("-".into(), |v| format!("{v:.1}")),
                    zoom.map_or("-".into(), |v| format!("{v:.2}"))
                ),
                View::Fit => "page entière".into(),
                View::FitWidth { top } => {
                    format!(
                        "largeur haut={}",
                        top.map_or("-".into(), |v| format!("{v:.1}"))
                    )
                }
                View::FitHeight { left } => {
                    format!(
                        "hauteur gauche={}",
                        left.map_or("-".into(), |v| format!("{v:.1}"))
                    )
                }
                View::FitRect(r) => format!(
                    "rectangle [{:.1} {:.1} {:.1} {:.1}]",
                    r.x0, r.y0, r.x1, r.y1
                ),
                View::FitBox => "boîte de contenu".into(),
            };
            format!("page {} ({view})", d.page + 1)
        }
        Action::Uri(u) => format!("URI {u}"),
        Action::Named(n) => format!("action {n}"),
        Action::Launch(f) => format!("lancer {f}"),
        Action::GoToRemote(f) => format!("document distant {f}"),
        Action::Unsupported(s) => format!("action non prise en charge /{s}"),
    }
}

fn cmd_links(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let index = PageIndex::new(&pages);
    let pos = positional(rest);
    let indices = match pos.first() {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    let mut total = 0;
    for i in indices {
        let links = page_links(&doc, &pages[i], &index)?;
        for l in &links {
            total += 1;
            println!(
                "page {:>4}  [{:.1} {:.1} {:.1} {:.1}]  {}",
                i + 1,
                l.rect.x0,
                l.rect.y0,
                l.rect.x1,
                l.rect.y1,
                describe_action(&l.action)
            );
        }
    }
    println!("{total} lien(s)");
    Ok(())
}

fn cmd_outline(path: &str) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let index = PageIndex::new(&pages);
    let items = outline(&doc, &index)?;
    let flat = flatten_outline(&items);
    if flat.is_empty() {
        println!("aucun signet");
        return Ok(());
    }
    for (depth, item) in &flat {
        let target = item
            .action
            .as_ref()
            .map_or_else(|| "(sans cible)".to_string(), describe_action);
        println!("{}{}  →  {target}", "  ".repeat(*depth), item.title);
    }
    println!("{} signet(s)", flat.len());
    Ok(())
}

// --- Filigranes, arrière-plans, en-têtes et pieds, numérotation Bates -------

/// Ancrage `--anchor`, décalage, rotation, échelle et marge, appliqués
/// par-dessus un placement de départ.
fn parse_placement(
    rest: &[String],
    base: acrux_features::stamp::Placement,
) -> acrux_core::Result<acrux_features::stamp::Placement> {
    use acrux_features::stamp::{Anchor, Scale};
    let mut placement = base;
    if let Some(name) = option_value(rest, "--anchor") {
        placement.anchor = Anchor::from_name(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "ancrage inconnu « {name} » : haut-gauche, haut-centre, haut-droite, \
                 milieu-gauche, centre, milieu-droite, bas-gauche, bas-centre, bas-droite"
            ))
        })?;
    }
    if let Some(spec) = option_value(rest, "--offset") {
        let v: Vec<f64> = spec
            .split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        if v.len() != 2 {
            return Err(acrux_core::Error::Corrupt(format!(
                "décalage « x,y » attendu, reçu « {spec} »"
            )));
        }
        placement.offset = (v[0], v[1]);
    }
    if let Some(v) = option_value(rest, "--rotation").and_then(|v| v.parse().ok()) {
        placement.rotation = v;
    }
    if let Some(v) = option_value(rest, "--margin").and_then(|v| v.parse().ok()) {
        placement.margin = v;
    }
    if rest.iter().any(|a| a == "--stretch") {
        placement.scale = Scale::Stretch;
    } else if let Some(v) = option_value(rest, "--fit").and_then(|v| v.parse().ok()) {
        placement.scale = Scale::RelativeToPage(v);
    } else if let Some(v) = option_value(rest, "--scale").and_then(|v| v.parse().ok()) {
        placement.scale = Scale::Absolute(v);
    }
    Ok(placement)
}

/// Police standard `--font` et corps `--size`.
fn parse_font(
    rest: &[String],
    default_font: acrux_features::stamp::StandardFont,
    default_size: f64,
) -> acrux_core::Result<(acrux_features::stamp::StandardFont, f64)> {
    let font = match option_value(rest, "--font") {
        Some(name) => acrux_features::stamp::StandardFont::from_name(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "police inconnue « {name} » : helvetica, times ou courier, \
                 éventuellement suivie de bold et / ou italic"
            ))
        })?,
        None => default_font,
    };
    let size = option_value(rest, "--size")
        .and_then(|v| v.parse().ok())
        .unwrap_or(default_size);
    Ok((font, size))
}

/// Dessin d'un filigrane ou d'un arrière-plan : `--image`, `--text` ou, à
/// défaut et si `fill_by_default`, un aplat de la couleur `--color`.
fn parse_source(
    rest: &[String],
    fill_by_default: bool,
) -> acrux_core::Result<acrux_features::stamp::StampSource> {
    use acrux_features::stamp::StampSource;
    let color = match option_value(rest, "--color") {
        Some(spec) => parse_rgb(spec)?,
        None => {
            if fill_by_default {
                [0.93, 0.93, 0.88]
            } else {
                [0.5, 0.5, 0.5]
            }
        }
    };
    if let Some(file) = option_value(rest, "--image") {
        return Ok(StampSource::Image {
            data: std::fs::read(file)?,
        });
    }
    if let Some(text) = option_value(rest, "--text") {
        let (font, size) = parse_font(
            rest,
            acrux_features::stamp::StandardFont::HelveticaBold,
            48.0,
        )?;
        return Ok(StampSource::Text {
            text: text.clone(),
            font,
            size,
            color,
        });
    }
    if fill_by_default {
        return Ok(StampSource::Fill { color });
    }
    Err(acrux_core::Error::Corrupt(
        "indiquer le contenu du filigrane : --text <texte> ou --image <fichier.png|jpg>".into(),
    ))
}

/// `--pages 1,3-5` ; absent = toutes les pages.
fn stamp_pages(doc: &Document, rest: &[String]) -> acrux_core::Result<Vec<usize>> {
    match option_value(rest, "--pages") {
        Some(spec) => {
            let n = collect_pages(doc)?.len();
            acrux_features::pages::parse_page_spec(spec, n)
        }
        None => Ok(Vec::new()),
    }
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("avertissement : {w}");
    }
}

fn cmd_watermark(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let defaults = acrux_features::stamp::WatermarkOptions::default();
    let opacity = option_value(rest, "--opacity")
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(defaults.opacity, |v| acrux_features::stamp::Opacity {
            fill: v,
            stroke: v,
        });
    let options = acrux_features::stamp::WatermarkOptions {
        source: parse_source(rest, false)?,
        placement: parse_placement(rest, defaults.placement)?,
        opacity,
        behind: rest.iter().any(|a| a == "--behind"),
        pages: stamp_pages(&doc, rest)?,
    };
    let report = acrux_features::stamp::add_watermark(&doc, &options)?;
    println!(
        "filigrane posé sur {} page(s) ({})",
        report.pages,
        if options.behind {
            "derrière le contenu"
        } else {
            "devant le contenu"
        }
    );
    print_warnings(&report.warnings);
    save(&doc, &out, rest)
}

fn cmd_background(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let defaults = acrux_features::stamp::BackgroundOptions::default();
    let opacity = option_value(rest, "--opacity")
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(defaults.opacity, |v| acrux_features::stamp::Opacity {
            fill: v,
            stroke: v,
        });
    let options = acrux_features::stamp::BackgroundOptions {
        source: parse_source(rest, true)?,
        placement: parse_placement(rest, defaults.placement)?,
        opacity,
        pages: stamp_pages(&doc, rest)?,
    };
    let report = acrux_features::stamp::add_background(&doc, &options)?;
    println!("arrière-plan posé sur {} page(s)", report.pages);
    print_warnings(&report.warnings);
    save(&doc, &out, rest)
}

fn cmd_header_footer(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let zone = |name: &str| option_value(rest, name).cloned().unwrap_or_default();
    let header = [
        zone("--header-left"),
        zone("--header-center"),
        zone("--header-right"),
    ];
    let footer = [
        zone("--footer-left"),
        zone("--footer-center"),
        zone("--footer-right"),
    ];
    if header.iter().chain(footer.iter()).all(String::is_empty) {
        return Err(acrux_core::Error::Corrupt(
            "aucune zone : indiquer au moins --header-left/-center/-right ou \
             --footer-left/-center/-right ; jetons {page}, {pages}, {date}, {time}, \
             {datetime}, {filename}"
                .into(),
        ));
    }
    let defaults = acrux_features::stamp::HeaderFooterOptions::default();
    let (font, size) = parse_font(rest, defaults.font, defaults.size)?;
    let margin = |name: &str, fallback: f64| {
        option_value(rest, name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    };
    let format = match option_value(rest, "--format") {
        Some(name) => acrux_features::stamp::NumberFormat::from_name(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "format de numéro inconnu « {name} » : 1, i, I, a ou A"
            ))
        })?,
        None => defaults.format,
    };
    let file_name = std::path::Path::new(path)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let options = acrux_features::stamp::HeaderFooterOptions {
        header,
        footer,
        font,
        size,
        color: match option_value(rest, "--color") {
            Some(spec) => parse_rgb(spec)?,
            None => defaults.color,
        },
        margins: acrux_features::stamp::Margins {
            top: margin("--margin-top", defaults.margins.top),
            bottom: margin("--margin-bottom", defaults.margins.bottom),
            left: margin("--margin-left", defaults.margins.left),
            right: margin("--margin-right", defaults.margins.right),
        },
        start_number: option_value(rest, "--start")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.start_number),
        format,
        file_name,
        pages: stamp_pages(&doc, rest)?,
        utc_offset_minutes: utc_offset(rest)?,
    };
    let report = acrux_features::stamp::add_header_footer(&doc, &options)?;
    println!("en-tête / pied posé sur {} page(s)", report.pages);
    print_warnings(&report.warnings);
    save(&doc, &out, rest)
}

/// Décalage horaire demandé par `--utc-offset`, en minutes. La forme est
/// `+HH:MM`, `-HH:MM` ou `+HH` ; sans l'option, les jetons de date restent en
/// UTC (aucune bibliothèque du projet ne lit le fuseau du système).
fn utc_offset(args: &[String]) -> acrux_core::Result<i32> {
    let Some(spec) = option_value(args, "--utc-offset") else {
        return Ok(0);
    };
    let bad = || {
        acrux_core::Error::Corrupt(format!(
            "décalage horaire illisible « {spec} » : attendu +HH:MM, -HH:MM ou +HH"
        ))
    };
    let (sign, rest) = match spec.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, spec.strip_prefix('+').unwrap_or(spec)),
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None => (rest, "0"),
    };
    let hours: i32 = hours.parse().map_err(|_| bad())?;
    let minutes: i32 = minutes.parse().map_err(|_| bad())?;
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return Err(bad());
    }
    Ok(sign * (hours * 60 + minutes))
}

/// Numérotation Bates, sur un ou plusieurs fichiers à la suite : la
/// numérotation continue d'un document au suivant.
fn cmd_bates(args: &[String]) -> acrux_core::Result<()> {
    let rest: Vec<String> = args.to_vec();
    let inputs: Vec<String> = positional(&rest).into_iter().cloned().collect();
    if inputs.is_empty() {
        return Err(acrux_core::Error::Corrupt(
            "usage : bates <fichier>... [options] -o <sortie>".into(),
        ));
    }
    let out = output_arg(&rest)?;
    let defaults = acrux_features::stamp::BatesOptions::default();
    let (font, size) = parse_font(&rest, defaults.font, defaults.size)?;
    let anchor = match option_value(&rest, "--anchor") {
        Some(name) => acrux_features::stamp::Anchor::from_name(name)
            .ok_or_else(|| acrux_core::Error::Corrupt(format!("ancrage inconnu « {name} »")))?,
        None => defaults.anchor,
    };
    let mut options = acrux_features::stamp::BatesOptions {
        prefix: option_value(&rest, "--prefix").cloned().unwrap_or_default(),
        suffix: option_value(&rest, "--suffix").cloned().unwrap_or_default(),
        digits: option_value(&rest, "--digits")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.digits),
        start: option_value(&rest, "--start")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.start),
        anchor,
        margin_x: option_value(&rest, "--margin-x")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.margin_x),
        margin_y: option_value(&rest, "--margin-y")
            .and_then(|v| v.parse().ok())
            .unwrap_or(defaults.margin_y),
        font,
        size,
        color: match option_value(&rest, "--color") {
            Some(spec) => parse_rgb(spec)?,
            None => defaults.color,
        },
        pages: Vec::new(),
    };
    // Chaque document est ouvert, numéroté puis enregistré à la suite : la
    // série reprend au numéro laissé par le précédent.
    for (index, input) in inputs.iter().enumerate() {
        let (doc, _) = open(input)?;
        options.pages = stamp_pages(&doc, &rest)?;
        let report = acrux_features::stamp::add_bates(&doc, &options)?;
        options.start = report.next;
        let file = if inputs.len() == 1 {
            out.clone()
        } else {
            let (stem, ext) = out.rsplit_once('.').unwrap_or((out.as_str(), "pdf"));
            format!("{stem}-{}.{ext}", index + 1)
        };
        print_warnings(&report.warnings);
        match (report.first, report.last) {
            (Some(first), Some(last)) => println!(
                "{input} : {} page(s), numéros {first} à {last}",
                report.pages
            ),
            _ => println!("{input} : aucune page numérotée"),
        }
        save(&doc, &file, &rest)?;
    }
    Ok(())
}

fn cmd_unstamp(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let kinds = match option_value(rest, "--kind") {
        Some(spec) => {
            let mut kinds = Vec::new();
            for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                kinds.push(
                    acrux_features::stamp::StampKind::from_name(name).ok_or_else(|| {
                        acrux_core::Error::Corrupt(format!(
                        "nature inconnue « {name} » : filigrane, arriere-plan, entete-pied, bates"
                    ))
                    })?,
                );
            }
            kinds
        }
        None => vec![
            acrux_features::stamp::StampKind::Watermark,
            acrux_features::stamp::StampKind::Background,
            acrux_features::stamp::StampKind::HeaderFooter,
            acrux_features::stamp::StampKind::Bates,
        ],
    };
    let removed = acrux_features::stamp::remove_stamps(&doc, &kinds)?;
    println!("{removed} tampon(s) retiré(s)");
    save(&doc, &out, rest)
}

/// Permissions d'après les options de `protect` : tout est permis, sauf ce
/// que les options retirent. Chaque option ne retire que son bit ; les liens
/// entre bits (commenter comprend remplir, copier comprend l'accessibilité)
/// sont ceux de [`Permissions::normalized`].
fn parse_permissions(rest: &[String]) -> acrux_core::Result<Permissions> {
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let mut perms = Permissions::all();
    if let Some(level) = option_value(rest, "--print") {
        perms.set_print(match level.as_str() {
            "none" | "non" => PrintLevel::None,
            "low" | "basse" => PrintLevel::Low,
            "high" | "haute" => PrintLevel::High,
            other => {
                return Err(acrux_core::Error::Unsupported(format!(
                    "--print {other} : attendu none, low ou high"
                )))
            }
        });
    }
    // Ancienne forme, gardée pour les scripts existants.
    if has("--no-print") {
        perms.set_print(PrintLevel::None);
    }
    for (flag, bit) in [
        ("--no-modify", &mut perms.modify),
        ("--no-copy", &mut perms.copy),
        ("--no-annotate", &mut perms.annotate),
        ("--no-fill", &mut perms.fill_forms),
        ("--no-accessibility", &mut perms.accessibility),
        ("--no-assemble", &mut perms.assemble),
    ] {
        if has(flag) {
            *bit = false;
        }
    }
    Ok(perms.normalized())
}

/// Force et mises en garde d'un mot de passe, sur la sortie d'erreur.
fn report_password(label: &str, pw: &[u8]) {
    let Ok(pw) = std::str::from_utf8(pw) else {
        return;
    };
    if pw.is_empty() {
        return;
    }
    let strength = password_strength(pw);
    eprintln!("{label} : force {}", strength.label().to_lowercase());
    if strength == Strength::Weak {
        eprintln!("  attention : ce mot de passe se devine vite");
    }
    for w in password_warnings(pw) {
        eprintln!("  attention : {w}");
    }
}

fn cmd_protect(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let user = option_value(rest, "--user")
        .map(String::as_bytes)
        .unwrap_or_default();
    let owner = option_value(rest, "--owner")
        .map(String::as_bytes)
        .unwrap_or_default();
    let perms = parse_permissions(rest)?;
    if user.is_empty() && owner.is_empty() {
        return Err(acrux_core::Error::Unsupported(
            "indiquer au moins un mot de passe (--user ou --owner)".into(),
        ));
    }
    // Sans mot de passe des permissions distinct, celui d'ouverture donne
    // tous les droits : les restrictions ne vaudraient rien.
    if !perms.is_all() && (owner.is_empty() || owner == user) {
        return Err(acrux_core::Error::Unsupported(
            "des restrictions exigent un mot de passe des permissions (--owner) distinct de --user"
                .into(),
        ));
    }
    let (doc, _) = open(path)?;
    report_password("--user", user);
    report_password("--owner", owner);
    doc.protect(user, owner, perms)?;
    let bytes = doc.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!(
        "écrit : {out} (AES-256, {} octets){}",
        bytes.len(),
        if user.is_empty() {
            " — ouverture libre"
        } else {
            " — mot de passe demandé à l'ouverture"
        }
    );
    let restrictions = perms.restrictions();
    println!(
        "permissions : {}",
        if restrictions.is_empty() {
            "tout autorisé".to_string()
        } else {
            restrictions.join(", ")
        }
    );
    Ok(())
}

fn cmd_unprotect(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    doc.unprotect()?;
    let bytes = doc.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!("écrit : {out} (sans chiffrement, {} octets)", bytes.len());
    Ok(())
}

#[allow(clippy::too_many_lines)] // affichage séquentiel
fn cmd_info(path: &str) -> acrux_core::Result<()> {
    let (doc, elapsed) = open(path)?;
    let pages = collect_pages(&doc)?;
    let (maj, min) = doc.version();
    println!("Fichier        : {path}");
    println!("Taille         : {} octets", doc.bytes().len());
    println!("Version PDF    : {maj}.{min}");
    println!("Pages          : {}", pages.len());
    if let Some(p) = pages.first() {
        let mb = p.media_box(&doc);
        println!(
            "Page 1         : {:.2} × {:.2} pt ({:.1} × {:.1} mm), rotation {}°",
            mb.width(),
            mb.height(),
            mb.width() * 25.4 / 72.0,
            mb.height() * 25.4 / 72.0,
            p.rotate(&doc)
        );
    }
    println!(
        "Table xref     : {}{}",
        match doc.xref_kind() {
            XrefKind::Table => "table classique",
            XrefKind::Stream => "flux xref (PDF 1.5+)",
            XrefKind::Reconstructed => "RECONSTRUITE par balayage",
        },
        if doc.was_repaired() {
            " (fichier réparé)"
        } else {
            ""
        }
    );
    println!("Objets         : {}", doc.xref_len());
    println!(
        "Chiffré        : {}",
        if doc.is_encrypted() { "oui" } else { "non" }
    );
    if let Some(h) = doc.security() {
        print_security(&h);
    }
    let trailer = doc.trailer();
    if let Some(Object::Array(ids)) = trailer.get(&Name::new("ID")) {
        if let Some(Object::String(id)) = ids.first() {
            println!("ID             : {}", hex(id));
        }
        if let Some(Object::String(id)) = ids.get(1) {
            println!("ID (version)   : {}", hex(id));
        }
    }
    let catalog = doc.catalog()?;
    let mut features = Vec::new();
    for (key, label) in [
        ("AcroForm", "formulaires"),
        ("Outlines", "signets"),
        ("Names", "noms"),
        ("OCProperties", "calques"),
        ("StructTreeRoot", "balisage"),
        ("Metadata", "XMP"),
        ("PageLabels", "étiquettes de pages"),
        ("Collection", "portfolio"),
        ("OpenAction", "action d'ouverture"),
        ("Lang", "langue"),
    ] {
        if catalog.contains_key(&Name::new(key)) {
            features.push(label);
        }
    }
    if let Some(Object::Name(m)) = catalog.get(&Name::new("PageMode")) {
        features.push(match m.0.as_slice() {
            b"FullScreen" => "plein écran",
            _ => "mode de page",
        });
    }
    println!(
        "Catalogue      : {}",
        if features.is_empty() {
            "—".to_string()
        } else {
            features.join(", ")
        }
    );
    if let Some(info) = doc.info() {
        for (key, label) in [
            ("Title", "Titre"),
            ("Author", "Auteur"),
            ("Subject", "Sujet"),
            ("Keywords", "Mots-clés"),
            ("Creator", "Créateur"),
            ("Producer", "Producteur"),
            ("CreationDate", "Créé le"),
            ("ModDate", "Modifié le"),
        ] {
            if let Ok(Some(v)) = doc.dict_get(&info, key) {
                if let Object::String(s) = &*v {
                    println!("{label:<15}: {}", decode_text_string(s));
                }
            }
        }
    }
    let warnings = doc.warnings();
    if !warnings.is_empty() {
        println!("Avertissements : {}", warnings.len());
        for w in warnings.iter().take(10) {
            println!("  - {w}");
        }
    }
    println!("Ouvert en      : {:.1} ms", elapsed.as_secs_f64() * 1000.0);
    Ok(())
}

/// Chiffrement, droits d'accès et permissions d'un document chiffré.
fn print_security(h: &acrux_document::crypt::SecurityHandler) {
    println!("Chiffrement    : {}", h.describe());
    println!(
        "Accès          : {}",
        if h.is_owner() {
            "propriétaire (tous les droits)"
        } else {
            "utilisateur"
        }
    );
    let perms = Permissions::from_p(h.permissions());
    let restrictions = perms.restrictions();
    println!(
        "Permissions    : {}",
        if restrictions.is_empty() {
            "tout autorisé".to_string()
        } else {
            restrictions.join(", ")
        }
    );
    println!(
        "Impression     : {}",
        match perms.print_level() {
            PrintLevel::None => "non",
            PrintLevel::Low => "basse résolution",
            PrintLevel::High => "haute résolution",
        }
    );
    if h.perms_tampered() {
        println!(
            "Attention      : /Perms contredit /P (permissions retouchées ?) ; la plus stricte s'applique"
        );
    }
}

fn cmd_pages(path: &str) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    println!("{} page(s)", pages.len());
    for p in &pages {
        let mb = p.media_box(&doc);
        let cb = p.crop_box(&doc);
        let r = p.reference.map_or("direct".to_string(), |r| {
            format!("{} {} R", r.number, r.generation)
        });
        println!(
            "{:>5}  obj {:<10} media [{:.0} {:.0} {:.0} {:.0}]  crop [{:.0} {:.0} {:.0} {:.0}]  rot {:>3}°  {}",
            p.index + 1,
            r,
            mb.x0,
            mb.y0,
            mb.x1,
            mb.y1,
            cb.x0,
            cb.y0,
            cb.x1,
            cb.y1,
            p.rotate(&doc),
            if p.dict.contains_key(&Name::new("Annots")) { "annots" } else { "" }
        );
    }
    Ok(())
}

fn cmd_dump(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let want_data = rest.iter().any(|a| a == "--data");
    let target = rest
        .iter()
        .find(|a| !a.starts_with("--"))
        .map_or("trailer", String::as_str);
    let obj: Object = match target {
        "trailer" => Object::Dict(doc.trailer()),
        "catalog" => Object::Dict(doc.catalog()?),
        n => {
            let number: u32 = n.parse().map_err(|_| {
                acrux_core::Error::Corrupt(format!("numéro d'objet invalide : {n}"))
            })?;
            (*doc.get(ObjectRef {
                number,
                generation: 0,
            })?)
            .clone()
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match &obj {
        Object::Stream { dict, raw } => {
            let decoded = doc.stream_data(&obj);
            let _ = writeln!(out, "{}", writer::to_string(&Object::Dict(dict.clone())));
            match &decoded {
                Ok(d) => {
                    let _ = writeln!(
                        out,
                        "stream : {} octets bruts, {} octets décodés{}",
                        raw.len(),
                        d.data.len(),
                        d.image_filter
                            .as_ref()
                            .map(|(n, _)| format!(" (reste : {})", String::from_utf8_lossy(n)))
                            .unwrap_or_default()
                    );
                }
                Err(e) => {
                    let _ = writeln!(
                        out,
                        "stream : {} octets bruts, décodage impossible : {e}",
                        raw.len()
                    );
                }
            }
            if want_data {
                let bytes = match &decoded {
                    Ok(d) => d.data.clone(),
                    Err(_) => raw.clone(),
                };
                let _ = out.write_all(&bytes);
                let _ = writeln!(out);
            }
        }
        other => {
            let _ = writeln!(out, "{}", writer::to_string(other));
        }
    }
    Ok(())
}

fn cmd_check(path: &str) -> acrux_core::Result<()> {
    let (doc, elapsed) = open(path)?;
    let numbers = doc.object_numbers();
    let mut errors = 0usize;
    let mut streams = 0usize;
    let mut stream_errors = 0usize;
    let mut by_type: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let t = Instant::now();
    for n in &numbers {
        let r = ObjectRef {
            number: *n,
            generation: 0,
        };
        match doc.get(r) {
            Ok(o) => {
                if let Some(d) = o.as_dict() {
                    if let Some(Object::Name(t)) = d.get(&Name::new("Type")) {
                        *by_type.entry(t.as_str()).or_insert(0) += 1;
                    }
                }
                if matches!(&*o, Object::Stream { .. }) {
                    streams += 1;
                    if let Err(e) = doc.stream_data(&o) {
                        stream_errors += 1;
                        if stream_errors <= 20 {
                            println!("flux {n} : {e}");
                        }
                    }
                }
            }
            Err(e) => {
                errors += 1;
                if errors <= 20 {
                    println!("objet {n} : {e}");
                }
            }
        }
    }
    let pages = collect_pages(&doc).map_or(0, |p| p.len());
    println!();
    println!(
        "{} objets, {} flux, {} pages",
        numbers.len(),
        streams,
        pages
    );
    println!("{errors} objet(s) illisible(s), {stream_errors} flux non décodable(s)");
    if !by_type.is_empty() {
        let types: Vec<String> = by_type.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("types : {}", types.join(" "));
    }
    let warnings = doc.warnings();
    if !warnings.is_empty() {
        println!("{} avertissement(s) :", warnings.len());
        for w in warnings.iter().take(20) {
            println!("  - {w}");
        }
    }
    println!(
        "ouverture {:.1} ms, parcours {:.1} ms",
        elapsed.as_secs_f64() * 1000.0,
        t.elapsed().as_secs_f64() * 1000.0
    );
    if errors > 0 || stream_errors > 0 {
        return Err(acrux_core::Error::Corrupt(
            "des objets sont illisibles".into(),
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Nom du fichier de sortie (`-o`), obligatoire pour les commandes d'écriture.
fn output_arg(rest: &[String]) -> acrux_core::Result<String> {
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "-o" || a == "--output" {
            return it
                .next()
                .cloned()
                .ok_or_else(|| acrux_core::Error::Corrupt("-o attend un nom de fichier".into()));
        }
    }
    Err(acrux_core::Error::Corrupt(
        "fichier de sortie manquant : ajouter -o <sortie>".into(),
    ))
}

#[allow(clippy::too_many_lines)] // la liste des options à valeur, une par ligne
fn positional(rest: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in rest {
        if skip {
            skip = false;
            continue;
        }
        if matches!(
            a.as_str(),
            "-o" | "--output"
                | "--dpi"
                | "--user"
                | "--owner"
                | "--format"
                | "--quality"
                | "--pages"
                | "--page"
                | "--rect"
                | "--text"
                | "--color"
                | "--key"
                | "--cert"
                | "--chain"
                | "--roots"
                | "--reason"
                | "--location"
                | "--contact"
                | "--field"
                | "--find"
                | "--pattern"
                | "--image"
                | "--object"
                | "--extract"
                | "--move"
                | "--rotate"
                | "--place"
                | "--crop"
                | "--order"
                | "--draw"
                | "--typed"
                | "--mark"
                | "--pen"
                | "--softness"
                | "--font"
                | "--size"
                | "--opacity"
                | "--rotation"
                | "--anchor"
                | "--offset"
                | "--scale"
                | "--fit"
                | "--margin"
                | "--margin-top"
                | "--margin-bottom"
                | "--margin-left"
                | "--margin-right"
                | "--margin-x"
                | "--margin-y"
                | "--header-left"
                | "--header-center"
                | "--header-right"
                | "--footer-left"
                | "--footer-center"
                | "--footer-right"
                | "--start"
                | "--prefix"
                | "--suffix"
                | "--digits"
                | "--kind"
                | "--description"
                | "--orientation"
                | "--align"
                | "--grid"
                | "--page-size"
                | "--background"
                | "--header"
                | "--footer"
                | "--toc-title"
                | "--title"
                | "--author"
                | "--subject"
                | "--keywords"
                | "--creator"
                | "--producer"
                | "--page-mode"
                | "--layout"
                | "--open-page"
                | "--open-zoom"
                | "--fill"
                | "--width"
                | "--border"
                | "--head"
                | "--tail"
                | "--points"
                | "--state"
                | "--marked"
        ) {
            skip = true;
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        out.push(a);
    }
    out
}

/// Enregistre : incrémental par défaut (le fichier d'origine est conservé),
/// complet avec `--full` ou quand l'incrémental est impossible.
fn save(doc: &Document, out_path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let full = rest.iter().any(|a| a == "--full");
    let bytes = if full {
        doc.save_full()?
    } else {
        match doc.save_incremental() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("enregistrement incrémental impossible ({e}) : réécriture complète");
                doc.save_full()?
            }
        }
    };
    std::fs::write(out_path, &bytes)?;
    println!("écrit : {out_path} ({} octets)", bytes.len());
    Ok(())
}

fn pages_arg(doc: &Document, spec: &str) -> acrux_core::Result<Vec<usize>> {
    let n = collect_pages(doc)?.len();
    acrux_features::pages::parse_page_spec(spec, n)
}

fn cmd_rotate(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let (Some(spec), Some(angle)) = (pos.first(), pos.get(1)) else {
        return Err(acrux_core::Error::Corrupt(
            "usage : rotate <fichier> <pages> <angle> -o <sortie>".into(),
        ));
    };
    let angle: i32 = angle
        .parse()
        .map_err(|_| acrux_core::Error::Corrupt(format!("angle invalide : {angle}")))?;
    let (doc, _) = open(path)?;
    let pages = pages_arg(&doc, spec)?;
    acrux_features::pages::rotate_pages(&doc, &pages, angle)?;
    save(&doc, &out, rest)
}

fn cmd_delete(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(spec) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : delete <fichier> <pages> -o <sortie>".into(),
        ));
    };
    let (doc, _) = open(path)?;
    let pages = pages_arg(&doc, spec)?;
    acrux_features::pages::delete_pages(&doc, &pages)?;
    save(&doc, &out, rest)
}

fn cmd_reorder(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(spec) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : reorder <fichier> <pages> -o <sortie>".into(),
        ));
    };
    let (doc, _) = open(path)?;
    let pages = pages_arg(&doc, spec)?;
    acrux_features::pages::reorder_pages(&doc, &pages)?;
    save(&doc, &out, rest)
}

fn cmd_extract(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(spec) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : extract <fichier> <pages> -o <sortie>".into(),
        ));
    };
    let (doc, _) = open(path)?;
    let pages = pages_arg(&doc, spec)?;
    let new_doc = acrux_features::pages::extract_pages(&doc, &pages)?;
    let bytes = new_doc.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!(
        "écrit : {out} ({} octets, {} page(s))",
        bytes.len(),
        pages.len()
    );
    Ok(())
}

fn cmd_merge(rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let inputs = positional(rest);
    if inputs.is_empty() {
        return Err(acrux_core::Error::Corrupt(
            "usage : merge <fichier>... -o <sortie>".into(),
        ));
    }
    let mut docs = Vec::with_capacity(inputs.len());
    for p in &inputs {
        docs.push(Document::load(p)?);
    }
    let refs: Vec<&Document> = docs.iter().collect();
    let merged = acrux_features::pages::merge(&refs)?;
    let bytes = merged.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!(
        "écrit : {out} ({} octets, {} page(s))",
        bytes.len(),
        collect_pages(&merged)?.len()
    );
    Ok(())
}

fn cmd_rewrite(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let compact = has("--compact");
    let options = acrux_document::SaveOptions {
        compress_streams: compact || has("--compress"),
        object_streams: compact || has("--objstm"),
        drop_unreferenced: compact || has("--gc"),
        ..acrux_document::SaveOptions::default()
    };
    let bytes = doc.save_full_with(&options)?;
    std::fs::write(&out, &bytes)?;
    println!(
        "écrit : {out} ({} → {} octets)",
        doc.bytes().len(),
        bytes.len()
    );
    Ok(())
}

/// Valeur de `--rotate` pour `render` : un quart de tour entier, dans le sens
/// horaire (négatif : l'autre sens) ; 0 sans l'option.
///
/// # Errors
/// Angle illisible ou qui n'est pas un multiple de 90.
fn view_rotation_arg(rest: &[String]) -> acrux_core::Result<i32> {
    let Some(value) = option_value(rest, "--rotate") else {
        return Ok(0);
    };
    match value.parse::<i32>() {
        Ok(degrees) if degrees % 90 == 0 => Ok(degrees),
        _ => Err(acrux_core::Error::Corrupt(format!(
            "--rotate attend un multiple de 90 degrés, pas « {value} »"
        ))),
    }
}

fn cmd_render(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let pos = positional(rest);
    let indices = match pos.first() {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    let dpi: f64 = rest
        .iter()
        .position(|a| a == "--dpi")
        .and_then(|i| rest.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(96.0);
    // Tourner l'image, pas le document : c'est la rotation de la vue de
    // l'application (Ctrl+Maj+Plus), pour voir debout une page couchée sans
    // rien réécrire. `acr rotate` écrit `/Rotate`, lui.
    let extra = view_rotation_arg(rest)?;
    let out = output_arg(rest).unwrap_or_else(|_| {
        let stem = std::path::Path::new(path)
            .file_stem()
            .map_or_else(|| "page".into(), |s| s.to_string_lossy().into_owned());
        format!("{stem}.png")
    });
    let options = acrux_render::RenderOptions {
        annotations: !rest.iter().any(|a| a == "--no-annots"),
        time_budget: Some(std::time::Duration::from_secs(60)),
        background: Some(acrux_graphics::Color::WHITE),
        ..acrux_render::RenderOptions::default()
    };
    for &i in &indices {
        let page = &pages[i];
        let t = Instant::now();
        let rendered = acrux_render::render_page_rotated(&doc, page, dpi / 72.0, extra, &options);
        let png = acrux_graphics::encode_png(&rendered.bitmap);
        let file = if indices.len() == 1 {
            out.clone()
        } else {
            let (stem, ext) = out.rsplit_once('.').unwrap_or((&out, "png"));
            format!("{stem}-{}.{ext}", i + 1)
        };
        std::fs::write(&file, &png)?;
        println!(
            "page {} : {}×{} px en {:.0} ms → {file}{}",
            i + 1,
            rendered.bitmap.width(),
            rendered.bitmap.height(),
            t.elapsed().as_secs_f64() * 1000.0,
            if rendered.warnings.is_empty() {
                String::new()
            } else {
                format!(" ({} avertissement(s))", rendered.warnings.len())
            }
        );
        for w in rendered.warnings.iter().take(8) {
            println!("    - {w}");
        }
    }
    Ok(())
}

fn cmd_text(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use std::fmt::Write as _;
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let pos = positional(rest);
    let indices = match pos.first() {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    let markdown = rest.iter().any(|a| a == "--markdown" || a == "--md");
    let html = rest.iter().any(|a| a == "--html");
    let layout = rest.iter().any(|a| a == "--layout");
    let mut texts = Vec::with_capacity(indices.len());
    for &i in &indices {
        texts.push(acrux_features::text::extract_page_text(&doc, &pages[i])?);
    }
    // En-têtes et pieds répétés d'une page à l'autre.
    acrux_features::text::mark_repeated_headers(&mut texts);
    let mut out = String::new();
    if html {
        out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>");
        out.push_str(&html_escape(path));
        out.push_str("</title></head><body>\n");
    }
    for (&i, text) in indices.iter().zip(&texts) {
        if html {
            let _ = writeln!(out, "<section class=\"page\" data-page=\"{}\">", i + 1);
            out.push_str(&text.to_html());
            out.push_str("</section>\n");
            continue;
        }
        if indices.len() > 1 {
            if markdown {
                if !out.is_empty() {
                    out.push_str("\n---\n\n");
                }
            } else {
                let _ = writeln!(out, "=== page {} ===", i + 1);
            }
        }
        if markdown {
            out.push_str(&text.to_markdown());
        } else if layout {
            out.push_str(&text.to_layout());
        } else {
            out.push_str(&text.to_plain());
        }
    }
    if html {
        out.push_str("</body></html>\n");
    }
    if let Ok(file) = output_arg(rest) {
        std::fs::write(&file, out.as_bytes())?;
        println!("écrit : {file}");
    } else {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        let _ = lock.write_all(out.as_bytes());
    }
    Ok(())
}

/// Casse et mot entier, tels que `--case` et `--word` les demandent.
fn search_options(rest: &[String]) -> acrux_features::text::SearchOptions {
    acrux_features::text::SearchOptions {
        match_case: rest.iter().any(|a| a == "--case"),
        whole_word: rest.iter().any(|a| a == "--word"),
    }
}

/// `find` : cherche un texte avec le moteur de la carte de recherche de
/// l'application — même ordre, mêmes options, mêmes occurrences à cheval sur
/// deux lignes. Une ligne par occurrence, puis le total.
fn cmd_find(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let usage = "usage : find <fichier> <texte> [--case] [--word] [--page N]";
    let pos = positional(rest);
    let Some(needle) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(usage.into()));
    };
    let options = search_options(rest);
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let indices = match option_value(rest, "--page") {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    let mut total = 0;
    for index in indices {
        let text = acrux_features::text::extract_page_text(&doc, &pages[index])?;
        for m in acrux_features::text::find_matches(&text, needle, options) {
            let Some(first) = m.pieces.first() else {
                continue;
            };
            total += 1;
            let context: Vec<String> = m
                .pieces
                .iter()
                .filter_map(|p| text.lines.get(p.line).map(acrux_features::text::Line::text))
                .collect();
            let spans = if m.pieces.len() > 1 {
                " (sur deux lignes)"
            } else {
                ""
            };
            println!(
                "page {}, ligne {} : {}{spans}",
                index + 1,
                first.line + 1,
                context.join(" / ")
            );
        }
    }
    println!("{total} occurrence(s) de « {needle} »");
    Ok(())
}

/// `export` : conversion vers un autre format (phase 8 de la feuille de route).
///
/// `render` reste la commande dédiée au rendu PNG d'une page ; `export`
/// couvre tous les formats de sortie, images comprises.
// Un `match` d'une branche par format : le découper éparpillerait la lecture.
#[allow(clippy::too_many_lines)]
fn cmd_export(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::export;

    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let out = output_arg(rest)?;
    // Format explicite, sinon déduit de l'extension de la sortie.
    let format = match option_value(rest, "--format") {
        Some(f) => f.to_ascii_lowercase(),
        None => out
            .rsplit_once('.')
            .map_or_else(|| "png".into(), |(_, ext)| ext.to_ascii_lowercase()),
    };
    let spec = option_value(rest, "--pages")
        .cloned()
        .or_else(|| positional(rest).first().map(|s| (*s).clone()));
    let indices = match &spec {
        Some(s) => acrux_features::pages::parse_page_spec(s, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    let dpi: f64 = option_value(rest, "--dpi")
        .and_then(|v| v.parse().ok())
        .unwrap_or(150.0);
    let quality: u8 = option_value(rest, "--quality")
        .and_then(|v| v.parse().ok())
        .unwrap_or(85);
    let flow = rest.iter().any(|a| a == "--flow");
    let stem = std::path::Path::new(path)
        .file_stem()
        .map_or_else(|| "document".into(), |s| s.to_string_lossy().into_owned());

    // Nom de fichier d'une page : « sortie-N.ext » dès qu'il y en a plusieurs.
    let numbered = |index: usize| -> String {
        if indices.len() == 1 {
            out.clone()
        } else {
            let (base, ext) = out.rsplit_once('.').unwrap_or((out.as_str(), "png"));
            format!("{base}-{}.{ext}", index + 1)
        }
    };
    match format.as_str() {
        "png" => export::export_pages_png(&doc, &indices, dpi, &mut |i, data| {
            let file = numbered(i);
            std::fs::write(&file, data)?;
            println!("page {} : {} octets → {file}", i + 1, data.len());
            Ok(())
        })?,
        "jpeg" | "jpg" => {
            export::export_pages_jpeg(&doc, &indices, dpi, quality, true, &mut |i, data| {
                let file = numbered(i);
                std::fs::write(&file, data)?;
                println!("page {} : {} octets → {file}", i + 1, data.len());
                Ok(())
            })?;
        }
        "images" => {
            let images = export::extract_images(&doc)?;
            let mut written = 0usize;
            for img in images.iter().filter(|i| indices.contains(&i.page)) {
                let base = out.rsplit_once('.').map_or(out.as_str(), |(b, _)| b);
                let file = format!(
                    "{base}-p{}-{}.{}",
                    img.page + 1,
                    img.name,
                    img.format.extension()
                );
                std::fs::write(&file, &img.data)?;
                println!(
                    "page {} : {} {}×{} {} {} bits → {file}",
                    img.page + 1,
                    img.name,
                    img.width,
                    img.height,
                    img.colorspace,
                    img.bits
                );
                written += 1;
            }
            println!("{written} image(s) extraite(s)");
        }
        "html" => {
            let options = export::HtmlOptions {
                layout: if flow {
                    export::HtmlLayout::Flow
                } else {
                    export::HtmlLayout::Absolute
                },
                title: stem,
                pages: indices.clone(),
                ..export::HtmlOptions::default()
            };
            let html = export::export_html(&doc, &options)?;
            std::fs::write(&out, html.as_bytes())?;
            println!("écrit : {out} ({} octets)", html.len());
        }
        "docx" => {
            let data = export::export_docx(&doc)?;
            std::fs::write(&out, &data)?;
            println!("écrit : {out} ({} octets)", data.len());
        }
        "xlsx" => {
            let data = export::export_tables_xlsx(&doc)?;
            std::fs::write(&out, &data)?;
            println!("écrit : {out} ({} octets)", data.len());
        }
        "md" | "markdown" | "txt" | "text" => {
            let texts = export::page_texts(&doc)?;
            let markdown = matches!(format.as_str(), "md" | "markdown");
            let mut buffer = String::new();
            for (n, &i) in indices.iter().enumerate() {
                let Some(text) = texts.get(i) else { continue };
                if n > 0 {
                    buffer.push_str(if markdown { "\n---\n\n" } else { "\u{c}" });
                }
                buffer.push_str(&if markdown {
                    text.to_markdown()
                } else {
                    text.to_plain()
                });
            }
            std::fs::write(&out, buffer.as_bytes())?;
            println!("écrit : {out} ({} octets)", buffer.len());
        }
        other => {
            return Err(acrux_core::Error::Unsupported(format!(
            "format d'export « {other} » : attendu png, jpeg, images, html, docx, xlsx, md ou txt"
        )))
        }
    }
    Ok(())
}

/// Encode les caractères spéciaux HTML d'un texte.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn cmd_bench(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let t0 = Instant::now();
    let doc = Document::load(path)?;
    let open_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let pages = collect_pages(&doc)?;
    let dpi: f64 = rest
        .iter()
        .position(|a| a == "--dpi")
        .and_then(|i| rest.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(96.0);
    let options = acrux_render::RenderOptions {
        annotations: true,
        time_budget: None,
        background: Some(acrux_graphics::Color::WHITE),
        ..acrux_render::RenderOptions::default()
    };
    let mut total = 0.0;
    let mut slowest = (0usize, 0.0f64);
    let mut warnings = 0usize;
    for (i, page) in pages.iter().enumerate() {
        let t = Instant::now();
        let r = acrux_render::render_page(&doc, page, dpi / 72.0, &options);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        warnings += r.warnings.len();
        if ms > slowest.1 {
            slowest = (i + 1, ms);
        }
    }
    let t = Instant::now();
    let mut chars = 0usize;
    for page in &pages {
        chars += acrux_features::text::extract_page_text(&doc, page)?
            .to_plain()
            .chars()
            .count();
    }
    let text_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("ouverture        : {open_ms:.1} ms");
    println!(
        "rendu {} page(s) : {total:.1} ms ({:.1} ms/page, la plus lente : page {} en {:.1} ms) à {dpi} dpi",
        pages.len(),
        total / f64::from(u32::try_from(pages.len().max(1)).unwrap_or(1)),
        slowest.0,
        slowest.1
    );
    println!("texte            : {text_ms:.1} ms, {chars} caractères");
    if warnings > 0 {
        println!("avertissements   : {warnings}");
    }
    Ok(())
}

fn cmd_annots(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::annotations::review::comment_threads;
    use acrux_features::annotations::{list_annotations, readable_date};
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    // `-v` montre en plus l'identifiant de chaque annotation ; la sortie par
    // défaut ne change pas, des scripts la lisent peut-être.
    let verbose = rest.iter().any(|a| a == "-v" || a == "--verbose");
    let pos: Vec<&String> = positional(rest)
        .into_iter()
        .filter(|a| a.as_str() != "-v")
        .collect();
    let indices = match pos.first() {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    // Les fils de discussion : les réponses et les statuts se lisent sous
    // leur commentaire, plutôt qu'en lignes à part.
    let threads = comment_threads(&doc, &pages);
    let in_thread: std::collections::HashSet<(usize, usize)> = threads
        .iter()
        .flat_map(|t| t.replies.iter().map(|r| (r.page, r.index)))
        .collect();
    let mut total = 0;
    for &i in &indices {
        let list = list_annotations(&doc, &pages[i])?;
        for a in &list {
            total += 1;
            if a.is_state() || in_thread.contains(&(i, a.index)) {
                continue;
            }
            println!(
                "page {:>3}  #{:<3} {:<12} [{:.0} {:.0} {:.0} {:.0}]{}{}{}{}{}",
                i + 1,
                a.index + 1,
                a.subtype,
                a.rect.x0,
                a.rect.y0,
                a.rect.x1,
                a.rect.y1,
                a.author
                    .as_ref()
                    .map_or(String::new(), |t| format!("  par {t}")),
                a.contents
                    .as_ref()
                    .map_or(String::new(), |c| format!("  « {} »", c.replace('\n', " "))),
                a.uri.as_ref().map_or(String::new(), |u| format!("  → {u}")),
                if a.has_appearance {
                    ""
                } else {
                    "  (sans apparence)"
                },
                match &a.name {
                    Some(nm) if verbose => format!("  [{}]", short_id(nm)),
                    _ => String::new(),
                }
            );
            // Le barré d'un remplacement n'est pas un commentaire à part :
            // il suit le signe d'insertion qui porte le texte proposé.
            if a.is_group_member() {
                let primary = list
                    .iter()
                    .find(|p| p.reference.is_some() && p.reference == a.in_reply_to)
                    .map_or(String::new(), |p| format!(" #{}", p.index + 1));
                println!("            ↳ groupé avec{primary}");
            }
            let Some(thread) = threads.iter().find(|t| t.page == i && t.index == a.index) else {
                continue;
            };
            let mut detail: Vec<String> = Vec::new();
            if let Some(date) = &thread.modified {
                detail.push(format!("le {}", readable_date(date)));
            }
            if let Some((state, who)) = &thread.review {
                detail.push(match who {
                    Some(who) => format!("[{}, par {who}]", state.label()),
                    None => format!("[{}]", state.label()),
                });
            }
            if thread.marked {
                detail.push("[✓]".to_string());
            }
            if !detail.is_empty() {
                println!("            {}", detail.join(" · "));
            }
            for r in &thread.replies {
                println!(
                    "            {}↳ #{} {}{} : {}",
                    "  ".repeat(r.depth.saturating_sub(1)),
                    r.index + 1,
                    r.author.as_deref().unwrap_or("?"),
                    r.modified
                        .as_deref()
                        .map_or(String::new(), |d| format!(" ({})", readable_date(d))),
                    r.contents.replace('\n', " ")
                );
            }
        }
    }
    println!("{total} annotation(s)");
    Ok(())
}

/// Page et rang (comptés à partir de 1 sur la ligne de commande, comme les
/// affiche `annots`) d'une commande qui vise une annotation, vérifiés
/// contre le document.
fn annotation_target(
    doc: &Document,
    pos: &[&String],
    usage: &str,
) -> acrux_core::Result<(Vec<acrux_document::Page>, usize, usize)> {
    let (Some(page), Some(number)) = (pos.first(), pos.get(1)) else {
        return Err(acrux_core::Error::Corrupt(usage.to_string()));
    };
    let pages = collect_pages(doc)?;
    let page = page
        .parse::<usize>()
        .ok()
        .filter(|p| (1..=pages.len()).contains(p))
        .ok_or_else(|| acrux_core::Error::Corrupt(format!("page « {page} » inexistante")))?
        - 1;
    let count = acrux_features::annotations::list_annotations(doc, &pages[page])?.len();
    let index = number
        .trim_start_matches('#')
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=count).contains(n))
        .ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "annotation « {number} » inexistante en page {} ({count} annotation(s))",
                page + 1
            ))
        })?
        - 1;
    Ok((pages, page, index))
}

/// Couleur en ligne de commande : « RRGGBB » (ou « #RRGGBB »), comme la
/// barre de propriétés, ou « r,g,b » de 0 à 1, comme `annotate`.
fn color_arg(spec: &str) -> acrux_core::Result<[f64; 3]> {
    let hex = spec.trim().trim_start_matches('#');
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let byte =
            |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_or(0.0, |b| f64::from(b) / 255.0);
        return Ok([byte(0), byte(2), byte(4)]);
    }
    parse_rgb(spec)
}

/// Usage de `annot-set`.
const ANNOT_SET_USAGE: &str =
    "usage : annot-set <fichier> <page> <n°> [--rect x0,y0,x1,y1] [--move dx,dy] \
[--color RRGGBB] [--fill RRGGBB|none] [--opacity 0..1] [--width N] [--text \"…\"] \
[--state accepted|rejected|cancelled|completed|none] [--marked oui|non] [--author NOM] -o <sortie>";

/// Statut de relecture nommé en ligne de commande, en anglais comme dans le
/// fichier ou en français.
fn review_state_arg(
    rest: &[String],
) -> acrux_core::Result<Option<acrux_features::annotations::review::ReviewState>> {
    use acrux_features::annotations::review::ReviewState;
    let Some(s) = option_value(rest, "--state").map(|s| s.to_lowercase()) else {
        return Ok(None);
    };
    Ok(Some(match s.as_str() {
        "accepted" | "accepte" | "accepté" => ReviewState::Accepted,
        "rejected" | "refuse" | "refusé" => ReviewState::Rejected,
        "cancelled" | "annule" | "annulé" => ReviewState::Cancelled,
        "completed" | "termine" | "terminé" => ReviewState::Completed,
        "none" | "aucun" => ReviewState::None,
        _ => {
            return Err(acrux_core::Error::Corrupt(format!(
                "--state : statut inconnu « {s} »\n{ANNOT_SET_USAGE}"
            )))
        }
    }))
}

/// `--marked oui|non`.
fn marked_arg(rest: &[String]) -> acrux_core::Result<Option<bool>> {
    let Some(s) = option_value(rest, "--marked").map(|s| s.to_lowercase()) else {
        return Ok(None);
    };
    match s.as_str() {
        "oui" | "yes" | "1" | "true" => Ok(Some(true)),
        "non" | "no" | "0" | "false" => Ok(Some(false)),
        _ => Err(acrux_core::Error::Corrupt(format!(
            "--marked : « oui » ou « non » attendu, reçu « {s} »"
        ))),
    }
}

/// Ce que `annot-set` change à l'annotation, d'après ses options ; `current`
/// est son rectangle actuel, d'où part `--move`.
fn annot_changes_arg(
    rest: &[String],
    current: acrux_core::Rect,
) -> acrux_core::Result<acrux_features::annotations::AnnotChanges> {
    let mut changes = acrux_features::annotations::AnnotChanges::default();
    if let Some(spec) = option_value(rest, "--rect") {
        changes.rect = Some(parse_rect(spec)?);
    }
    if let Some(spec) = option_value(rest, "--move") {
        let d: Vec<f64> = spec
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
            .collect();
        let [dx, dy] = d[..] else {
            return Err(acrux_core::Error::Corrupt(format!(
                "--move : « dx,dy » attendu, reçu « {spec} »"
            )));
        };
        let r = changes.rect.unwrap_or(current);
        changes.rect = Some(acrux_core::Rect::new(
            r.x0 + dx,
            r.y0 + dy,
            r.x1 + dx,
            r.y1 + dy,
        ));
    }
    if let Some(spec) = option_value(rest, "--color") {
        changes.color = Some(color_arg(spec)?);
    }
    if let Some(spec) = option_value(rest, "--fill") {
        changes.fill = Some(if matches!(spec.as_str(), "none" | "aucun" | "aucune") {
            None
        } else {
            Some(color_arg(spec)?)
        });
    }
    changes.opacity = number_option(rest, "--opacity")?;
    changes.width = number_option(rest, "--width")?;
    changes.contents = option_value(rest, "--text").map(|t| cli_text(t));
    Ok(changes)
}

fn cmd_annot_set(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::annotations::review::{set_state, StateChange};
    use acrux_features::annotations::{set_annotation_properties, AnnotMeta};
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let pos = positional(rest);
    let (pages, page, index) = annotation_target(&doc, &pos, ANNOT_SET_USAGE)?;
    let current = acrux_features::annotations::list_annotations(&doc, &pages[page])?
        .get(index)
        .map_or_else(acrux_core::Rect::default, |a| a.rect);
    let changes = annot_changes_arg(rest, current)?;
    let state = review_state_arg(rest)?;
    let marked = marked_arg(rest)?;
    if changes.is_empty() && state.is_none() && marked.is_none() {
        return Err(acrux_core::Error::Corrupt(format!(
            "rien à changer\n{ANNOT_SET_USAGE}"
        )));
    }
    let author = option_value(rest, "--author").map_or("acr", String::as_str);
    if !changes.is_empty() {
        set_annotation_properties(&doc, &pages[page], index, &changes)?;
        println!("annotation #{} de la page {} modifiée", index + 1, page + 1);
    }
    let mut states = Vec::new();
    if let Some(s) = state {
        states.push((StateChange::Review(s), s.label()));
    }
    if let Some(m) = marked {
        states.push((
            StateChange::Marked(m),
            if m { "case cochée" } else { "case décochée" },
        ));
    }
    for (change, label) in states {
        let pages = collect_pages(&doc)?;
        set_state(
            &doc,
            &pages[page],
            index,
            change,
            &AnnotMeta::fresh(Some(author)),
        )?;
        println!("statut : {label}");
    }
    save(&doc, &out, rest)
}

fn cmd_annot_remove(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let usage = "usage : annot-remove <fichier> <page> <n°> -o <sortie>";
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let pos = positional(rest);
    let (pages, page, index) = annotation_target(&doc, &pos, usage)?;
    let list = acrux_features::annotations::list_annotations(&doc, &pages[page])?;
    // Un élément de « remplir et signer » se retire par `fillsign`, un champ
    // par les commandes des formulaires : on ne supprime jamais l'annotation
    // d'un autre module par une erreur de numéro.
    if let Some(a) = list
        .get(index)
        .filter(|a| a.fill_sign || a.subtype == "Widget")
    {
        return Err(acrux_core::Error::Corrupt(format!(
            "l'annotation #{} est {} : elle se retire avec sa propre commande",
            index + 1,
            if a.fill_sign {
                "un élément de « remplir et signer »"
            } else {
                "un champ de formulaire"
            }
        )));
    }
    acrux_features::annotations::remove_annotation(&doc, &pages[page], index)?;
    let pages = collect_pages(&doc)?;
    let after = acrux_features::annotations::list_annotations(&doc, &pages[page])?.len();
    println!(
        "annotation #{} de la page {} retirée ({} annotation(s) en moins, réponses comprises)",
        index + 1,
        page + 1,
        list.len().saturating_sub(after)
    );
    save(&doc, &out, rest)
}

fn cmd_reply(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::annotations::AnnotMeta;
    let usage = "usage : reply <fichier> <page> <n°> <texte> [--author NOM] -o <sortie>";
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let pos = positional(rest);
    let (pages, page, index) = annotation_target(&doc, &pos, usage)?;
    let text = pos
        .get(2)
        .map(|t| cli_text(t))
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| acrux_core::Error::Corrupt(usage.into()))?;
    let author = option_value(rest, "--author").map_or("acr", String::as_str);
    acrux_features::annotations::review::add_reply(
        &doc,
        &pages[page],
        index,
        &text,
        &AnnotMeta::fresh(Some(author)),
    )?;
    println!(
        "réponse de {author} ajoutée à l'annotation #{} de la page {}",
        index + 1,
        page + 1
    );
    save(&doc, &out, rest)
}

/// Identifiant d'annotation raccourci pour l'affichage. Ceux d'Acrux
/// (`acrux-…`, une trentaine de caractères) passent en entier : c'est leur
/// fin qui distingue les membres d'un groupe. Ceux d'autres logiciels
/// peuvent être bien plus longs, et s'arrêtent à quarante caractères.
fn short_id(name: &str) -> String {
    if name.chars().count() <= 40 {
        return name.to_string();
    }
    let head: String = name.chars().take(39).collect();
    format!("{head}…")
}

/// Usage de `annotate`, rappelé à chaque erreur de syntaxe.
const ANNOTATE_USAGE: &str = "usage : annotate <fichier> <page> <type> … -o <sortie>
  square|circle|highlight|underline|strikeout|squiggly|link <x0> <y0> <x1> <y1> [texte]
  caret|replace|note <x0> <y0> <x1> <y1> <texte>
  line|arrow <x1> <y1> <x2> <y2> [texte] [--head open|closed|circle|square|butt|none] [--tail …]
  polygon|polyline <x> <y> <x> <y> <x> <y> … [texte]
  ink --points \"x,y x,y …;x,y …\" [texte]
  text <x0> <y0> <x1> <y1> <texte> [--font helvetica|times|courier[-bold]] [--size 12] [--align left|center|right]
  callout <x0> <y0> <x1> <y1> <ax> <ay> <texte>
  formes : --color r,g,b --fill r,g,b --width <pt> --opacity 0..1 ; texte : --color, --fill, --border r,g,b";

fn annotate_error(message: &str) -> acrux_core::Error {
    acrux_core::Error::Corrupt(if message.is_empty() {
        ANNOTATE_USAGE.to_string()
    } else {
        format!("{message}\n{ANNOTATE_USAGE}")
    })
}

/// Une valeur d'option lue en nombre, ou l'erreur qui dit laquelle.
fn number_option(rest: &[String], name: &str) -> acrux_core::Result<Option<f64>> {
    option_value(rest, name)
        .map(|v| {
            v.trim()
                .replace(',', ".")
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| annotate_error(&format!("{name} : nombre attendu, reçu « {v} »")))
        })
        .transpose()
}

/// Terminaison de ligne nommée en ligne de commande.
fn line_ending_arg(
    rest: &[String],
    name: &str,
    default: acrux_features::annotations::LineEnding,
) -> acrux_core::Result<acrux_features::annotations::LineEnding> {
    use acrux_features::annotations::LineEnding;
    let Some(v) = option_value(rest, name) else {
        return Ok(default);
    };
    Ok(match v.to_ascii_lowercase().as_str() {
        "open" | "ouverte" => LineEnding::OpenArrow,
        "closed" | "fermee" | "fermée" => LineEnding::ClosedArrow,
        "circle" | "rond" => LineEnding::Circle,
        "square" | "carre" | "carré" => LineEnding::Square,
        "butt" | "butee" | "butée" => LineEnding::Butt,
        "none" | "aucune" => LineEnding::None,
        _ => {
            return Err(annotate_error(&format!(
                "{name} : terminaison inconnue « {v} »"
            )))
        }
    })
}

/// Police standard nommée en ligne de commande : `helvetica`, `times`,
/// `courier`, avec `-bold`, `-italic` ou `-bolditalic`.
fn standard_font_arg(rest: &[String]) -> acrux_core::Result<acrux_features::stamp::StandardFont> {
    use acrux_features::stamp::StandardFont;
    let Some(v) = option_value(rest, "--font") else {
        return Ok(StandardFont::Helvetica);
    };
    let name = v.to_ascii_lowercase().replace(['_', ' '], "-");
    let (family, style) = name.split_once('-').unwrap_or((name.as_str(), ""));
    let bold = style.contains("bold") || style.contains("gras");
    let italic = style.contains("italic") || style.contains("oblique") || style.contains("ital");
    Ok(match (family, bold, italic) {
        ("helvetica" | "arial" | "sans", false, false) => StandardFont::Helvetica,
        ("helvetica" | "arial" | "sans", true, false) => StandardFont::HelveticaBold,
        ("helvetica" | "arial" | "sans", false, true) => StandardFont::HelveticaOblique,
        ("helvetica" | "arial" | "sans", true, true) => StandardFont::HelveticaBoldOblique,
        ("times" | "serif", false, false) => StandardFont::TimesRoman,
        ("times" | "serif", true, false) => StandardFont::TimesBold,
        ("times" | "serif", false, true) => StandardFont::TimesItalic,
        ("times" | "serif", true, true) => StandardFont::TimesBoldItalic,
        ("courier" | "mono", false, false) => StandardFont::Courier,
        ("courier" | "mono", true, false) => StandardFont::CourierBold,
        ("courier" | "mono", false, true) => StandardFont::CourierOblique,
        ("courier" | "mono", true, true) => StandardFont::CourierBoldOblique,
        _ => return Err(annotate_error(&format!("--font : police inconnue « {v} »"))),
    })
}

/// Traits d'encre : `x,y x,y …`, un « ; » entre deux traits.
fn ink_points_arg(spec: &str) -> acrux_core::Result<Vec<Vec<acrux_core::Point>>> {
    let mut strokes = Vec::new();
    for part in spec.split(';') {
        let mut stroke = Vec::new();
        for pair in part.split_whitespace() {
            let (x, y) = pair
                .split_once(',')
                .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
                .filter(|(x, y): &(f64, f64)| x.is_finite() && y.is_finite())
                .ok_or_else(|| {
                    annotate_error(&format!("--points : « {pair} » n'est pas « x,y »"))
                })?;
            stroke.push(acrux_core::Point::new(x, y));
        }
        if !stroke.is_empty() {
            strokes.push(stroke);
        }
    }
    if strokes.is_empty() {
        return Err(annotate_error("--points : aucun point"));
    }
    Ok(strokes)
}

/// Texte donné en ligne de commande : « \n » écrit tel quel y est un saut de
/// ligne, faute de pouvoir en taper un dans la plupart des consoles.
fn cli_text(raw: &str) -> String {
    raw.replace("\\n", "\n")
}

#[allow(clippy::too_many_lines)] // une branche par type d'annotation
fn cmd_annotate(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_core::Point;
    use acrux_features::annotations::{
        Callout, LineEnding, MarkupKind, NewAnnotation, ShapeStyle, TextAlign, CARET_COLOR,
    };
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let (Some(page_spec), Some(kind)) = (pos.first(), pos.get(1)) else {
        return Err(annotate_error(""));
    };
    let kind = kind.to_ascii_lowercase();
    let args: Vec<&str> = pos[2..].iter().map(|s| s.as_str()).collect();
    // Les nombres en tête, puis, s'il y en a, le texte.
    let numbers: Vec<f64> = args
        .iter()
        .map_while(|a| a.parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect();
    let text = args.get(numbers.len()).map(|t| cli_text(t));
    let need = |n: usize| -> acrux_core::Result<()> {
        if numbers.len() < n {
            Err(annotate_error(&format!(
                "{kind} : {n} coordonnées attendues, {} reçues",
                numbers.len()
            )))
        } else {
            Ok(())
        }
    };
    // Insérer, remplacer ou écrire sans dire quoi n'a pas de sens.
    let required = || {
        text.clone()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| annotate_error(&format!("{kind} : texte obligatoire")))
    };
    let color = option_value(rest, "--color")
        .map(|c| parse_rgb(c))
        .transpose()?;
    let fill = option_value(rest, "--fill")
        .map(|c| parse_rgb(c))
        .transpose()?;
    let style = ShapeStyle {
        stroke: Some(color.unwrap_or(acrux_features::annotations::SHAPE_COLOR)),
        fill,
        width: number_option(rest, "--width")?.unwrap_or(2.0),
        opacity: number_option(rest, "--opacity")?.unwrap_or(1.0),
    };
    let (doc, _) = open(path)?;
    let all_pages = collect_pages(&doc)?;
    let page_index = acrux_features::pages::parse_page_spec(page_spec, all_pages.len())?[0];
    let zone = || -> acrux_core::Result<acrux_core::Rect> {
        need(4)?;
        Ok(acrux_core::Rect::new(
            numbers[0], numbers[1], numbers[2], numbers[3],
        ))
    };
    let markup = |kind: MarkupKind| -> acrux_core::Result<NewAnnotation> {
        Ok(NewAnnotation::Markup {
            kind,
            quads: vec![zone()?],
            color: color.unwrap_or_else(|| kind.default_color()),
            contents: text.clone(),
        })
    };
    let points = || -> acrux_core::Result<Vec<Point>> {
        if numbers.len() % 2 != 0 {
            return Err(annotate_error(&format!(
                "{kind} : les coordonnées vont par paires x y"
            )));
        }
        Ok(numbers
            .chunks_exact(2)
            .map(|c| Point::new(c[0], c[1]))
            .collect())
    };
    let free_text = |rect: acrux_core::Rect,
                     callout: Option<Callout>|
     -> acrux_core::Result<NewAnnotation> {
        let body = required()?;
        let unsupported = acrux_features::annotations::freetext::unsupported_chars(&body);
        if !unsupported.is_empty() {
            let list: String = unsupported.iter().collect();
            eprintln!("attention : caractères non représentables dans une police standard, remplacés par « ? » : {list}");
        }
        let align = match option_value(rest, "--align").map(|a| a.to_ascii_lowercase()) {
            None => TextAlign::Left,
            Some(a) => match a.as_str() {
                "left" | "gauche" => TextAlign::Left,
                "center" | "centre" | "centré" => TextAlign::Center,
                "right" | "droite" => TextAlign::Right,
                _ => return Err(annotate_error(&format!("--align : « {a} » inconnu"))),
            },
        };
        let border_width = number_option(rest, "--width")?.unwrap_or(1.0);
        let border = option_value(rest, "--border")
            .map(|c| parse_rgb(c))
            .transpose()?
            .map(|c| (c, border_width))
            // Une légende a toujours un cadre : sans lui, la ligne ne
            // mènerait nulle part de visible.
            .or_else(|| callout.map(|_| (color.unwrap_or([0.0, 0.0, 0.0]), 1.0)));
        Ok(NewAnnotation::FreeText {
            rect,
            text: body,
            font: standard_font_arg(rest)?,
            size: number_option(rest, "--size")?.unwrap_or(12.0),
            color: color.unwrap_or([0.0, 0.0, 0.0]),
            border,
            fill,
            align,
            callout,
            // Le texte s'écrit droit pour qui lit la page tournée, comme
            // dans l'application.
            rotation: all_pages[page_index].rotate(&doc),
        })
    };
    let annot = match kind.as_str() {
        "square" | "rectangle" => NewAnnotation::Square {
            rect: zone()?,
            style,
            contents: text.clone(),
        },
        "circle" | "ellipse" => NewAnnotation::Circle {
            rect: zone()?,
            style,
            contents: text.clone(),
        },
        "line" | "arrow" => {
            need(4)?;
            let head = if kind == "arrow" {
                LineEnding::OpenArrow
            } else {
                LineEnding::None
            };
            NewAnnotation::Line {
                from: Point::new(numbers[0], numbers[1]),
                to: Point::new(numbers[2], numbers[3]),
                style,
                start: line_ending_arg(rest, "--tail", LineEnding::None)?,
                end: line_ending_arg(rest, "--head", head)?,
                contents: text.clone(),
            }
        }
        "polygon" => {
            need(6)?;
            NewAnnotation::Polygon {
                points: points()?,
                style,
                contents: text.clone(),
            }
        }
        "polyline" => {
            need(4)?;
            NewAnnotation::PolyLine {
                points: points()?,
                style,
                start: line_ending_arg(rest, "--tail", LineEnding::None)?,
                end: line_ending_arg(rest, "--head", LineEnding::None)?,
                contents: text.clone(),
            }
        }
        "ink" => {
            let spec = option_value(rest, "--points")
                .ok_or_else(|| annotate_error("ink : --points obligatoire"))?;
            NewAnnotation::Ink {
                strokes: ink_points_arg(spec)?,
                style,
                // Pour l'encre, pas de coordonnées : le texte est le premier
                // argument libre.
                contents: args.first().map(|t| cli_text(t)),
            }
        }
        "text" | "freetext" => free_text(zone()?, None)?,
        "callout" => {
            need(6)?;
            let call = Callout {
                anchor: Point::new(numbers[4], numbers[5]),
                knee: None,
                ending: line_ending_arg(rest, "--head", LineEnding::OpenArrow)?,
            };
            free_text(zone()?, Some(call))?
        }
        "highlight" => markup(MarkupKind::Highlight)?,
        "underline" => markup(MarkupKind::Underline)?,
        "strikeout" => markup(MarkupKind::StrikeOut)?,
        "squiggly" => markup(MarkupKind::Squiggly)?,
        "caret" => {
            let zone = zone()?;
            NewAnnotation::Caret {
                x: zone.x0,
                line: zone,
                contents: required()?,
                color: color.unwrap_or(CARET_COLOR),
            }
        }
        "replace" => NewAnnotation::Replace {
            quads: vec![zone()?],
            text: required()?,
            strike: MarkupKind::StrikeOut.default_color(),
            caret: CARET_COLOR,
        },
        "note" => {
            need(2)?;
            NewAnnotation::Note {
                x: numbers[0],
                y: numbers.get(3).copied().unwrap_or(numbers[1]),
                contents: text.clone().unwrap_or_default(),
                color: color.unwrap_or([1.0, 0.8, 0.0]),
            }
        }
        "link" => NewAnnotation::Link {
            rect: zone()?,
            uri: text.clone().unwrap_or_default(),
        },
        _ => return Err(annotate_error(&format!("type « {kind} » inconnu"))),
    };
    acrux_features::annotations::add_annotation(&doc, &all_pages[page_index], &annot, Some("acr"))?;
    save(&doc, &out, rest)
}

fn cmd_attachments(path: &str) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let list = acrux_features::attach::list_attachments(&doc)?;
    for a in &list {
        println!(
            "{:<32} {:>10}  {}{}{}{}",
            a.name,
            a.size.map_or("?".to_string(), |s| format!("{s} o")),
            a.mime.as_deref().unwrap_or("type inconnu"),
            a.page
                .map_or(String::new(), |p| format!("  page {}", p + 1)),
            a.modified.as_ref().map_or(String::new(), |d| format!(
                "  {}",
                acrux_features::annotations::readable_date(d)
            )),
            a.description
                .as_ref()
                .map_or(String::new(), |d| format!("  « {d} »")),
        );
    }
    println!("{} pièce(s) jointe(s)", list.len());
    Ok(())
}

fn cmd_attach(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(source) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : attach <fichier> <fichier-à-joindre> [--description <texte>] [--page N] -o <sortie>".into(),
        ));
    };
    let data = std::fs::read(source.as_str())?;
    let name = std::path::Path::new(source.as_str())
        .file_name()
        .map_or_else(|| (*source).clone(), |n| n.to_string_lossy().into_owned());
    let (doc, _) = open(path)?;
    let page =
        match option_value(rest, "--page") {
            Some(spec) => Some(pages_arg(&doc, spec)?.first().copied().ok_or_else(|| {
                acrux_core::Error::Corrupt(format!("page « {spec} » inexistante"))
            })?),
            None => None,
        };
    let options = acrux_features::attach::AttachOptions {
        description: option_value(rest, "--description").cloned(),
        page,
        ..acrux_features::attach::AttachOptions::default()
    };
    acrux_features::attach::add_attachment(&doc, &name, &data, &options)?;
    println!("joint : {name} ({} octets)", data.len());
    save(&doc, &out, rest)
}

fn cmd_detach(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(name) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : detach <fichier> <nom> -o <fichier-extrait>".into(),
        ));
    };
    let (doc, _) = open(path)?;
    let list = acrux_features::attach::list_attachments(&doc)?;
    let found = list.iter().find(|a| a.name == **name).ok_or_else(|| {
        acrux_core::Error::Corrupt(format!("pièce jointe « {name} » introuvable"))
    })?;
    let data = acrux_features::attach::read_attachment(&doc, found)?;
    std::fs::write(&out, &data)?;
    println!("écrit : {out} ({} octets)", data.len());
    Ok(())
}

fn cmd_labels(path: &str) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let labels = acrux_features::pagelabels::read_page_labels(&doc)?;
    let ranges = acrux_features::pagelabels::read_label_ranges(&doc)?;
    if ranges.is_empty() {
        println!("aucun /PageLabels : numérotation décimale implicite");
    }
    for (index, label) in labels.iter().enumerate() {
        println!("page {:>4}  {label}", index + 1);
    }
    Ok(())
}

/// Une plage « page:style[:préfixe][:début] » (page comptée à partir de 1).
fn parse_label_range(spec: &str) -> acrux_core::Result<acrux_features::pagelabels::LabelRange> {
    use acrux_features::pagelabels::{LabelRange, LabelStyle};
    let fields: Vec<&str> = spec.split(':').collect();
    let page: usize = fields
        .first()
        .and_then(|f| f.trim().parse().ok())
        .filter(|p| *p >= 1)
        .ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "plage « {spec} » : numéro de page attendu en premier (1 = première page)"
            ))
        })?;
    let style = match fields.get(1).map_or("", |s| s.trim()) {
        "" => None,
        // « i » et « I » ne sont pas des codes PDF, mais c'est ainsi qu'on
        // écrit des chiffres romains : on les accepte comme synonymes.
        "D" | "1" => Some(LabelStyle::Decimal),
        "r" | "i" => Some(LabelStyle::LowerRoman),
        "R" | "I" => Some(LabelStyle::UpperRoman),
        "a" => Some(LabelStyle::LowerLetters),
        "A" => Some(LabelStyle::UpperLetters),
        other => {
            return Err(acrux_core::Error::Corrupt(format!(
                "plage « {spec} » : style « {other} » inconnu (D, r, R, a, A ou vide)"
            )))
        }
    };
    // Troisième champ : un nombre est un numéro de départ, le reste un préfixe.
    let third = fields.get(2).copied().unwrap_or("");
    let (prefix, start) = match third.parse::<u32>() {
        Ok(n) if n >= 1 && fields.len() == 3 => (String::new(), n),
        _ => (
            third.to_string(),
            fields
                .get(3)
                .and_then(|f| f.trim().parse().ok())
                .filter(|n| *n >= 1)
                .unwrap_or(1),
        ),
    };
    Ok(LabelRange {
        first_page: page - 1,
        style,
        prefix,
        start,
    })
}

fn cmd_set_labels(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(spec) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : set-labels <fichier> <plages> -o <sortie> (exemple : 1:i,5:D:1)".into(),
        ));
    };
    let ranges: Vec<_> = spec
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(parse_label_range)
        .collect::<acrux_core::Result<_>>()?;
    let (doc, _) = open(path)?;
    acrux_features::pagelabels::set_page_labels(&doc, &ranges)?;
    let labels = acrux_features::pagelabels::read_page_labels(&doc)?;
    println!(
        "{} plage(s) ; première page « {} », dernière « {} »",
        ranges.len(),
        labels.first().map_or("-", String::as_str),
        labels.last().map_or("-", String::as_str)
    );
    save(&doc, &out, rest)
}

/// « x0,y0,x1,y1 » en points.
fn parse_rect(spec: &str) -> acrux_core::Result<acrux_core::Rect> {
    let v: Vec<f64> = spec
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    if v.len() != 4 {
        return Err(acrux_core::Error::Corrupt(format!(
            "rectangle « x0,y0,x1,y1 » attendu, reçu « {spec} »"
        )));
    }
    Ok(acrux_core::Rect::new(v[0], v[1], v[2], v[3]))
}

/// « r,g,b » avec des composantes de 0 à 1.
fn parse_rgb(spec: &str) -> acrux_core::Result<[f64; 3]> {
    let v: Vec<f64> = spec
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    if v.len() != 3 {
        return Err(acrux_core::Error::Corrupt(format!(
            "couleur « r,g,b » (0 à 1) attendue, reçue « {spec} »"
        )));
    }
    Ok([
        v[0].clamp(0.0, 1.0),
        v[1].clamp(0.0, 1.0),
        v[2].clamp(0.0, 1.0),
    ])
}

/// Enregistrement d'une biffure ou d'un nettoyage : toujours une réécriture
/// complète avec purge des objets inatteignables, sinon les anciennes
/// révisions laisseraient le contenu retiré dans le fichier.
fn save_sanitized(doc: &Document, out: &str) -> acrux_core::Result<()> {
    let bytes = doc.save_full_with(&acrux_document::SaveOptions {
        compress_streams: true,
        object_streams: false,
        drop_unreferenced: true,
        ..acrux_document::SaveOptions::default()
    })?;
    std::fs::write(out, &bytes)?;
    println!(
        "écrit : {out} ({} octets, réécriture complète)",
        bytes.len()
    );
    Ok(())
}

fn cmd_redact(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::redact::{
        apply_redactions, find_redaction_candidates, mark_redactions, Pattern, RedactionMark,
    };
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let overlay = option_value(rest, "--text").cloned();
    let fill = match option_value(rest, "--color") {
        Some(c) => Some(parse_rgb(c)?),
        None => None,
    };
    let all_pages = rest.iter().any(|a| a == "--all-pages");
    let page = match option_value(rest, "--page") {
        Some(spec) => *pages_arg(&doc, spec)?
            .first()
            .ok_or_else(|| acrux_core::Error::Corrupt(format!("page « {spec} » inexistante")))?,
        None => 0,
    };
    let mut marks: Vec<RedactionMark> = Vec::new();
    if let Some(spec) = option_value(rest, "--rect") {
        marks.push(RedactionMark::new(page, parse_rect(spec)?));
    }
    let mut patterns: Vec<Pattern> = Vec::new();
    if let Some(needle) = option_value(rest, "--find") {
        patterns.push(Pattern::Literal(needle.clone()));
    }
    if let Some(list) = option_value(rest, "--pattern") {
        for name in list.split(',').filter(|n| !n.trim().is_empty()) {
            patterns.push(Pattern::parse(name).ok_or_else(|| {
                acrux_core::Error::Corrupt(format!(
                    "motif « {name} » inconnu : email, telephone, iban, carte, \
                     securite-sociale, date, ip"
                ))
            })?);
        }
    }
    if !patterns.is_empty() {
        let mut found = find_redaction_candidates(&doc, &patterns)?;
        if !all_pages {
            found.retain(|m| m.page == page);
        }
        println!("{} occurrence(s) trouvée(s)", found.len());
        marks.extend(found);
    }
    if marks.is_empty() {
        return Err(acrux_core::Error::Corrupt(
            "rien à biffer : indiquer --rect, --find ou --pattern".into(),
        ));
    }
    for m in &mut marks {
        m.fill = fill;
        m.overlay_text.clone_from(&overlay);
    }
    mark_redactions(&doc, &marks)?;
    if rest.iter().any(|a| a == "--mark-only") {
        println!("{} marque(s) /Redact posée(s), non appliquées", marks.len());
        return save_sanitized(&doc, &out);
    }
    let report = apply_redactions(&doc)?;
    println!("zones biffées      : {}", report.zones);
    println!("glyphes retirés    : {}", report.glyphs_removed);
    println!("images modifiées   : {}", report.images_modified);
    println!("annotations retirées : {}", report.annotations_removed);
    for w in &report.warnings {
        eprintln!("attention : {w}");
    }
    save_sanitized(&doc, &out)
}

fn cmd_sanitize(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::redact::{sanitize, SanitizeOptions};
    let out = output_arg(rest)?;
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let all = has("--all");
    let options = SanitizeOptions {
        metadata: all || has("--metadata"),
        attachments: all || has("--attachments"),
        javascript: all || has("--javascript"),
        hidden_layers: all || has("--layers"),
        comments: all || has("--comments"),
        forms: all || has("--forms"),
        invisible_text: all || has("--invisible-text"),
        unreachable: all || has("--gc"),
        // La réécriture est toujours complète : les révisions antérieures
        // disparaissent quoi qu'il arrive.
        previous_revisions: true,
    };
    let nothing_chosen = SanitizeOptions {
        previous_revisions: false,
        ..options
    }
    .is_empty();
    if nothing_chosen {
        return Err(acrux_core::Error::Corrupt(
            "rien à nettoyer : choisir --metadata, --attachments, --javascript, --layers, \
             --comments, --forms, --invisible-text, --gc, ou --all"
                .into(),
        ));
    }
    let (doc, _) = open(path)?;
    let report = sanitize(&doc, &options)?;
    if report.removed.is_empty() {
        println!("rien à retirer");
    }
    for item in &report.removed {
        println!("retiré : {item}");
    }
    for w in &report.warnings {
        eprintln!("attention : {w}");
    }
    save_sanitized(&doc, &out)
}

fn cmd_edit_text(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let usage = "usage : edit-text <fichier> --find <texte> --replace <texte> [--page N] [--all] [--size N] [--color r,g,b] [--font <nom>] [--bold] [--italic] [--case] [--word] -o <sortie>";
    let (Some(needle), Some(replacement)) = (
        option_value(rest, "--find"),
        option_value(rest, "--replace"),
    ) else {
        return Err(acrux_core::Error::Corrupt(usage.into()));
    };
    let style = acrux_features::edit_text::StyleOverride {
        font: option_value(rest, "--font").cloned(),
        size: option_value(rest, "--size").and_then(|v| v.parse().ok()),
        color: match option_value(rest, "--color") {
            Some(spec) => Some(parse_rgb(spec)?),
            None => None,
        },
        bold: rest.iter().any(|a| a == "--bold").then_some(true),
        italic: rest.iter().any(|a| a == "--italic").then_some(true),
    };
    let all = rest.iter().any(|a| a == "--all");
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let indices = match option_value(rest, "--page") {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?,
        None => (0..pages.len()).collect(),
    };
    let mut edits = Vec::new();
    for index in indices {
        let text = acrux_features::text::extract_page_text(&doc, &pages[index])?;
        let mut ranges =
            acrux_features::edit_text::find_ranges_with(&text, needle, search_options(rest));
        if !all {
            ranges.truncate(1);
        }
        for target in ranges {
            edits.push(acrux_features::edit_text::TextEdit {
                page: index,
                target,
                new_text: replacement.clone(),
                style: (!style.is_empty()).then(|| style.clone()),
            });
        }
        if !all && !edits.is_empty() {
            break;
        }
    }
    if edits.is_empty() {
        return Err(acrux_core::Error::Corrupt(format!(
            "texte « {needle} » introuvable"
        )));
    }
    let report = acrux_features::edit_text::apply_edits_reporting(&doc, &edits)?;
    println!(
        "{} remplacement(s) : « {needle} » → « {replacement} »",
        report.edits
    );
    for w in &report.warnings {
        eprintln!("avertissement : {w}");
    }
    save(&doc, &out, rest)
}

fn cmd_reflow(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let usage = "usage : reflow <fichier> --page N --paragraph K --text <texte> [--shrink] [--min-size N] -o <sortie>";
    let Some(new_text) = option_value(rest, "--text") else {
        return Err(acrux_core::Error::Corrupt(usage.into()));
    };
    let paragraph: usize = option_value(rest, "--paragraph")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| acrux_core::Error::Corrupt(usage.into()))?;
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let index = match option_value(rest, "--page") {
        Some(spec) => acrux_features::pages::parse_page_spec(spec, pages.len())?
            .first()
            .copied()
            .unwrap_or(0),
        None => 0,
    };
    let options = acrux_features::edit_text::ReflowOptions {
        keep_font_size: !rest.iter().any(|a| a == "--shrink"),
        min_size: option_value(rest, "--min-size")
            .and_then(|v| v.parse().ok())
            .unwrap_or(6.0),
    };
    acrux_features::edit_text::reflow_paragraph(
        &doc,
        &pages[index],
        paragraph,
        new_text,
        &options,
    )?;
    println!(
        "paragraphe {paragraph} de la page {} recomposé ({} caractères)",
        index + 1,
        new_text.chars().count()
    );
    save(&doc, &out, rest)
}

fn cmd_fields(path: &str) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let fields = acrux_features::forms::list_fields(&doc)?;
    if fields.is_empty() {
        println!("aucun champ de formulaire (pas d'AcroForm)");
        return Ok(());
    }
    for f in &fields {
        // Un saut de ligne (champ multiligne) s'écrit « \n » : une valeur
        // tient sur la ligne de son champ, et la sortie reste lisible par un
        // script.
        let value = f.value.as_ref().map_or_else(
            || "-".to_string(),
            |v| {
                let shown = v
                    .to_display()
                    .replace('\\', "\\\\")
                    .replace("\r\n", "\\n")
                    .replace(['\r', '\n'], "\\n");
                format!("« {shown} »")
            },
        );
        let mut extra: Vec<String> = f.flags.labels().iter().map(ToString::to_string).collect();
        if let Some(n) = f.max_len {
            extra.push(format!("max {n}"));
        }
        if let Some(tu) = &f.alternate_name {
            extra.push(format!("info-bulle « {tu} »"));
        }
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!("  ({})", extra.join(", "))
        };
        println!("{:<28} {:<10} {value}{extra}", f.name, f.kind.label());
        if !f.options.is_empty() {
            let opts: Vec<String> = f
                .options
                .iter()
                .map(|o| {
                    if o.export == o.display {
                        o.export.clone()
                    } else {
                        format!("{} → {}", o.export, o.display)
                    }
                })
                .collect();
            println!("{:<40}options : {}", "", opts.join(", "));
        }
        for w in &f.widgets {
            let state = match (&w.state, &w.on_state) {
                (Some(s), Some(on)) => format!("  état {s} (coché : {on})"),
                (None, Some(on)) => format!("  (coché : {on})"),
                _ => String::new(),
            };
            println!(
                "{:<40}page {} [{:.0} {:.0} {:.0} {:.0}]{state}",
                "",
                w.page
                    .map_or_else(|| "?".to_string(), |p| (p + 1).to_string()),
                w.rect.x0,
                w.rect.y0,
                w.rect.x1,
                w.rect.y1
            );
        }
    }
    println!("{} champ(s)", fields.len());
    Ok(())
}

fn cmd_fill(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let assignments = positional(rest);
    let reset_first = rest.iter().any(|a| a == "--reset");
    if assignments.is_empty() && !reset_first {
        return Err(acrux_core::Error::Corrupt(
            "usage : fill <fichier> [<nom>=<valeur>...] [--reset] [--flatten] -o <sortie>".into(),
        ));
    }
    let (doc, _) = open(path)?;
    if reset_first {
        // Le formulaire repart de ses valeurs par défaut avant les
        // affectations : « effacer le formulaire » de l'application.
        let n = acrux_features::forms::reset_fields(&doc)?;
        println!("{n} champ(s) remis à leur valeur par défaut");
    }
    let fields = acrux_features::forms::list_fields(&doc)?;
    for a in assignments {
        let Some((name, value)) = a.split_once('=') else {
            return Err(acrux_core::Error::Corrupt(format!(
                "affectation « nom=valeur » attendue, reçu « {a} »"
            )));
        };
        let suffix = format!(".{name}");
        let kind = fields
            .iter()
            .find(|f| f.name == name || f.name.ends_with(&suffix))
            .map_or(acrux_features::forms::FieldType::Text, |f| f.kind);
        let parsed = acrux_features::forms::FieldValue::parse(kind, value);
        acrux_features::forms::set_field_value(&doc, name, parsed)?;
        println!("{name} = {value}");
    }
    if rest.iter().any(|a| a == "--flatten") {
        let n = acrux_features::forms::flatten_fields(&doc)?;
        println!("{n} widget(s) aplati(s), formulaire retiré");
    }
    save(&doc, &out, rest)
}

fn cmd_fdf_export(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let bytes = acrux_features::forms::export_fdf(&doc);
    std::fs::write(&out, &bytes)?;
    println!("écrit : {out} ({} octets)", bytes.len());
    Ok(())
}

fn cmd_fdf_import(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(fdf_path) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : fdf-import <fichier> <données.fdf> -o <sortie>".into(),
        ));
    };
    let data = std::fs::read(fdf_path)?;
    let (doc, _) = open(path)?;
    let n = acrux_features::forms::import_fdf(&doc, &data)?;
    println!("{n} champ(s) mis à jour");
    save(&doc, &out, rest)
}

// ---------------------------------------------------------------------------
// Accessibilité et contrôle en amont (phase 7)
// ---------------------------------------------------------------------------

fn cmd_check_a11y(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::accessibility::{check_accessibility, Severity};
    let (doc, _) = open(path)?;
    let report = check_accessibility(&doc)?;
    if rest.iter().any(|a| a == "--json") {
        print!("{}", report.to_json());
        return Ok(());
    }
    println!(
        "{path} : {} page(s), document {}",
        report.pages,
        if report.tagged {
            "balisé"
        } else {
            "NON balisé"
        }
    );
    if report.issues.is_empty() {
        println!("aucun problème d'accessibilité détecté");
        return Ok(());
    }
    for issue in &report.issues {
        let place = match issue.page {
            Some(p) => format!("page {}", p + 1),
            None => "document".to_string(),
        };
        let element = issue
            .element
            .as_ref()
            .map_or_else(String::new, |e| format!(" [{e}]"));
        println!(
            "{:>14}  {place:<10} {:<18} {}{element}",
            issue.severity.label(),
            issue.rule,
            issue.message
        );
    }
    println!();
    println!("{}", report.summary());
    if report.count(Severity::Error) > 0 {
        println!("« acr autotag » corrige le balisage manquant ; le texte de remplacement");
        println!("des figures et la portée des en-têtes de tableau restent à saisir à la main.");
    }
    Ok(())
}

fn cmd_autotag(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::accessibility::{autotag, check_accessibility};
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let created = autotag(&doc)?;
    println!("{created} élément(s) de structure créé(s)");
    let report = check_accessibility(&doc)?;
    println!("après balisage : {}", report.summary());
    // Le balisage réécrit les flux de contenu : une réécriture complète évite
    // de laisser les anciens flux dans le fichier.
    let bytes = doc.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!("écrit : {out} ({} octets)", bytes.len());
    Ok(())
}

fn cmd_preflight(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::accessibility::Severity;
    use acrux_features::preflight::{check_profile, fix, FixOptions, Profile};
    let name = option_value(rest, "--profile").ok_or_else(|| {
        acrux_core::Error::Corrupt(
            "profil manquant : --profile pdfa-1b|pdfa-2b|pdfa-3b|pdfx-1a|pdfx-4|pdfua-1".into(),
        )
    })?;
    let profile = Profile::parse(name)
        .ok_or_else(|| acrux_core::Error::Corrupt(format!("profil « {name} » inconnu")))?;
    let (doc, _) = open(path)?;
    let json = rest.iter().any(|a| a == "--json");

    if rest.iter().any(|a| a == "--fix") {
        let out = output_arg(rest)?;
        let options = FixOptions {
            title: option_value(rest, "--title").cloned(),
            ..FixOptions::default()
        };
        let report = fix(&doc, profile, &options)?;
        println!("{} : corrections appliquées", profile.label());
        if report.applied.is_empty() {
            println!("  (rien à corriger)");
        }
        for a in &report.applied {
            println!("  - {a}");
        }
        let errors = report
            .remaining
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count();
        if errors == 0 {
            println!("document conforme à {}", profile.label());
        } else {
            println!("{errors} problème(s) non réparable(s) :");
            for issue in report
                .remaining
                .iter()
                .filter(|i| i.severity == Severity::Error)
            {
                print_preflight_issue(issue);
            }
        }
        let bytes = doc.save_full()?;
        std::fs::write(&out, &bytes)?;
        println!("écrit : {out} ({} octets)", bytes.len());
        return Ok(());
    }

    let report = check_profile(&doc, profile)?;
    if json {
        print!("{}", report.to_json());
        return Ok(());
    }
    println!("{path} : {} page(s)", report.pages);
    for issue in &report.issues {
        print_preflight_issue(issue);
    }
    println!();
    println!("{}", report.summary());
    if report.is_conforming() {
        println!("document conforme à {}", profile.label());
    } else if report.issues.iter().any(|i| i.fixable) {
        println!("« --fix -o <sortie> » répare ce qui est marqué (réparable).");
    }
    Ok(())
}

fn print_preflight_issue(issue: &acrux_features::preflight::PreflightIssue) {
    let place = match issue.page {
        Some(p) => format!("page {}", p + 1),
        None => "document".to_string(),
    };
    println!(
        "{:>14}  {place:<10} {:<18} {}{}",
        issue.severity.label(),
        issue.rule,
        issue.message,
        if issue.fixable { " (réparable)" } else { "" }
    );
}

fn cmd_separations(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::preflight::{ink_coverage, separations};
    let out = output_arg(rest)?;
    let page: usize = option_value(rest, "--page")
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(1, |v| v.max(1))
        - 1;
    let dpi: f64 = option_value(rest, "--dpi")
        .and_then(|v| v.parse().ok())
        .unwrap_or(150.0);
    let (doc, _) = open(path)?;
    let plates = separations(&doc, page, dpi)?;
    let (stem, ext) = out.rsplit_once('.').unwrap_or((out.as_str(), "png"));
    for plate in &plates {
        // Une plaque se regarde comme un film : l'encre en noir sur blanc.
        let mut rgb = Vec::with_capacity(plate.coverage.len() * 3);
        for &c in &plate.coverage {
            let gray = 255 - c;
            rgb.extend_from_slice(&[gray, gray, gray]);
        }
        let png = acrux_graphics::encode_png_rgb(plate.width, plate.height, &rgb);
        let safe: String = plate
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let file = format!("{stem}-{}.{ext}", safe.to_lowercase());
        std::fs::write(&file, &png)?;
        println!(
            "{:<20} couverture moyenne {:>6.2} %{} → {file}",
            plate.name,
            plate.average(),
            if plate.is_blank() {
                "  (plaque vide)"
            } else {
                ""
            }
        );
    }
    let total = ink_coverage(&doc, page)?;
    println!("taux d'encre total maximal : {total:.0} %");
    if total > 300.0 {
        println!("attention : au-delà de 300 %, les encres ne sèchent pas sur papier couché");
    }
    Ok(())
}

fn cmd_compare(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::compare::{
        compare_documents, write_report, CompareOptions, ReportOptions, VisualOptions,
    };
    let usage =
        "usage : compare <avant.pdf> <apres.pdf> [--pages] [--visual] [--tolerance N] [-o rapport.pdf]";
    // Le second fichier est le premier argument qui n'est ni une option ni sa
    // valeur ; `--pages` est ici un drapeau, pas une liste de pages.
    let mut other = None;
    let mut skip = false;
    for a in rest {
        if skip {
            skip = false;
            continue;
        }
        if matches!(a.as_str(), "-o" | "--output" | "--tolerance") {
            skip = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        other = Some(a.clone());
        break;
    }
    let other = other.ok_or_else(|| acrux_core::Error::Corrupt(usage.into()))?;
    let tolerance = match option_value(rest, "--tolerance") {
        Some(v) => v.parse::<u8>().map_err(|_| {
            acrux_core::Error::Corrupt(format!("tolérance 0 à 255 attendue, reçue « {v} »"))
        })?,
        None => VisualOptions::default().tolerance,
    };
    let options = CompareOptions {
        visual: rest.iter().any(|a| a == "--visual").then(|| VisualOptions {
            tolerance,
            ..VisualOptions::default()
        }),
        ..CompareOptions::default()
    };
    let (old, _) = open(path)?;
    let (new, _) = open(&other)?;
    let comparison = compare_documents(&old, &new, &options)?;
    for w in &comparison.warnings {
        eprintln!("avertissement : {w}");
    }
    if rest.iter().any(|a| a == "-o" || a == "--output") {
        let out = output_arg(rest)?;
        let name = |p: &str| {
            std::path::Path::new(p)
                .file_name()
                .map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned())
        };
        let bytes = write_report(
            &old,
            &new,
            &comparison,
            &ReportOptions {
                old_title: name(path),
                new_title: name(&other),
                ..ReportOptions::default()
            },
        )?;
        std::fs::write(&out, &bytes)?;
        println!("écrit : {out} ({} octets)", bytes.len());
        return Ok(());
    }
    if rest.iter().any(|a| a == "--pages") {
        println!("avant  après  similarité  état");
        for page in &comparison.pages {
            let label =
                |i: Option<usize>| i.map_or_else(|| "-".to_string(), |v| (v + 1).to_string());
            let state = match (page.pair.old_page, page.pair.new_page, page.pair.moved) {
                (None, _, _) => "ajoutée",
                (_, None, _) => "supprimée",
                (_, _, true) => "déplacée",
                _ if page.is_identical() => "identique",
                _ => "modifiée",
            };
            println!(
                "{:>5}  {:>5}  {:>9.2}  {state}",
                label(page.pair.old_page),
                label(page.pair.new_page),
                page.pair.similarity
            );
        }
        return Ok(());
    }
    print!("{}", comparison.summary(5));
    Ok(())
}

// --- Signatures numériques (§12.8) -----------------------------------------

/// Charge les certificats racines d'un dossier : tout fichier y est lu comme
/// du DER, et un fichier peut en contenir plusieurs à la suite.
fn load_roots(
    directory: &str,
) -> acrux_core::Result<Vec<acrux_features::signature::x509::Certificate>> {
    let mut roots = Vec::new();
    let entries = std::fs::read_dir(directory)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let data = std::fs::read(&path)?;
        match acrux_features::signature::x509::parse_many(&data) {
            Ok(list) => roots.extend(list),
            Err(e) => eprintln!("racine ignorée {} : {e}", path.display()),
        }
    }
    if roots.is_empty() {
        eprintln!("aucun certificat racine lisible dans {directory}");
    }
    Ok(roots)
}

/// Échappe une chaîne pour du JSON.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_verdict(name: &str, verdict: &acrux_features::signature::Verdict) -> String {
    format!(
        "{}: {{ \"statut\": {}, \"detail\": {} }}",
        json_string(name),
        json_string(match verdict.status {
            acrux_features::signature::Status::Valid => "valide",
            acrux_features::signature::Status::Invalid => "invalide",
            acrux_features::signature::Status::Unknown => "inconnu",
        }),
        json_string(&verdict.detail)
    )
}

fn print_report_json(reports: &[acrux_features::signature::VerificationReport]) {
    println!("{{ \"signatures\": [");
    for (index, report) in reports.iter().enumerate() {
        let comma = if index + 1 == reports.len() { "" } else { "," };
        println!("  {{");
        println!("    \"champ\": {},", json_string(&report.field_name));
        println!(
            "    \"sous_filtre\": {},",
            json_string(report.sub_filter.name())
        );
        println!(
            "    \"signataire\": {},",
            report
                .signer
                .as_deref()
                .map_or_else(|| "null".to_string(), json_string)
        );
        println!(
            "    \"date_signee\": {},",
            report
                .signing_time
                .map_or_else(|| "null".to_string(), |t| json_string(&t.to_display()))
        );
        println!("    {},", json_verdict("condense", &report.digest));
        println!("    {},", json_verdict("signature", &report.signature));
        println!("    {},", json_verdict("chaine", &report.chain));
        println!("    {},", json_verdict("couverture", &report.coverage));
        println!(
            "    {},",
            json_verdict("modifications", &report.modifications)
        );
        println!("    \"valide\": {}", report.is_fully_valid());
        println!("  }}{comma}");
    }
    println!("] }}");
}

/// `acr media` : inventaire des vidéos et des sons, et extraction.
/// `acr 3d fichier.pdf [--extract dossier]` : les modèles 3D d'un document.
fn cmd_3d(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::three_d;
    let (doc, _) = open(path)?;
    let models = three_d::list(&doc)?;
    if models.is_empty() {
        println!("aucun modèle 3D");
        return Ok(());
    }
    // `--extract <dossier>` sort les modèles tels qu'ils sont dans le fichier.
    if let Some(dir) = option_value(rest, "--extract") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| acrux_core::Error::Io(format!("{} : {e}", dir.display())))?;
        for (i, model) in models.iter().enumerate() {
            let data = three_d::data(&doc, model)?;
            let name = format!("modele-{}.{}", i + 1, model.kind.label().to_lowercase());
            let out = dir.join(name);
            std::fs::write(&out, &data)
                .map_err(|e| acrux_core::Error::Io(format!("{} : {e}", out.display())))?;
            println!("{} ({} Kio)", out.display(), data.len() / 1024);
        }
        println!("{} modèle(s) extrait(s)", models.len());
        return Ok(());
    }
    println!("page  format  rectangle                      géométrie");
    for model in &models {
        let geometry = match three_d::scene(&doc, model) {
            Ok(scene) => {
                let points: usize = scene.items.iter().map(|i| i.mesh.positions.len()).sum();
                let faces: usize = scene.items.iter().map(|i| i.mesh.faces.len()).sum();
                format!(
                    "{} objet(s), {points} sommets, {faces} triangles",
                    scene.items.len()
                )
            }
            Err(e) => format!("{e}"),
        };
        println!(
            "{:>4}  {:<6}  {:<28}  {geometry}",
            model.page + 1,
            model.kind.label(),
            format!(
                "{:.0} {:.0} {:.0} {:.0}",
                model.rect.x0, model.rect.y0, model.rect.x1, model.rect.y1
            ),
        );
        if let Some(view) = &model.view {
            println!("      vue par défaut : « {view} »");
        }
    }
    Ok(())
}

fn cmd_media(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::media::{self, Source};
    let (doc, _) = open(path)?;
    let items = media::list(&doc)?;
    if items.is_empty() {
        println!("aucun média");
        return Ok(());
    }

    // `--extract <dossier>` sort les fichiers incorporés.
    if let Some(dir) = option_value(rest, "--extract") {
        let dir = std::path::PathBuf::from(dir);
        let mut sortis = 0usize;
        for item in &items {
            if !item.is_embedded() {
                println!(
                    "page {} : {} est hors du document, non extrait",
                    item.page + 1,
                    item.name.clone().unwrap_or_else(|| "le média".into())
                );
                continue;
            }
            let out = media::extract(&doc, item, &dir)?;
            let taille = std::fs::metadata(&out).map_or(0, |m| m.len());
            println!("{} ({} Kio)", out.display(), taille / 1024);
            sortis += 1;
        }
        println!("{sortis} média(s) extrait(s)");
        return Ok(());
    }

    println!("page  n°  forme            rectangle                      nom");
    for item in &items {
        let source = match &item.source {
            Some(Source::Embedded { size, .. }) => match size {
                Some(n) => format!("incorporé, {} Kio", n / 1024),
                None => "incorporé".into(),
            },
            Some(Source::Samples {
                rate,
                channels,
                bits,
                encoding,
                ..
            }) => format!(
                "échantillons bruts, {rate} Hz, {channels} voie(s), {bits} bits, {}",
                match encoding {
                    acrux_features::media::SoundEncoding::Raw => "non signé",
                    acrux_features::media::SoundEncoding::Signed => "signé",
                    acrux_features::media::SoundEncoding::MuLaw => "loi µ",
                    acrux_features::media::SoundEncoding::ALaw => "loi A",
                }
            ),
            Some(Source::External(name)) => format!("hors du document : {name}"),
            None => "source introuvable".into(),
        };
        println!(
            "{:>4}  {:>2}  {:<15}  {:>6.0} {:>6.0} {:>6.0} {:>6.0}  {}",
            item.page + 1,
            item.index,
            item.kind.label(),
            item.rect.x0,
            item.rect.y0,
            item.rect.x1,
            item.rect.y1,
            item.name.clone().unwrap_or_default()
        );
        println!(
            "                       {source}{}{}",
            item.content_type
                .as_ref()
                .map(|t| format!(" — {t}"))
                .unwrap_or_default(),
            if item.has_poster {
                ", avec affiche"
            } else {
                ", sans affiche"
            }
        );
    }
    println!("{} média(s)", items.len());
    Ok(())
}

/// `acr objects` : inventaire des objets dessinés, page par page.
fn cmd_objects(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::edit_objects;
    let (doc, _) = open(path)?;
    let only = option_value(rest, "--page").and_then(|v| v.parse::<usize>().ok());
    let mut total = 0usize;
    for (index, objects) in edit_objects::list_all(&doc)? {
        if only.is_some_and(|p| p != index + 1) {
            continue;
        }
        if objects.is_empty() {
            continue;
        }
        println!("page {}", index + 1);
        println!("  n°  nature          nom     rectangle                        remarque");
        for o in &objects {
            let note = if o.cut() {
                "rognée par une découpe"
            } else {
                ""
            };
            println!(
                "  {:>2}  {:<14}  {:<6}  {:>7.1} {:>7.1} {:>7.1} {:>7.1}  {}",
                o.index,
                o.kind.label(),
                o.name.clone().unwrap_or_default(),
                o.bbox.x0,
                o.bbox.y0,
                o.bbox.x1,
                o.bbox.y1,
                note
            );
            total += 1;
        }
    }
    if total == 0 {
        println!("aucun objet");
    }
    Ok(())
}

/// `acr edit-object` : déplacer, redimensionner, pivoter, recadrer, réordonner,
/// supprimer ou remplacer un objet.
#[allow(clippy::too_many_lines)] // une option par ligne de commande, à la suite
fn cmd_edit_object(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_core::{Matrix, Rect};
    use acrux_features::edit_objects::{self, Align, Edit, Order};
    let usage = "usage : edit-object <fichier> --page N --object n \
                 [--move dx,dy] [--scale sx[,sy]] [--rotate deg] [--place x0,y0,x1,y1] \
                 [--crop x0,y0,x1,y1] [--order devant|derriere|avancer|reculer] \
                 [--delete] [--image f.png] [--align gauche|centre-x|droite|haut|centre-y|bas] \
                 -o <sortie>";
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let page_number: usize = option_value(rest, "--page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let page = pages
        .get(page_number.wrapping_sub(1))
        .ok_or_else(|| acrux_core::Error::Corrupt(format!("page {page_number} inexistante")))?;

    // `--object 2` ou `--object 2,5,7` pour un alignement.
    let indices: Vec<usize> = match option_value(rest, "--object") {
        Some(spec) => spec
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect(),
        None => return Err(acrux_core::Error::Corrupt(usage.into())),
    };
    let Some(&first) = indices.first() else {
        return Err(acrux_core::Error::Corrupt(usage.into()));
    };

    if let Some(spec) = option_value(rest, "--align") {
        let how = match spec.as_str() {
            "gauche" | "left" => Align::Left,
            "centre-x" | "center-x" => Align::CenterX,
            "droite" | "right" => Align::Right,
            "haut" | "top" => Align::Top,
            "centre-y" | "center-y" => Align::CenterY,
            "bas" | "bottom" => Align::Bottom,
            other => {
                return Err(acrux_core::Error::Corrupt(format!(
                    "alignement « {other} » inconnu"
                )))
            }
        };
        let count = edit_objects::align(&doc, page, &indices, how)?;
        println!("{count} objet(s) alignés");
        return save(&doc, &out, rest);
    }

    let mut edits: Vec<Edit> = Vec::new();
    let objects = edit_objects::list(&doc, page)?;
    let object = objects
        .get(first)
        .ok_or_else(|| acrux_core::Error::Corrupt(format!("objet {first} inexistant")))?
        .clone();

    if rest.iter().any(|a| a == "--delete") {
        edits.push(Edit::Delete { index: first });
    }
    if let Some(spec) = option_value(rest, "--image") {
        let data =
            std::fs::read(spec).map_err(|e| acrux_core::Error::Corrupt(format!("{spec} : {e}")))?;
        edits.push(Edit::ReplaceImage { index: first, data });
    }
    if let Some(spec) = option_value(rest, "--crop") {
        edits.push(Edit::Clip {
            index: first,
            rect: parse_rect(spec)?,
        });
    }
    if let Some(spec) = option_value(rest, "--order") {
        let to = match spec.as_str() {
            "devant" | "front" => Order::Front,
            "derriere" | "derrière" | "back" => Order::Back,
            "avancer" | "forward" => Order::Forward,
            "reculer" | "backward" => Order::Backward,
            other => {
                return Err(acrux_core::Error::Corrupt(format!(
                    "ordre « {other} » inconnu : devant, derriere, avancer, reculer"
                )))
            }
        };
        edits.push(Edit::Arrange { index: first, to });
    }

    // Les transformations se composent dans l'ordre où on les écrit.
    let mut matrix: Option<Matrix> = None;
    let compose = |m: Matrix, slot: &mut Option<Matrix>| {
        *slot = Some(match *slot {
            Some(previous) => previous.then(&m),
            None => m,
        });
    };
    if let Some(spec) = option_value(rest, "--place") {
        let target = parse_rect(spec)?;
        compose(edit_objects::fit(object.bbox, target), &mut matrix);
    }
    if let Some(spec) = option_value(rest, "--scale") {
        let v: Vec<f64> = spec
            .split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        let (sx, sy) = match v.as_slice() {
            [s] => (*s, *s),
            [x, y, ..] => (*x, *y),
            _ => return Err(acrux_core::Error::Corrupt("--scale attend sx[,sy]".into())),
        };
        compose(
            edit_objects::around(object.bbox, Matrix::scale(sx, sy)),
            &mut matrix,
        );
    }
    if let Some(spec) = option_value(rest, "--rotate") {
        let degrees: f64 = spec
            .parse()
            .map_err(|_| acrux_core::Error::Corrupt("--rotate attend un angle".into()))?;
        compose(
            edit_objects::around(object.bbox, Matrix::rotate(degrees.to_radians())),
            &mut matrix,
        );
    }
    if let Some(spec) = option_value(rest, "--move") {
        let v: Vec<f64> = spec
            .split(',')
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        if v.len() != 2 {
            return Err(acrux_core::Error::Corrupt("--move attend dx,dy".into()));
        }
        compose(Matrix::translate(v[0], v[1]), &mut matrix);
    }
    if let Some(m) = matrix {
        edits.push(Edit::Transform {
            index: first,
            matrix: m,
        });
    }
    if edits.is_empty() {
        return Err(acrux_core::Error::Corrupt(usage.into()));
    }
    let count = edit_objects::apply(&doc, page, &edits)?;
    let _ = Rect::default();
    println!(
        "{count} modification(s) sur l'objet {first} ({}) de la page {page_number}",
        object.kind.label()
    );
    save(&doc, &out, rest)
}

/// `acr fillsign` : remplir et signer.
///
/// Quatre actions : `place`, `list`, `remove`, `flatten`.
fn cmd_fillsign(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::fillsign;
    let usage = "usage : fillsign <fichier> place|list|remove|flatten …";
    let pos = positional(rest);
    let action = pos.first().map_or("list", |v| v.as_str());
    let (doc, _) = open(path)?;
    match action {
        "list" => {
            let items = fillsign::list(&doc)?;
            if items.is_empty() {
                println!("aucun élément « remplir et signer »");
                return Ok(());
            }
            println!("page  n°  sorte     rectangle                        auteur");
            for item in items {
                println!(
                    "{:>4}  {:>2}  {:<8}  {:>7.1} {:>7.1} {:>7.1} {:>7.1}  {}",
                    item.page + 1,
                    item.index,
                    item.kind,
                    item.rect.x0,
                    item.rect.y0,
                    item.rect.x1,
                    item.rect.y1,
                    item.author.unwrap_or_default()
                );
            }
            Ok(())
        }
        "flatten" => {
            let out = output_arg(rest)?;
            let count = fillsign::flatten(&doc)?;
            println!("{count} élément(s) fondu(s) dans les pages");
            save(&doc, &out, rest)
        }
        "remove" => {
            let out = output_arg(rest)?;
            let bad = || {
                acrux_core::Error::Corrupt(
                    "usage : fillsign <fichier> remove <page> <index> -o <sortie>".into(),
                )
            };
            let page: usize = pos
                .get(1)
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n >= 1)
                .ok_or_else(bad)?;
            let index: usize = pos.get(2).and_then(|v| v.parse().ok()).ok_or_else(bad)?;
            fillsign::remove(&doc, page - 1, index)?;
            save(&doc, &out, rest)
        }
        "place" => {
            let out = output_arg(rest)?;
            let options = fillsign_options(rest)?;
            fillsign::place(&doc, &options)?;
            if rest.iter().any(|a| a == "--flatten") {
                fillsign::flatten(&doc)?;
            }
            save(&doc, &out, rest)
        }
        _ => Err(acrux_core::Error::Corrupt(usage.into())),
    }
}

/// Assemble les options de pose depuis la ligne de commande.
fn fillsign_options(rest: &[String]) -> acrux_core::Result<acrux_features::fillsign::Options> {
    use acrux_features::fillsign::{cutout, Fit, Item, Mark, Options, Pen};
    let page: usize = option_value(rest, "--page")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
        - 1;
    let Some(spec) = option_value(rest, "--rect") else {
        return Err(acrux_core::Error::Corrupt(
            "indiquer l'emplacement avec --rect x0,y0,x1,y1".into(),
        ));
    };
    let mut options = Options {
        page,
        rect: parse_rect(spec)?,
        fit: if rest.iter().any(|a| a == "--stretch") {
            Fit::Stretch
        } else {
            Fit::Contain
        },
        author: option_value(rest, "--author").cloned(),
        ..Options::default()
    };
    if let Some(spec) = option_value(rest, "--color") {
        options.color = parse_rgb(spec)?;
    }
    let sources = [
        option_value(rest, "--draw").map(|v| ("draw", v)),
        option_value(rest, "--typed").map(|v| ("typed", v)),
        option_value(rest, "--image").map(|v| ("image", v)),
        option_value(rest, "--text").map(|v| ("text", v)),
        option_value(rest, "--mark").map(|v| ("mark", v)),
    ];
    let chosen: Vec<_> = sources.into_iter().flatten().collect();
    if chosen.len() != 1 {
        return Err(acrux_core::Error::Corrupt(
            "indiquer une source et une seule : --draw, --typed, --image, --text ou --mark".into(),
        ));
    }
    options.item = match chosen[0] {
        ("draw", file) => {
            let mut pen = Pen::default();
            if let Some(width) = option_value(rest, "--pen").and_then(|v| v.parse::<f64>().ok()) {
                pen.width = width;
            }
            Item::Drawn {
                strokes: read_strokes(file)?,
                pen,
            }
        }
        ("typed", text) => Item::Typed { text: text.clone() },
        ("text", text) => Item::Text { text: text.clone() },
        ("mark", name) => Item::Mark(Mark::from_name(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "marque « {name} » inconnue : check, cross, dot, circle, line"
            ))
        })?),
        ("image", file) => {
            let data = std::fs::read(file)
                .map_err(|e| acrux_core::Error::Corrupt(format!("{file} : {e}")))?;
            let cut = if rest.iter().any(|a| a == "--no-cutout") {
                None
            } else {
                let mut o = cutout::Options {
                    recolor: !rest.iter().any(|a| a == "--keep-color"),
                    ..cutout::Options::default()
                };
                if let Some(v) = option_value(rest, "--softness").and_then(|v| v.parse().ok()) {
                    o.softness = v;
                }
                Some(o)
            };
            Item::Image { data, cutout: cut }
        }
        _ => {
            return Err(acrux_core::Error::Corrupt(
                "source de signature inconnue".into(),
            ))
        }
    };
    Ok(options)
}

/// Lit un fichier de traits : une ligne « x y [pression] » par point, une
/// ligne vide entre deux traits, `#` pour un commentaire. L'axe Y monte,
/// comme dans une page PDF.
fn read_strokes(path: &str) -> acrux_core::Result<Vec<acrux_features::fillsign::Stroke>> {
    use acrux_features::fillsign::{InkPoint, Stroke};
    let text = std::fs::read_to_string(path)
        .map_err(|e| acrux_core::Error::Corrupt(format!("{path} : {e}")))?;
    let mut strokes = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            if !current.is_empty() {
                strokes.push(Stroke {
                    points: std::mem::take(&mut current),
                });
            }
            continue;
        }
        let values: Vec<f64> = line
            .split([' ', '\t', ',', ';'])
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect();
        if values.len() < 2 {
            return Err(acrux_core::Error::Corrupt(format!(
                "{path} : « {line} » n'est pas un point « x y »"
            )));
        }
        current.push(InkPoint {
            x: values[0],
            y: values[1],
            pressure: values.get(2).copied(),
        });
    }
    if !current.is_empty() {
        strokes.push(Stroke { points: current });
    }
    if strokes.is_empty() {
        return Err(acrux_core::Error::Corrupt(format!(
            "{path} : aucun trait lisible"
        )));
    }
    Ok(strokes)
}

/// `acr signatures` : inventaire des champs de signature, sans jugement
/// cryptographique — ce que le fichier *déclare*.
fn cmd_signatures(path: &str) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let signatures = acrux_features::signature::list_signatures(&doc)?;
    if signatures.is_empty() {
        println!("aucun champ de signature");
        return Ok(());
    }
    for info in &signatures {
        println!("champ « {} »", info.field_name);
        if info.unsigned {
            println!("  (emplacement réservé, non signé)");
            continue;
        }
        println!(
            "  filtre           {} / {}",
            info.filter,
            info.sub_filter.name()
        );
        for (label, value) in [
            ("nom déclaré", &info.name),
            ("date déclarée", &info.date),
            ("motif", &info.reason),
            ("lieu", &info.location),
            ("contact", &info.contact_info),
        ] {
            if let Some(v) = value {
                println!("  {label:<16} {v}");
            }
        }
        if let (Some(page), Some(rect)) = (info.page, info.rect) {
            if rect.width() > 0.0 {
                println!(
                    "  apparence        page {} [{:.0} {:.0} {:.0} {:.0}]",
                    page + 1,
                    rect.x0,
                    rect.y0,
                    rect.x1,
                    rect.y1
                );
            } else {
                println!("  apparence        invisible (page {})", page + 1);
            }
        }
        let ranges: Vec<String> = info
            .byte_range
            .iter()
            .map(|(s, l)| format!("{s}+{l}"))
            .collect();
        println!(
            "  /ByteRange       [{}] → {} octets couverts sur {}",
            ranges.join(" "),
            info.byte_range.iter().map(|(_, l)| l).sum::<usize>(),
            doc.bytes().len()
        );
        if info.certification {
            println!(
                "  certification    /DocMDP niveau {}",
                info.doc_mdp_level.unwrap_or(0)
            );
        }
        println!("  → verdicts : `acr verify {path} --roots <dossier de certificats>`");
    }
    println!("{} champ(s) de signature", signatures.len());
    Ok(())
}

/// `acr verify` : les cinq verdicts, pour chaque signature.
fn cmd_verify(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let (doc, _) = open(path)?;
    let roots = match option_value(rest, "--roots") {
        Some(directory) => load_roots(directory)?,
        None => Vec::new(),
    };
    let signatures = acrux_features::signature::list_signatures(&doc)?;
    let mut reports = Vec::new();
    for info in &signatures {
        reports.push(acrux_features::signature::verify_signature(
            &doc, info, &roots,
        )?);
    }
    if rest.iter().any(|a| a == "--json") {
        print_report_json(&reports);
        return Ok(());
    }
    if reports.is_empty() {
        println!("aucun champ de signature");
        return Ok(());
    }
    for report in &reports {
        println!(
            "champ « {} » ({})",
            report.field_name,
            report.sub_filter.name()
        );
        if let Some(signer) = &report.signer {
            println!("  signataire       {signer}");
        }
        if let Some(when) = report.signing_time {
            println!("  date signée      {}", when.to_display());
        }
        for (label, verdict) in [
            ("condensé", &report.digest),
            ("signature", &report.signature),
            ("chaîne", &report.chain),
            ("couverture", &report.coverage),
            ("modifications", &report.modifications),
        ] {
            println!(
                "  {label:<16} {:<8} {}",
                verdict.status.mark(),
                verdict.detail
            );
        }
        for (index, element) in report.chain_path.iter().enumerate() {
            println!("  chaîne {}          {element}", index + 1);
        }
        for note in &report.notes {
            println!("  remarque         {note}");
        }
        println!(
            "  → {}",
            if report.is_fully_valid() {
                "signature valide".to_string()
            } else {
                report.summary()
            }
        );
    }
    let valid = reports.iter().filter(|r| r.is_fully_valid()).count();
    println!("{valid} signature(s) valide(s) sur {}", reports.len());
    if valid != reports.len() {
        return Err(acrux_core::Error::Corrupt(
            "au moins une signature n'est pas pleinement valide".into(),
        ));
    }
    Ok(())
}

/// `acr sign` : pose une signature CMS détachée par mise à jour incrémentale.
fn cmd_sign(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::signature::{sign, Appearance, SignOptions, SigningKey};

    let out = option_value(rest, "-o")
        .or_else(|| option_value(rest, "--output"))
        .ok_or_else(|| acrux_core::Error::Corrupt("indiquer la sortie avec -o".into()))?;
    let key_path = option_value(rest, "--key")
        .ok_or_else(|| acrux_core::Error::Corrupt("indiquer la clé avec --key <cle.der>".into()))?;
    let cert_path = option_value(rest, "--cert").ok_or_else(|| {
        acrux_core::Error::Corrupt("indiquer le certificat avec --cert <cert.der>".into())
    })?;
    let mut key = SigningKey::from_der(&std::fs::read(key_path)?, &std::fs::read(cert_path)?)?;
    if let Some(chain) = option_value(rest, "--chain") {
        let data = std::fs::read(chain)?;
        let certificates = acrux_features::signature::x509::parse_many(&data)?;
        key.chain = certificates;
    }
    let appearance = match (option_value(rest, "--page"), option_value(rest, "--rect")) {
        (Some(page), Some(spec)) => {
            let page: usize = page
                .parse()
                .map_err(|_| acrux_core::Error::Corrupt(format!("page « {page} » invalide")))?;
            let v: Vec<f64> = spec
                .split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            if v.len() != 4 || page == 0 {
                return Err(acrux_core::Error::Corrupt(
                    "apparence : --page N --rect x0,y0,x1,y1".into(),
                ));
            }
            Some(Appearance {
                page: page - 1,
                rect: acrux_core::Rect::new(v[0], v[1], v[2], v[3]),
            })
        }
        (None, None) => None,
        _ => {
            return Err(acrux_core::Error::Corrupt(
                "apparence : --page et --rect vont ensemble".into(),
            ))
        }
    };
    let (doc, _) = open(path)?;
    let bytes = sign(
        &doc,
        &key,
        &SignOptions {
            field_name: option_value(rest, "--field").cloned(),
            name: option_value(rest, "--name").cloned(),
            reason: option_value(rest, "--reason").cloned(),
            location: option_value(rest, "--location").cloned(),
            contact_info: option_value(rest, "--contact").cloned(),
            appearance,
            ..SignOptions::default()
        },
    )?;
    std::fs::write(out, &bytes)?;
    println!(
        "écrit : {out} ({} octets, signature ajoutée par mise à jour incrémentale)",
        bytes.len()
    );
    println!("signataire : {}", key.certificate.subject.to_display());
    println!("vérifier : acr verify {out} --roots <dossier de certificats>");
    Ok(())
}

// --- Propriétés du document : métadonnées, vue initiale ---------------------

fn cmd_metadata(path: &str) -> acrux_core::Result<()> {
    use acrux_features::docinfo::{read_metadata, read_view_preferences, Field};
    let (doc, _) = open(path)?;
    let meta = read_metadata(&doc)?;
    for field in Field::ALL {
        if let Some(value) = meta.get(field) {
            println!("{:<18}: {value}", field.label());
        }
    }
    if let Some(trapped) = &meta.trapped {
        println!("{:<18}: {trapped}", "Recouvrement");
    }
    println!(
        "{:<18}: {}",
        "Paquet XMP",
        if meta.has_xmp { "oui" } else { "non" }
    );
    if !meta.custom.is_empty() {
        println!("Propriétés personnalisées :");
        for (key, value) in &meta.custom {
            println!("  {key} = {value}");
        }
    }
    if !meta.divergences.is_empty() {
        println!(
            "Divergences entre /Info et le XMP ({}) :",
            meta.divergences.len()
        );
        for d in &meta.divergences {
            println!(
                "  {} : /Info « {} » ≠ XMP « {} »",
                d.field.label(),
                d.info,
                d.xmp
            );
        }
    }
    let prefs = read_view_preferences(&doc)?;
    print_view_preferences(&prefs);
    Ok(())
}

fn print_view_preferences(prefs: &acrux_features::docinfo::ViewPreferences) {
    use acrux_features::docinfo::{describe_open_action, Duplex, PageLayout, PageMode};
    println!(
        "{:<18}: {}",
        "Panneau",
        prefs.page_mode.map_or("—", PageMode::label)
    );
    println!(
        "{:<18}: {}",
        "Disposition",
        prefs.page_layout.map_or("—", PageLayout::label)
    );
    println!(
        "{:<18}: {}",
        "À l'ouverture",
        prefs
            .open_action
            .as_ref()
            .map_or_else(|| "—".to_string(), describe_open_action)
    );
    let mut flags = Vec::new();
    for (on, label) in [
        (prefs.display_document_title, "titre du document en fenêtre"),
        (prefs.hide_toolbar, "barre d'outils masquée"),
        (prefs.hide_menubar, "barre de menus masquée"),
        (prefs.hide_window_ui, "interface masquée"),
        (prefs.fit_window, "fenêtre ajustée"),
        (prefs.center_window, "fenêtre centrée"),
    ] {
        if on {
            flags.push(label);
        }
    }
    if !flags.is_empty() {
        println!("{:<18}: {}", "Fenêtre", flags.join(", "));
    }
    if let Some(duplex) = prefs.duplex {
        println!(
            "{:<18}: {}",
            "Impression",
            match duplex {
                Duplex::Simplex => "recto seul",
                Duplex::ShortEdge => "recto verso, petit côté",
                Duplex::LongEdge => "recto verso, grand côté",
            }
        );
    }
    if !prefs.print_ranges.is_empty() {
        let ranges: Vec<String> = prefs
            .print_ranges
            .iter()
            .map(|(a, b)| format!("{}-{}", a + 1, b + 1))
            .collect();
        println!("{:<18}: {}", "Pages à imprimer", ranges.join(", "));
    }
    if let Some(copies) = prefs.copies {
        println!("{:<18}: {copies}", "Copies");
    }
}

fn cmd_set_metadata(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::docinfo::{read_metadata, remove_all_metadata, set_metadata, Field};
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    if rest.iter().any(|a| a == "--clear") {
        let removed = remove_all_metadata(&doc)?;
        if removed.is_empty() {
            println!("aucune métadonnée à retirer");
        }
        for item in &removed {
            println!("retiré : {item}");
        }
        return save(&doc, &out, rest);
    }
    let mut meta = read_metadata(&doc)?;
    let mut changed = 0;
    for (option, field) in [
        ("--title", Field::Title),
        ("--author", Field::Author),
        ("--subject", Field::Subject),
        ("--keywords", Field::Keywords),
        ("--creator", Field::Creator),
        ("--producer", Field::Producer),
    ] {
        if let Some(value) = option_value(rest, option) {
            // Une chaîne vide efface le champ : c'est la seule façon de le
            // retirer sans ajouter une option de plus.
            meta.set(field, (!value.is_empty()).then(|| value.clone()));
            changed += 1;
        }
    }
    if changed == 0 {
        return Err(acrux_core::Error::Corrupt(
            "rien à écrire : indiquer au moins --title, --author, --subject, --keywords, \
             --creator, --producer ou --clear"
                .into(),
        ));
    }
    set_metadata(&doc, &meta)?;
    println!("{changed} champ(s) écrits dans /Info et dans le paquet XMP");
    save(&doc, &out, rest)
}

fn cmd_view_prefs(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::docinfo::{
        read_view_preferences, set_view_preferences, PageLayout, PageMode,
    };
    use acrux_features::navigation::{Action, Destination, View};
    let (doc, _) = open(path)?;
    let mut prefs = read_view_preferences(&doc)?;
    let mut changed = false;

    if let Some(name) = option_value(rest, "--page-mode") {
        prefs.page_mode = Some(PageMode::parse(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "mode « {name} » inconnu : aucun, signets, vignettes, plein-ecran, \
                 calques, pieces-jointes"
            ))
        })?);
        changed = true;
    }
    if let Some(name) = option_value(rest, "--layout") {
        prefs.page_layout = Some(PageLayout::parse(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "disposition « {name} » inconnue : une-page, continu, deux-colonnes, \
                 deux-colonnes-droite, deux-pages, deux-pages-droite"
            ))
        })?);
        changed = true;
    }
    let zoom = option_value(rest, "--open-zoom");
    if option_value(rest, "--open-page").is_some() || zoom.is_some() {
        let pages = collect_pages(&doc)?;
        // Sans --open-page, on garde la page déjà visée (la première à défaut).
        let page = match option_value(rest, "--open-page") {
            Some(spec) => {
                let n: usize = spec.trim().parse().map_err(|_| {
                    acrux_core::Error::Corrupt(format!("« {spec} » n'est pas un numéro de page"))
                })?;
                if n == 0 || n > pages.len() {
                    return Err(acrux_core::Error::Corrupt(format!(
                        "page {n} hors du document ({} pages)",
                        pages.len()
                    )));
                }
                n - 1
            }
            None => match &prefs.open_action {
                Some(Action::GoTo(d)) => d.page,
                _ => 0,
            },
        };
        let view = match zoom.map(String::as_str) {
            None => match &prefs.open_action {
                Some(Action::GoTo(d)) => d.view.clone(),
                _ => View::Xyz {
                    left: None,
                    top: None,
                    zoom: None,
                },
            },
            Some("fit" | "page") => View::Fit,
            Some("largeur") => View::FitWidth { top: None },
            Some("hauteur") => View::FitHeight { left: None },
            Some("contenu") => View::FitBox,
            Some(percent) => {
                let value: f64 = percent.trim().trim_end_matches('%').parse().map_err(|_| {
                    acrux_core::Error::Corrupt(format!(
                        "zoom « {percent} » : un pourcentage, ou fit, largeur, hauteur, contenu"
                    ))
                })?;
                if value <= 0.0 {
                    return Err(acrux_core::Error::Corrupt("zoom nul ou négatif".into()));
                }
                View::Xyz {
                    left: None,
                    top: None,
                    zoom: Some(value / 100.0),
                }
            }
        };
        prefs.open_action = Some(Action::GoTo(Destination { page, view }));
        changed = true;
    }
    if !changed {
        print_view_preferences(&prefs);
        return Ok(());
    }
    let out = output_arg(rest)?;
    set_view_preferences(&doc, &prefs)?;
    print_view_preferences(&prefs);
    save(&doc, &out, rest)
}

// --- Signets ---------------------------------------------------------------

fn cmd_bookmarks(path: &str) -> acrux_core::Result<()> {
    use acrux_features::outline_edit::{flatten, read_outline};
    let (doc, _) = open(path)?;
    let tree = read_outline(&doc)?;
    let flat = flatten(&tree);
    if flat.is_empty() {
        println!("aucun signet");
        return Ok(());
    }
    for (path, node) in &flat {
        let target = node
            .action
            .as_ref()
            .map_or_else(|| "(sans cible)".to_string(), describe_action);
        let mut style = Vec::new();
        if node.bold {
            style.push("gras");
        }
        if node.italic {
            style.push("italique");
        }
        if node.color.is_some() {
            style.push("couleur");
        }
        if !node.children.is_empty() {
            style.push(if node.open { "ouvert" } else { "fermé" });
        }
        let style = if style.is_empty() {
            String::new()
        } else {
            format!("  [{}]", style.join(", "))
        };
        println!(
            "{}{}  →  {target}{style}",
            "  ".repeat(path.len() - 1),
            node.title
        );
    }
    println!("{} signet(s)", flat.len());
    Ok(())
}

/// Lit le format texte des signets : une ligne par signet, la profondeur
/// donnée par les tabulations de début de ligne, puis « titre<TAB>page ».
fn parse_bookmark_file(
    text: &str,
    pages: usize,
) -> acrux_core::Result<Vec<acrux_features::outline_edit::OutlineNode>> {
    use acrux_features::navigation::{Action, Destination, View};
    use acrux_features::outline_edit::OutlineNode;
    let mut roots: Vec<OutlineNode> = Vec::new();
    // Pile des parents ouverts : le chemin du dernier nœud de chaque niveau.
    let mut stack: Vec<Vec<usize>> = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        let depth = line.chars().take_while(|c| *c == '\t').count();
        let body = &line[depth..];
        let (title, page) = match body.rsplit_once('\t') {
            Some((title, digits)) => {
                let n: usize = digits.trim().parse().map_err(|_| {
                    acrux_core::Error::Corrupt(format!(
                        "ligne {} : « {} » n'est pas un numéro de page",
                        number + 1,
                        digits.trim()
                    ))
                })?;
                (title.trim(), Some(n))
            }
            None => (body.trim(), None),
        };
        if title.is_empty() {
            return Err(acrux_core::Error::Corrupt(format!(
                "ligne {} : titre vide",
                number + 1
            )));
        }
        if let Some(n) = page {
            if n == 0 || n > pages {
                return Err(acrux_core::Error::Corrupt(format!(
                    "ligne {} : page {n} hors du document ({pages} pages)",
                    number + 1
                )));
            }
        }
        let node = OutlineNode {
            title: title.to_string(),
            // Le premier niveau reste déplié, comme dans Acrobat.
            open: depth == 0,
            action: page.map(|n| {
                Action::GoTo(Destination {
                    page: n - 1,
                    view: View::Fit,
                })
            }),
            ..OutlineNode::default()
        };
        // Un saut de niveau est ramené au niveau immédiatement disponible.
        stack.truncate(depth);
        let path = match stack.last().cloned() {
            None => {
                roots.push(node);
                vec![roots.len() - 1]
            }
            Some(parent) => {
                let mut current = &mut roots;
                for step in &parent {
                    current = match current.get_mut(*step) {
                        Some(n) => &mut n.children,
                        None => {
                            return Err(acrux_core::Error::Corrupt(format!(
                                "ligne {} : indentation incohérente",
                                number + 1
                            )))
                        }
                    };
                }
                current.push(node);
                let mut path = parent;
                path.push(current.len() - 1);
                path
            }
        };
        stack.push(path);
    }
    Ok(roots)
}

fn cmd_set_bookmarks(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::outline_edit::{flatten, set_outline};
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let Some(file) = pos.first() else {
        return Err(acrux_core::Error::Corrupt(
            "usage : set-bookmarks <fichier> <signets.txt> -o <sortie>".into(),
        ));
    };
    let text = std::fs::read_to_string(file.as_str())?;
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?.len();
    let tree = parse_bookmark_file(&text, pages)?;
    let total = flatten(&tree).len();
    set_outline(&doc, &tree)?;
    println!("{total} signet(s) écrits");
    save(&doc, &out, rest)
}

fn cmd_auto_bookmarks(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::outline_edit::{flatten, outline_from_headings, set_outline};
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let tree = outline_from_headings(&doc)?;
    if tree.is_empty() {
        return Err(acrux_core::Error::Corrupt(
            "aucun titre reconnu : ni structure balisée, ni titre détectable dans la mise en page"
                .into(),
        ));
    }
    let flat = flatten(&tree);
    for (path, node) in &flat {
        println!("{}{}", "  ".repeat(path.len() - 1), node.title);
    }
    set_outline(&doc, &tree)?;
    println!("{} signet(s) créés", flat.len());
    save(&doc, &out, rest)
}

// --- Liens -----------------------------------------------------------------

fn cmd_autolink(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let (doc, _) = open(path)?;
    let report = acrux_features::linkedit::autolink(&doc)?;
    for found in &report.found {
        let Some(zone) = found.rects.first() else {
            continue;
        };
        println!(
            "page {:>4}  [{:.1} {:.1} {:.1} {:.1}]  {}",
            found.page + 1,
            zone.x0,
            zone.y0,
            zone.x1,
            zone.y1,
            found.uri
        );
    }
    for warning in &report.warnings {
        println!("attention : {warning}");
    }
    println!(
        "{} adresse(s) reconnue(s), {} lien(s) posés, {} déjà liée(s)",
        report.found.len(),
        report.added,
        report.already_linked
    );
    if report.added == 0 {
        println!("rien à écrire");
        return Ok(());
    }
    save(&doc, &out, rest)
}

fn cmd_link(path: &str, rest: &[String]) -> acrux_core::Result<()> {
    use acrux_features::linkedit::{add_link, LinkTarget};
    let out = output_arg(rest)?;
    let pos = positional(rest);
    let (Some(page), Some(spec), Some(target)) = (pos.first(), pos.get(1), pos.get(2)) else {
        return Err(acrux_core::Error::Corrupt(
            "usage : link <fichier> <page> <x0,y0,x1,y1> <cible> -o <sortie>".into(),
        ));
    };
    let zone = parse_rect(spec)?;
    let target = LinkTarget::parse(target)?;
    let (doc, _) = open(path)?;
    let pages = collect_pages(&doc)?;
    let number: usize = page
        .trim()
        .parse()
        .ok()
        .filter(|n| *n >= 1 && *n <= pages.len())
        .ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "page « {page} » hors du document ({} pages)",
                pages.len()
            ))
        })?;
    add_link(&doc, &pages[number - 1], zone, &target)?;
    println!(
        "lien posé page {number} sur [{:.1} {:.1} {:.1} {:.1}]",
        zone.x0, zone.y0, zone.x1, zone.y1
    );
    save(&doc, &out, rest)
}

/// Gabarit de page commun aux quatre formes de `create` : `--size` (ou
/// `--page-size`), `--orientation`, `--pages` et `--margin`.
fn create_setup(rest: &[String]) -> acrux_core::Result<acrux_features::create::PageSetup> {
    use acrux_features::create::{Margins, Orientation, PageSetup, PageSize};
    let mut setup = PageSetup::default();
    let named = option_value(rest, "--page-size").or_else(|| option_value(rest, "--size"));
    if let Some(spec) = named {
        // En mode texte, `--size` est le corps de la police : un nombre seul
        // n'est donc pas un format de page.
        if spec.parse::<f64>().is_err() {
            setup.size = PageSize::from_name(spec).ok_or_else(|| {
                acrux_core::Error::Corrupt(format!(
                    "format de page inconnu : « {spec} » (A0 à A6, lettre, légal, tabloïd, \
                     dl, c4, c5, c6, monarch, ou « 210x297mm »)"
                ))
            })?;
        }
    }
    if let Some(spec) = option_value(rest, "--orientation") {
        setup.orientation = Orientation::from_name(spec).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "orientation inconnue : « {spec} » (portrait, paysage)"
            ))
        })?;
    }
    if let Some(n) = option_value(rest, "--pages").and_then(|v| v.parse::<usize>().ok()) {
        setup.pages = n;
    }
    if let Some(m) = option_value(rest, "--margin").and_then(|v| v.parse::<f64>().ok()) {
        setup.margins = Margins {
            top: m,
            bottom: m,
            left: m,
            right: m,
        };
    }
    Ok(setup)
}

/// Mise en page du texte et du Markdown : `--font`, `--size` (corps),
/// `--align`, `--header`, `--footer`, plus le gabarit de page.
fn create_text_layout(rest: &[String]) -> acrux_core::Result<acrux_features::create::TextLayout> {
    use acrux_features::create::{TextAlign, TextLayout};
    let mut layout = TextLayout {
        setup: create_setup(rest)?,
        ..TextLayout::default()
    };
    if let Some(name) = option_value(rest, "--font") {
        layout.font = acrux_features::stamp::StandardFont::from_name(name).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "police inconnue : « {name} » (helvetica, times, courier, avec bold / italic)"
            ))
        })?;
    }
    if let Some(size) = option_value(rest, "--size").and_then(|v| v.parse::<f64>().ok()) {
        layout.size = size;
    }
    if let Some(spec) = option_value(rest, "--align") {
        layout.align = TextAlign::from_name(spec).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "alignement inconnu : « {spec} » (gauche, droite, centre, justifie)"
            ))
        })?;
    }
    layout.header = option_value(rest, "--header").cloned();
    layout.footer = option_value(rest, "--footer").cloned();
    Ok(layout)
}

/// Montage des images : gabarit de page, `--fit`, `--dpi`, `--grid CxL`,
/// `--background`.
fn create_image_layout(rest: &[String]) -> acrux_core::Result<acrux_features::create::ImageLayout> {
    use acrux_features::create::{Fit, ImageLayout};
    let mut layout = ImageLayout {
        setup: create_setup(rest)?,
        ..ImageLayout::default()
    };
    if let Some(spec) = option_value(rest, "--fit") {
        layout.fit = Fit::from_name(spec).ok_or_else(|| {
            acrux_core::Error::Corrupt(format!(
                "ajustement inconnu : « {spec} » (contain, cover, actual)"
            ))
        })?;
    }
    if let Some(dpi) = option_value(rest, "--dpi").and_then(|v| v.parse::<f64>().ok()) {
        layout.dpi = dpi;
    }
    if let Some(spec) = option_value(rest, "--grid") {
        let (c, r) = spec.split_once(['x', 'X', '*']).ok_or_else(|| {
            acrux_core::Error::Corrupt("grille « colonnesxlignes » attendue".into())
        })?;
        layout.columns = c.trim().parse().unwrap_or(1);
        layout.rows = r.trim().parse().unwrap_or(1);
    }
    if let Some(spec) = option_value(rest, "--background") {
        layout.background = Some(parse_rgb(spec)?);
    }
    Ok(layout)
}

/// Fichiers cités après une option à valeurs multiples (`--images`), jusqu'à
/// la prochaine option.
fn values_after<'a>(rest: &'a [String], flag: &str) -> Vec<&'a String> {
    let Some(i) = rest.iter().position(|a| a == flag) else {
        return Vec::new();
    };
    rest[i + 1..]
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .collect()
}

fn cmd_create(rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let doc = if rest.iter().any(|a| a == "--blank") {
        acrux_features::create::new_document(&create_setup(rest)?)?
    } else if rest.iter().any(|a| a == "--images") {
        let files = values_after(rest, "--images");
        if files.is_empty() {
            return Err(acrux_core::Error::Corrupt(
                "usage : create --images <fichier>... -o <sortie>".into(),
            ));
        }
        let images = files
            .iter()
            .map(acrux_features::create::ImageInput::from_path)
            .collect::<acrux_core::Result<Vec<_>>>()?;
        acrux_features::create::from_images(&images, &create_image_layout(rest)?)?
    } else if let Some(file) = option_value(rest, "--3d") {
        let model =
            std::fs::read(file).map_err(|e| acrux_core::Error::Io(format!("{file}: {e}")))?;
        acrux_features::create::from_3d(&model, &create_setup(rest)?)?
    } else if let Some(file) = option_value(rest, "--text") {
        let text = std::fs::read_to_string(file)
            .map_err(|e| acrux_core::Error::Io(format!("{file}: {e}")))?;
        acrux_features::create::from_text(&text, &create_text_layout(rest)?)?
    } else if let Some(file) = option_value(rest, "--markdown") {
        let source = std::fs::read_to_string(file)
            .map_err(|e| acrux_core::Error::Io(format!("{file}: {e}")))?;
        acrux_features::create::from_markdown(&source, &create_text_layout(rest)?)?
    } else {
        return Err(acrux_core::Error::Corrupt(
            "create attend --blank, --images, --3d, --text ou --markdown".into(),
        ));
    };
    let bytes = doc.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!(
        "écrit : {out} ({} octets, {} page(s))",
        bytes.len(),
        collect_pages(&doc)?.len()
    );
    Ok(())
}

fn cmd_combine(rest: &[String]) -> acrux_core::Result<()> {
    let out = output_arg(rest)?;
    let files = positional(rest);
    if files.is_empty() {
        return Err(acrux_core::Error::Corrupt(
            "usage : combine <fichier>... -o <sortie>".into(),
        ));
    }
    let inputs = files
        .iter()
        .map(acrux_features::create::CombineInput::from_path)
        .collect::<acrux_core::Result<Vec<_>>>()?;
    let options = acrux_features::create::CombineOptions {
        bookmarks: rest.iter().any(|a| a == "--bookmarks"),
        table_of_contents: rest.iter().any(|a| a == "--toc"),
        toc_title: option_value(rest, "--toc-title")
            .cloned()
            .unwrap_or_else(|| "Sommaire".into()),
        page_numbers: rest.iter().any(|a| a == "--numbers"),
        image_layout: create_image_layout(rest)?,
        text_layout: create_text_layout(rest)?,
    };
    let doc = acrux_features::create::combine(&inputs, &options)?;
    let bytes = doc.save_full()?;
    std::fs::write(&out, &bytes)?;
    println!(
        "écrit : {out} ({} octets, {} page(s), {} fichier(s))",
        bytes.len(),
        collect_pages(&doc)?.len(),
        inputs.len()
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Les traits d'encre, les terminaisons, les polices et les sauts de
    /// ligne se lisent comme l'aide les décrit ; ce qui ne se lit pas est
    /// refusé avec un message, jamais deviné.
    #[test]
    fn annotate_lit_ses_options() {
        use acrux_features::annotations::LineEnding;
        use acrux_features::stamp::StandardFont;
        let strokes = ink_points_arg("10,10 40,60 80,20;100,100 140,140").unwrap();
        assert_eq!(strokes.len(), 2);
        assert_eq!(strokes[0].len(), 3);
        assert_eq!(strokes[1][1], acrux_core::Point::new(140.0, 140.0));
        assert!(ink_points_arg("10;20").is_err());
        assert!(ink_points_arg(" ; ").is_err());
        let rest = args(&[
            "--head",
            "closed",
            "--tail",
            "circle",
            "--font",
            "times-bold",
        ]);
        assert_eq!(
            line_ending_arg(&rest, "--head", LineEnding::None).unwrap(),
            LineEnding::ClosedArrow
        );
        assert_eq!(
            line_ending_arg(&rest, "--tail", LineEnding::None).unwrap(),
            LineEnding::Circle
        );
        assert_eq!(
            line_ending_arg(&args(&[]), "--head", LineEnding::OpenArrow).unwrap(),
            LineEnding::OpenArrow
        );
        assert!(
            line_ending_arg(&args(&["--head", "losange"]), "--head", LineEnding::None).is_err()
        );
        assert_eq!(standard_font_arg(&rest).unwrap(), StandardFont::TimesBold);
        assert_eq!(
            standard_font_arg(&args(&["--font", "courier"])).unwrap(),
            StandardFont::Courier
        );
        assert!(standard_font_arg(&args(&["--font", "comic"])).is_err());
        assert_eq!(
            cli_text("Ligne 1\nLigne 2"),
            "Ligne 1
Ligne 2"
        );
        // Les options à valeur ne sont pas des coordonnées.
        let rest = args(&[
            "1", "circle", "10", "20", "30", "40", "--width", "3", "--fill", "1,1,0",
        ]);
        let pos: Vec<&str> = positional(&rest).into_iter().map(String::as_str).collect();
        assert_eq!(pos, ["1", "circle", "10", "20", "30", "40"]);
        assert!(number_option(&args(&["--width", "épais"]), "--width").is_err());
        assert_eq!(
            number_option(&args(&["--opacity", "0,5"]), "--opacity").unwrap(),
            Some(0.5)
        );
    }

    /// Les options d'`annot-set` : couleur en hexadécimal ou en « r,g,b »,
    /// déplacement relatif au rectangle actuel, fond retiré, statut en
    /// français ou en anglais.
    #[test]
    #[allow(clippy::float_cmp)] // des huitièmes d'octet : des valeurs exactes
    fn annot_set_lit_ses_options() {
        use acrux_features::annotations::review::ReviewState;
        assert_eq!(color_arg("FF8000").unwrap(), [1.0, 128.0 / 255.0, 0.0]);
        assert_eq!(color_arg("#0000ff").unwrap(), [0.0, 0.0, 1.0]);
        assert_eq!(color_arg("0,1,0").unwrap(), [0.0, 1.0, 0.0]);
        assert!(color_arg("vert").is_err());
        let rect = acrux_core::Rect::new(10.0, 10.0, 50.0, 30.0);
        let c = annot_changes_arg(
            &args(&["--move", "5,-2", "--fill", "none", "--text", "a\nb"]),
            rect,
        )
        .unwrap();
        assert_eq!(c.rect, Some(acrux_core::Rect::new(15.0, 8.0, 55.0, 28.0)));
        assert_eq!(c.fill, Some(None));
        assert_eq!(
            c.contents.as_deref(),
            Some(
                "a
b"
            )
        );
        assert!(annot_changes_arg(&args(&["--move", "5"]), rect).is_err());
        assert!(annot_changes_arg(&args(&[]), rect).unwrap().is_empty());
        assert_eq!(
            review_state_arg(&args(&["--state", "Accepté"])).unwrap(),
            Some(ReviewState::Accepted)
        );
        assert_eq!(
            review_state_arg(&args(&["--state", "none"])).unwrap(),
            Some(ReviewState::None)
        );
        assert!(review_state_arg(&args(&["--state", "peut-être"])).is_err());
        assert_eq!(marked_arg(&args(&["--marked", "oui"])).unwrap(), Some(true));
        assert!(marked_arg(&args(&["--marked", "bof"])).is_err());
        let pos = args(&["1", "3", "--state", "accepted", "--marked", "non"]);
        let pos: Vec<&str> = positional(&pos).into_iter().map(String::as_str).collect();
        assert_eq!(pos, ["1", "3"]);
    }

    #[test]
    fn parse_permissions_levels_and_bits() {
        let p = parse_permissions(&args(&["--print", "low"])).unwrap();
        assert_eq!(p.print_level(), PrintLevel::Low);
        assert!(p.modify && p.copy);
        for none in [&["--print", "none"][..], &["--no-print"]] {
            let p = parse_permissions(&args(none)).unwrap();
            assert_eq!(p.print_level(), PrintLevel::None);
        }
        // Chaque option ne coupe que son bit : --no-fill --no-assemble
        // retirent les bits 9 et 11 et rien d'autre, --no-annotate le 6.
        let p = parse_permissions(&args(&["--no-fill", "--no-annotate", "--no-assemble"])).unwrap();
        let bits = |p: Permissions| u32::from_le_bytes(p.to_p().to_le_bytes());
        assert_eq!(
            bits(Permissions::all()) ^ bits(p),
            (1 << 5) | (1 << 8) | (1 << 10)
        );
        // Commenter comprend remplir : --no-fill seul ne coupe rien tant
        // que les commentaires restent permis.
        let p = parse_permissions(&args(&["--no-fill"])).unwrap();
        assert!(p.fill_forms && p.annotate);
        assert!(parse_permissions(&args(&["--print", "moyen"])).is_err());
        let p = parse_permissions(&args(&["--no-copy"])).unwrap();
        assert!(!p.copy && p.accessibility, "l'accessibilité reste");
        let p = parse_permissions(&args(&["--no-modify"])).unwrap();
        assert!(!p.modify && p.assemble, "l'assemblage reste");
        assert!(parse_permissions(&args(&[])).unwrap().is_all());
    }

    #[test]
    fn render_tourne_par_quarts_de_tour() {
        assert_eq!(view_rotation_arg(&args(&[])).unwrap(), 0);
        assert_eq!(view_rotation_arg(&args(&["--rotate", "90"])).unwrap(), 90);
        assert_eq!(view_rotation_arg(&args(&["--rotate", "-90"])).unwrap(), -90);
        assert_eq!(view_rotation_arg(&args(&["--rotate", "540"])).unwrap(), 540);
        assert!(view_rotation_arg(&args(&["--rotate", "45"])).is_err());
        assert!(view_rotation_arg(&args(&["--rotate", "droite"])).is_err());
    }
}
