//! Édition des liens d'une page (`/Link`, §12.5.6.5) : pose, retrait et
//! détection automatique des adresses dans le texte — le panneau *Liens*
//! d'Acrobat et sa commande « Créer des liens à partir des URL ».
//!
//! La lecture est du ressort de [`crate::navigation::page_links`] ; ce
//! module écrit.
//!
//! ## Un lien ne se voit pas
//!
//! Les annotations posées ici portent `/Border [0 0 0]` et **pas** de
//! couleur `/C` : elles ne dessinent rien. C'est ce que fait Acrobat par
//! défaut depuis longtemps, et c'est ce qui permet de vérifier qu'un
//! `autolink` n'a **pas** changé un seul pixel du document.

use acrux_core::{Error, Rect, Result};
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};

use crate::annotations::encode_text;
use crate::navigation::{
    action_object, destination_object, page_links, Action, Destination, PageIndex, View,
};
use crate::text::{extract_page_text, Line, PageText};

/// Ce vers quoi un lien conduit.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    /// Une adresse : `https://…`, `mailto:…`, `ftp://…`.
    Uri(String),
    /// Une page du document, avec son cadrage.
    Page(Destination),
    /// Un autre document (`GoToR`), à la page indiquée (0 = première).
    File {
        /// Chemin du fichier, relatif de préférence.
        path: String,
        /// Page à ouvrir dans ce fichier.
        page: usize,
    },
    /// Une action prédéfinie du lecteur : `NextPage`, `PrevPage`,
    /// `FirstPage`, `LastPage`, `GoBack`, `Print`…
    Named(String),
}

impl LinkTarget {
    /// Lit une cible écrite en une seule chaîne, comme sur la ligne de
    /// commande : `https://…`, `mailto:…`, `page:12`, `fichier.pdf#7`,
    /// `nommee:NextPage`.
    ///
    /// # Errors
    /// Cible vide ou numéro de page illisible.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Corrupt("cible de lien vide".into()));
        }
        if let Some(rest) = text.strip_prefix("page:") {
            let number: usize = rest
                .trim()
                .parse()
                .map_err(|_| Error::Corrupt(format!("« {rest} » n'est pas un numéro de page")))?;
            if number == 0 {
                return Err(Error::Corrupt("les pages se comptent à partir de 1".into()));
            }
            return Ok(LinkTarget::Page(Destination {
                page: number - 1,
                view: View::Fit,
            }));
        }
        if let Some(rest) = text.strip_prefix("nommee:") {
            return Ok(LinkTarget::Named(rest.trim().to_string()));
        }
        if text.contains("://") || text.starts_with("mailto:") {
            return Ok(LinkTarget::Uri(text.to_string()));
        }
        // « fichier.pdf » ou « fichier.pdf#7 ».
        let (path, page) = match text.rsplit_once('#') {
            Some((p, n)) => (
                p,
                n.trim()
                    .parse::<usize>()
                    .map_err(|_| Error::Corrupt(format!("« {n} » n'est pas un numéro de page")))?
                    .saturating_sub(1),
            ),
            None => (text, 0),
        };
        Ok(LinkTarget::File {
            path: path.to_string(),
            page,
        })
    }
}

/// Dictionnaire d'action pour une cible.
fn target_action(pages: &[Page], target: &LinkTarget) -> Result<Object> {
    match target {
        LinkTarget::Uri(uri) => action_object(pages, &Action::Uri(uri.clone()))
            .ok_or_else(|| Error::Corrupt("adresse illisible".into())),
        LinkTarget::Named(name) => action_object(pages, &Action::Named(name.clone()))
            .ok_or_else(|| Error::Corrupt("action nommée illisible".into())),
        LinkTarget::Page(destination) => {
            let dest = destination_object(pages, destination).ok_or_else(|| {
                Error::Corrupt(format!("page {} inexistante", destination.page + 1))
            })?;
            let mut d = Dict::new();
            d.insert(Name::new("S"), Object::Name(Name::new("GoTo")));
            d.insert(Name::new("D"), dest);
            Ok(Object::Dict(d))
        }
        LinkTarget::File { path, page } => {
            let mut d = Dict::new();
            d.insert(Name::new("S"), Object::Name(Name::new("GoToR")));
            d.insert(Name::new("F"), Object::String(encode_text(path)));
            // La page d'un document distant se désigne par son **numéro** :
            // on n'a pas ses objets sous la main (§12.3.2.2).
            d.insert(
                Name::new("D"),
                Object::Array(vec![
                    Object::Integer(i64::try_from(*page).unwrap_or(0)),
                    Object::Name(Name::new("Fit")),
                ]),
            );
            // Ouvrir dans la même fenêtre, comme le fait Acrobat.
            d.insert(Name::new("NewWindow"), Object::Bool(false));
            Ok(Object::Dict(d))
        }
    }
}

/// Pose une annotation `/Link` sur une page et rend sa référence.
///
/// # Errors
/// Page directe (non indirecte), rectangle vide, ou cible invalide.
pub fn add_link(doc: &Document, page: &Page, rect: Rect, target: &LinkTarget) -> Result<ObjectRef> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return Err(Error::Corrupt(format!(
            "rectangle de lien vide : [{:.1} {:.1} {:.1} {:.1}]",
            rect.x0, rect.y0, rect.x1, rect.y1
        )));
    }
    let pages = collect_pages(doc)?;
    let action = target_action(&pages, target)?;
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Link")));
    d.insert(
        Name::new("Rect"),
        Object::Array(vec![
            Object::Real(rect.x0.min(rect.x1)),
            Object::Real(rect.y0.min(rect.y1)),
            Object::Real(rect.x0.max(rect.x1)),
            Object::Real(rect.y0.max(rect.y1)),
        ]),
    );
    // Épaisseur nulle : le lien est actif mais invisible.
    d.insert(
        Name::new("Border"),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(0),
        ]),
    );
    // /H /I : au clic, le lecteur inverse la zone plutôt que d'y dessiner.
    d.insert(Name::new("H"), Object::Name(Name::new("I")));
    d.insert(Name::new("A"), action);
    let annot_ref = doc.add(Object::Dict(d));

    // Le dictionnaire de page est relu dans le document et non repris de
    // `page` : deux poses successives sur la même page doivent s'ajouter
    // l'une à l'autre, alors que `page.dict` est une copie figée à la
    // lecture, qui ignorerait la première.
    let mut page_dict = doc
        .get(page_ref)
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_else(|| page.dict.clone());
    let mut annots = match page_dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    annots.push(Object::Reference(annot_ref));
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(annot_ref)
}

/// Retire un lien désigné par sa position dans `/Annots`
/// ([`crate::navigation::Link::annotation_index`]).
///
/// # Errors
/// Index invalide, ou annotation qui n'est pas un lien.
pub fn remove_link(doc: &Document, page: &Page, annotation_index: usize) -> Result<()> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    // Comme pour la pose : le dictionnaire est relu dans le document, afin
    // que plusieurs retraits de suite voient le résultat des précédents.
    let mut page_dict = doc
        .get(page_ref)
        .ok()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_else(|| page.dict.clone());
    let mut annots = match page_dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)?
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let entry = annots.get(annotation_index).ok_or_else(|| {
        Error::Corrupt(format!("annotation {} inexistante", annotation_index + 1))
    })?;
    let is_link = doc
        .resolve(entry)
        .ok()
        .and_then(|o| {
            o.as_dict()
                .and_then(|d| d.get(&Name::new("Subtype")).cloned())
        })
        .and_then(|s| s.as_name().cloned())
        .is_some_and(|n| n.0 == b"Link");
    if !is_link {
        return Err(Error::Corrupt(format!(
            "l'annotation {} n'est pas un lien",
            annotation_index + 1
        )));
    }
    if let Object::Reference(r) = annots.remove(annotation_index) {
        doc.delete(r);
    }
    page_dict.insert(Name::new("Annots"), Object::Array(annots));
    doc.set(page_ref, Object::Dict(page_dict));
    Ok(())
}

// ---------------------------------------------------------------------------
// Détection des adresses
// ---------------------------------------------------------------------------

/// Une adresse repérée dans le texte d'une page.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundUri {
    /// Page (0 = première).
    pub page: usize,
    /// Adresse normalisée, telle qu'elle sera écrite dans l'action.
    pub uri: String,
    /// Rectangles couverts : un par ligne quand l'adresse est coupée.
    pub rects: Vec<Rect>,
    /// Vrai si l'adresse a été recollée d'une ligne à la suivante.
    pub joined: bool,
}

/// Bilan d'un [`autolink`].
#[derive(Debug, Clone, Default)]
pub struct AutolinkReport {
    /// Adresses reconnues, dans l'ordre des pages.
    pub found: Vec<FoundUri>,
    /// Annotations `/Link` réellement posées.
    pub added: usize,
    /// Adresses laissées de côté parce qu'un lien les couvrait déjà.
    pub already_linked: usize,
    /// Remarques (adresses vraisemblablement coupées sans recollage sûr).
    pub warnings: Vec<String>,
}

/// Détecte les adresses du texte et pose les annotations `/Link`
/// correspondantes, sur le rectangle exact des mots concernés.
///
/// Rien n'est dessiné : le rendu du document est inchangé au pixel près.
/// Une adresse déjà couverte par un lien existant est laissée tranquille,
/// de sorte que la commande peut être relancée sans créer de doublons.
///
/// # Errors
/// Pages illisibles, ou écriture impossible.
pub fn autolink(doc: &Document) -> Result<AutolinkReport> {
    let pages = collect_pages(doc)?;
    let index = PageIndex::new(&pages);
    let mut report = AutolinkReport::default();
    for page in &pages {
        let Ok(text) = extract_page_text(doc, page) else {
            continue;
        };
        let existing: Vec<Rect> = page_links(doc, page, &index)?
            .into_iter()
            .map(|l| l.rect)
            .collect();
        let mut found = find_uris_in_page(&text);
        for uri in &mut found {
            uri.page = page.index;
        }
        for uri in found {
            if uri
                .rects
                .iter()
                .any(|r| existing.iter().any(|e| covers(e, r)))
            {
                report.already_linked += 1;
                report.found.push(uri);
                continue;
            }
            for rect in &uri.rects {
                add_link(doc, page, *rect, &LinkTarget::Uri(uri.uri.clone()))?;
                report.added += 1;
            }
            if uri.joined {
                report.warnings.push(format!(
                    "page {} : adresse recollée d'une ligne à la suivante, à vérifier — {}",
                    page.index + 1,
                    uri.uri
                ));
            }
            report.found.push(uri);
        }
    }
    Ok(report)
}

/// Vrai si `outer` couvre `inner` à un point près (les rectangles d'un lien
/// existant ne coïncident jamais au centième avec ceux du texte).
fn covers(outer: &Rect, inner: &Rect) -> bool {
    let margin = 1.0;
    outer.x0 <= inner.x0 + margin
        && outer.y0 <= inner.y0 + margin
        && outer.x1 >= inner.x1 - margin
        && outer.y1 >= inner.y1 - margin
}

/// Un caractère du texte d'une ligne, avec la boîte du glyphe qui le porte.
struct Placed {
    /// Position du caractère dans le texte reconstruit de la ligne.
    offset: usize,
    /// Boîte du glyphe ; absente pour l'espace inséré entre deux mots.
    bbox: Option<Rect>,
}

/// Texte d'une ligne et position de chacun de ses caractères.
fn place_line(line: &Line) -> (String, Vec<Placed>) {
    let mut text = String::new();
    let mut placed = Vec::new();
    for (i, word) in line.words.iter().enumerate() {
        if i > 0 {
            placed.push(Placed {
                offset: text.len(),
                bbox: None,
            });
            text.push(' ');
        }
        for glyph in &word.glyphs {
            for c in glyph.text.chars() {
                placed.push(Placed {
                    offset: text.len(),
                    bbox: Some(glyph.bbox),
                });
                text.push(c);
            }
        }
        // Un mot sans glyphes (texte reconstruit) : sa boîte sert pour tout.
        if word.glyphs.is_empty() {
            for c in word.text.chars() {
                placed.push(Placed {
                    offset: text.len(),
                    bbox: Some(word.bbox),
                });
                text.push(c);
            }
        }
    }
    (text, placed)
}

/// Rectangle couvrant les caractères de l'intervalle d'octets donné.
fn rect_of(placed: &[Placed], range: (usize, usize)) -> Option<Rect> {
    let mut out: Option<Rect> = None;
    for p in placed {
        if p.offset < range.0 || p.offset >= range.1 {
            continue;
        }
        if let Some(b) = p.bbox {
            out = Some(match out {
                Some(r) => r.union(&b),
                None => b,
            });
        }
    }
    out.filter(|r| r.width() > 0.0 && r.height() > 0.0)
}

/// Adresses d'une page, avec leurs rectangles.
fn find_uris_in_page(text: &PageText) -> Vec<FoundUri> {
    let placed: Vec<(String, Vec<Placed>)> = text.lines.iter().map(place_line).collect();
    let mut out = Vec::new();
    // Caractères déjà consommés comme suite d'une adresse de la ligne
    // précédente : ils ne doivent pas être analysés une seconde fois.
    let mut consumed: Vec<usize> = vec![0; placed.len()];
    for (i, (line_text, line_placed)) in placed.iter().enumerate() {
        for (start, end, uri) in find_uris(line_text) {
            if start < consumed[i] {
                continue;
            }
            let Some(rect) = rect_of(line_placed, (start, end)) else {
                continue;
            };
            let mut rects = vec![rect];
            let mut joined = false;
            let mut uri = uri;
            // Adresse coupée en fin de ligne : voir la règle en tête de
            // `continuation_of`.
            if end == line_text.len() {
                if let Some((next_text, next_placed)) = placed.get(i + 1) {
                    if lines_follow(&text.lines[i], &text.lines[i + 1]) {
                        if let Some(rest) = continuation_of(&uri, next_text) {
                            if let Some(next_rect) = rect_of(next_placed, (0, rest.len())) {
                                uri.push_str(&rest);
                                rects.push(next_rect);
                                consumed[i + 1] = rest.len();
                                joined = true;
                            }
                        }
                    }
                }
            }
            out.push(FoundUri {
                page: 0,
                uri,
                rects,
                joined,
            });
        }
    }
    out
}

/// Vrai si `next` est vraisemblablement la ligne suivante de `line` :
/// en dessous, de taille voisine, et horizontalement recouvrante.
fn lines_follow(line: &Line, next: &Line) -> bool {
    let height = line.bbox.height().max(1.0);
    next.bbox.y1 <= line.bbox.y1
        && line.bbox.y0 - next.bbox.y1 < height * 1.5
        && next.bbox.x0 < line.bbox.x1
        && next.bbox.x1 > line.bbox.x0
}

/// Début de `next_line` à recoller à une adresse coupée, s'il y a lieu.
///
/// **Le choix retenu**, et il est volontairement prudent : on ne recolle que
/// si l'adresse tronquée se termine par `/`. Aucune phrase française ne se
/// termine par une barre oblique, alors qu'un chemin d'URL coupé là le fait
/// tout le temps ; et le caractère suivant n'a pas été ajouté par la
/// composition, contrairement au trait d'union d'une césure, dont on ne
/// saurait jamais dire s'il appartient à l'adresse ou au typographe.
///
/// Le morceau repris doit en outre être un mot entier fait de caractères
/// autorisés dans un chemin et porter au moins un séparateur
/// (`/`, `.`, `-`, `_`, `%`, `=`, `?`, `#`) : « Introduction » en début de
/// ligne suivante n'est pas une suite d'adresse, `partie-2/index.html` si.
///
/// Quand la règle ne s'applique pas, l'adresse est liée sur sa seule
/// première ligne : on préfère un lien tronqué — visible, corrigeable — à
/// un lien faux qui mènerait ailleurs sans prévenir.
fn continuation_of(uri: &str, next_line: &str) -> Option<String> {
    if !uri.ends_with('/') {
        return None;
    }
    let token: String = next_line
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    if token.is_empty() || token.len() > 200 {
        return None;
    }
    if !token.chars().all(is_path_char) {
        return None;
    }
    if !token.contains(['/', '.', '-', '_', '%', '=', '?', '#']) {
        return None;
    }
    Some(token)
}

/// Caractère admis dans un chemin d'URL (RFC 3986, jeu restreint).
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~%/?#=&+:@!$'*,;()[]".contains(c)
}

/// Caractère admis dans une adresse en cours de lecture.
fn is_uri_char(c: char) -> bool {
    is_path_char(c)
}

/// Amorces reconnues dans le texte courant.
const PREFIXES: [&str; 4] = ["https://", "http://", "mailto:", "www."];

/// Repère les adresses d'un texte et rend `(début, fin, adresse normalisée)`
/// en octets.
///
/// Reconnaît `http://`, `https://`, `mailto:` et les adresses nues en
/// `www.` (alors préfixées de `https://`). La ponctuation finale d'une
/// phrase est exclue, de même qu'une parenthèse fermante non appariée.
#[must_use]
pub fn find_uris(text: &str) -> Vec<(usize, usize, String)> {
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    'scan: while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &text[i..];
        for prefix in PREFIXES {
            if rest.len() < prefix.len()
                || !rest.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
            {
                continue;
            }
            // L'amorce doit commencer un mot : « nowww.chose.fr » n'en est pas.
            let before = text[..i].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '@' || c == '/') {
                continue;
            }
            let mut end = i;
            for (offset, c) in rest.char_indices() {
                if !is_uri_char(c) {
                    break;
                }
                end = i + offset + c.len_utf8();
            }
            let candidate = trim_trailing(&text[i..end]);
            let end = i + candidate.len();
            if let Some(uri) = normalize(candidate, prefix == "www.") {
                out.push((i, end, uri));
                i = end;
                continue 'scan;
            }
            // Amorce trompeuse (« www. » seul) : on reprend après elle.
            i += prefix.len();
            continue 'scan;
        }
        // Avance d'un caractère complet.
        i += 1;
        while i < bytes.len() && !text.is_char_boundary(i) {
            i += 1;
        }
    }
    out
}

/// Retire la ponctuation de fin de phrase et les fermantes non appariées.
fn trim_trailing(candidate: &str) -> &str {
    let mut end = candidate.len();
    loop {
        let trimmed = &candidate[..end];
        let Some(last) = trimmed.chars().next_back() else {
            return trimmed;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' => true,
            ')' => trimmed.matches('(').count() < trimmed.matches(')').count(),
            ']' => trimmed.matches('[').count() < trimmed.matches(']').count(),
            _ => false,
        };
        if !drop {
            return trimmed;
        }
        end -= last.len_utf8();
    }
}

/// Valide une adresse candidate et la normalise, ou la rejette.
///
/// `bare` distingue une adresse écrite sans protocole (`www.…`), pour
/// laquelle on exige une étiquette de plus : `www.fr` dans une phrase est
/// bien plus souvent une abréviation qu'un site, alors que
/// `https://exemple.fr` ne laisse aucun doute.
fn normalize(candidate: &str, bare: bool) -> Option<String> {
    let lower = candidate.to_ascii_lowercase();
    if let Some(address) = lower.strip_prefix("mailto:") {
        return is_mail(address).then(|| format!("mailto:{}", &candidate["mailto:".len()..]));
    }
    let (scheme, rest) = match lower.strip_prefix("https://") {
        Some(rest) => ("https://", rest),
        None => match lower.strip_prefix("http://") {
            Some(rest) => ("http://", rest),
            // Adresse nue : on la préfixe en https, qui est aujourd'hui le
            // choix sûr, et non en http comme le faisaient les vieux outils.
            None => ("https://", lower.as_str()),
        },
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    if !is_host(host) {
        return None;
    }
    if bare && host.split('.').count() < 3 {
        return None;
    }
    if candidate.to_ascii_lowercase().starts_with(scheme) {
        Some(candidate.to_string())
    } else {
        Some(format!("{scheme}{candidate}"))
    }
}

/// Nom d'hôte plausible : au moins deux étiquettes et un domaine de tête
/// alphabétique d'au moins deux lettres (ou `localhost`).
fn is_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    if !labels
        .iter()
        .all(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return false;
    }
    let tld = labels[labels.len() - 1];
    tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// Adresse électronique plausible.
fn is_mail(address: &str) -> bool {
    let Some((user, host)) = address.split_once('@') else {
        return false;
    };
    !user.is_empty() && is_host(host)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::docinfo::tests::doc_with;

    fn uris(text: &str) -> Vec<String> {
        find_uris(text).into_iter().map(|(_, _, u)| u).collect()
    }

    #[test]
    fn an_address_at_the_end_of_a_sentence_loses_its_full_stop() {
        assert_eq!(
            uris("Voir https://exemple.test/page."),
            ["https://exemple.test/page"]
        );
        assert_eq!(
            find_uris("Voir https://exemple.test/page.")[0].1,
            "Voir https://exemple.test/page".len()
        );
        assert_eq!(
            uris("Écrire à mailto:jean@exemple.test !"),
            ["mailto:jean@exemple.test"]
        );
    }

    #[test]
    fn an_address_between_brackets_keeps_its_own_parentheses() {
        assert_eq!(
            uris("(voir https://exemple.test/a)"),
            ["https://exemple.test/a"]
        );
        // La parenthèse appariée appartient bien à l'adresse.
        assert_eq!(
            uris("https://exemple.test/wiki/Test_(page)"),
            ["https://exemple.test/wiki/Test_(page)"]
        );
        assert_eq!(uris("[https://exemple.test/b]"), ["https://exemple.test/b"]);
    }

    #[test]
    fn a_bare_www_address_is_completed_in_https() {
        assert_eq!(
            uris("Visitez www.exemple.test dès demain"),
            ["https://www.exemple.test"]
        );
        assert_eq!(uris("WWW.Exemple.Test"), ["https://WWW.Exemple.Test"]);
    }

    #[test]
    fn www_alone_is_not_an_address() {
        assert!(uris("le www est ancien").is_empty());
        assert!(uris("www. quelque chose").is_empty());
        assert!(uris("www.fr").is_empty(), "une seule étiquette après www");
        assert!(uris("fichier.pdf").is_empty());
        assert!(uris("version 1.2.3").is_empty());
        assert!(uris("nowww.exemple.test").is_empty());
    }

    #[test]
    fn a_cut_address_is_joined_only_after_a_slash() {
        assert_eq!(
            continuation_of("https://exemple.test/", "partie-2/index.html suite"),
            Some("partie-2/index.html".to_string())
        );
        // Un mot ordinaire ne prolonge rien.
        assert_eq!(
            continuation_of("https://exemple.test/", "Introduction"),
            None
        );
        // Sans barre oblique finale, on ne recolle pas : le trait d'union
        // peut venir de la césure comme de l'adresse.
        assert_eq!(
            continuation_of("https://exemple.test/tres-long-", "chemin/page.html"),
            None
        );
    }

    #[test]
    fn several_addresses_on_one_line() {
        assert_eq!(
            uris("a https://un.test, b http://deux.test/x et www.trois.test."),
            [
                "https://un.test",
                "http://deux.test/x",
                "https://www.trois.test"
            ]
        );
    }

    #[test]
    fn links_are_written_and_read_back() {
        let doc = doc_with("", &[], "");
        let pages = collect_pages(&doc).unwrap();
        add_link(
            &doc,
            &pages[0],
            Rect::new(10.0, 20.0, 90.0, 35.0),
            &LinkTarget::Uri("https://exemple.test/".into()),
        )
        .unwrap();
        add_link(
            &doc,
            &collect_pages(&doc).unwrap()[0],
            Rect::new(10.0, 50.0, 90.0, 65.0),
            &LinkTarget::Page(Destination {
                page: 2,
                view: View::Fit,
            }),
        )
        .unwrap();
        let bytes = doc.save_incremental().or_else(|_| doc.save_full()).unwrap();
        let saved = Document::from_bytes(bytes).unwrap();
        let pages = collect_pages(&saved).unwrap();
        let index = PageIndex::new(&pages);
        let links = page_links(&saved, &pages[0], &index).unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].action, Action::Uri("https://exemple.test/".into()));
        assert_eq!(links[0].rect, Rect::new(10.0, 20.0, 90.0, 35.0));
        assert_eq!(
            links[1].action,
            Action::GoTo(Destination {
                page: 2,
                view: View::Fit
            })
        );
    }

    #[test]
    fn a_link_can_be_removed_and_a_note_cannot_be_taken_for_one() {
        let doc = doc_with("", &[], "");
        let pages = collect_pages(&doc).unwrap();
        add_link(
            &doc,
            &pages[0],
            Rect::new(10.0, 20.0, 90.0, 35.0),
            &LinkTarget::Named("NextPage".into()),
        )
        .unwrap();
        let pages = collect_pages(&doc).unwrap();
        assert!(remove_link(&doc, &pages[0], 5).is_err());
        remove_link(&doc, &pages[0], 0).unwrap();
        let pages = collect_pages(&doc).unwrap();
        let index = PageIndex::new(&pages);
        assert!(page_links(&doc, &pages[0], &index).unwrap().is_empty());
    }

    /// Écrit `tests/corpus/synthese/autolink-adresses-dans-le-texte.pdf` :
    /// `cargo test -p acrux-features --lib -- --ignored generate_autolink_corpus`.
    ///
    /// Une page qui réunit les cas qui font trébucher un détecteur
    /// d'adresses : adresse finissant une phrase, adresse entre
    /// parenthèses, adresse **coupée par un retour à la ligne**, adresse
    /// électronique, adresse nue en `www.`, et deux faux amis (« www » seul
    /// et un nom de fichier). Aucun lien n'est posé dans le fichier : c'est
    /// `acr autolink` qui doit les trouver.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    fn generate_autolink_corpus() {
        // Les parenthèses d'une chaîne PDF littérale sont échappées, les
        // accents écrits en octal WinAnsi (§7.9.2.2).
        let lines = [
            (16.0, "F1", 214.0, "Adresses dans le texte"),
            (
                11.0,
                "F2",
                186.0,
                "Le site officiel est https://exemple.test/documentation.",
            ),
            (
                11.0,
                "F2",
                168.0,
                r"Voir aussi \(https://exemple.test/annexe\) pour le d\351tail.",
            ),
            (
                11.0,
                "F2",
                140.0,
                r"La page compl\350te se trouve \340 https://exemple.test/rapports/",
            ),
            (
                11.0,
                "F2",
                124.0,
                "2024/synthese-finale.html et rien d'autre.",
            ),
            (
                11.0,
                "F2",
                96.0,
                r"\311crire \340 mailto:contact@exemple.test ou visiter www.exemple.test.",
            ),
            (
                11.0,
                "F2",
                68.0,
                "Le www est ancien ; ce fichier.pdf n'est pas une adresse.",
            ),
        ];
        let mut content = String::from("0.12 0.14 0.20 rg\n");
        for (size, font, y, text) in lines {
            let _ = std::fmt::Write::write_fmt(
                &mut content,
                format_args!("BT /{font} {size} Tf 40 {y} Td ({text}) Tj ET\n"),
            );
        }
        content.push_str("0.55 0.57 0.62 RG 1 w 40 206 m 380 206 l S\n");
        let objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (
                2,
                "<< /Type /Pages /Kids [5 0 R] /Count 1 >>".to_string(),
            ),
            (
                3,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                    .to_string(),
            ),
            (
                4,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                    .to_string(),
            ),
            (
                5,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 240] /Contents 6 0 R \
                 /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> >>"
                    .to_string(),
            ),
            (
                6,
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                ),
            ),
        ];
        let doc = Document::from_bytes(crate::docinfo::tests::build_pdf(&objects, "")).unwrap();
        let saved = doc.save_full().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/autolink-adresses-dans-le-texte.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }

    #[test]
    fn targets_are_parsed_from_one_string() {
        assert_eq!(
            LinkTarget::parse("https://exemple.test").unwrap(),
            LinkTarget::Uri("https://exemple.test".into())
        );
        assert_eq!(
            LinkTarget::parse("page:3").unwrap(),
            LinkTarget::Page(Destination {
                page: 2,
                view: View::Fit
            })
        );
        assert_eq!(
            LinkTarget::parse("nommee:LastPage").unwrap(),
            LinkTarget::Named("LastPage".into())
        );
        assert_eq!(
            LinkTarget::parse("annexe.pdf#4").unwrap(),
            LinkTarget::File {
                path: "annexe.pdf".into(),
                page: 3
            }
        );
        assert!(LinkTarget::parse("  ").is_err());
        assert!(LinkTarget::parse("page:0").is_err());
    }
}
