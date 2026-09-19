//! Biffure définitive (ISO 32000-2 §12.5.6.23) et nettoyage des données
//! cachées.
//!
//! Trois temps, comme Acrobat :
//!
//! 1. **Marquer** ([`mark_redactions`]) : des annotations `/Subtype /Redact`
//!    sont posées sur les zones ; rien n'est encore détruit, le fichier
//!    montre un cadre rouge et peut être relu, corrigé, annulé.
//! 2. **Chercher** ([`find_redaction_candidates`]) : repère le texte à
//!    biffer par motif (adresse électronique, IBAN, carte bancaire…) et
//!    produit les marques correspondantes.
//! 3. **Appliquer** ([`apply_redactions`]) : le contenu est **réécrit**.
//!    Les codes des glyphes couverts disparaissent du flux, les pixels
//!    couverts des images sont noircis, les tracés entièrement couverts
//!    sont retirés, les annotations qui touchent une zone sont supprimées,
//!    puis le rectangle de remplissage et le texte de remplacement sont
//!    dessinés. Aucun « rectangle noir posé par-dessus » : après
//!    `save_full_with(SaveOptions { drop_unreferenced: true, .. })` le texte
//!    biffé n'existe plus nulle part dans le fichier.
//!
//! [`sanitize`] complète la biffure en retirant ce qui ne se voit pas :
//! métadonnées, pièces jointes, JavaScript, calques désactivés, objets
//! inatteignables, commentaires, formulaires, texte invisible.
//!
//! ## Ce qui est garanti et ce qui ne l'est pas
//!
//! - Garanti : les codes de glyphes couverts ne sont plus dans le flux de
//!   contenu (page ou XObject de formulaire, récursivement), les pixels
//!   couverts des images XObject et en ligne valent zéro, les objets
//!   devenus inatteignables sont supprimés.
//! - Non garanti : une police à sous-ensemble conserve le dessin des
//!   glyphes retirés (le programme de police n'est pas réécrit ; il ne dit
//!   pas où ils se trouvaient) ; un texte affiché par une police que nous
//!   ne savons pas décoder est signalé dans `warnings` et laissé en place ;
//!   un tracé partiellement couvert est conservé et recouvert.

// Réécriture de flux : beaucoup de conversions bornées (indices de pixels,
// codes de glyphes) et quelques fonctions longues qui suivent, opérateur par
// opérateur, la grammaire du contenu — les découper nuirait à la lecture.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;

use acrux_core::{Error, Matrix, Point, Rect, Result};
use acrux_document::writer::write_object;
use acrux_document::{collect_pages, Dict, Document, Name, Object, ObjectRef, Page};
use acrux_render::content::{ContentLexer, Operation};
use acrux_render::font::LoadedFont;
use acrux_render::image::{decode_image, DecodedImage};
use acrux_render::page::page_content;

/// Couleur RVB, composantes dans [0, 1].
pub type Rgb = [f64; 3];

/// Budget d'opérateurs par page (fichiers hostiles).
const MAX_OPS: usize = 2_000_000;
/// Profondeur maximale de récursion dans les XObjects de formulaire.
const MAX_DEPTH: usize = 8;
/// Part de la boîte d'un glyphe qui doit être couverte pour le retirer.
/// Un glyphe simplement effleuré par le bord d'une zone est conservé.
const MIN_GLYPH_COVERAGE: f64 = 0.20;
/// Nombre maximal d'avertissements conservés dans un rapport.
const MAX_WARNINGS: usize = 64;

// ---------------------------------------------------------------------------
// 1. Marquage
// ---------------------------------------------------------------------------

/// Zone à biffer.
#[derive(Debug, Clone)]
pub struct RedactionMark {
    /// Index de la page (0 = première).
    pub page: usize,
    /// Zone en espace utilisateur par défaut de la page (points, origine en bas à gauche).
    pub rect: Rect,
    /// Couleur de remplissage après application (`None` : noir).
    pub fill: Option<Rgb>,
    /// Texte de remplacement dessiné dans la zone (`None` : aucun).
    pub overlay_text: Option<String>,
}

impl RedactionMark {
    /// Marque noire sans texte de remplacement.
    #[must_use]
    pub fn new(page: usize, rect: Rect) -> Self {
        Self {
            page,
            rect,
            fill: None,
            overlay_text: None,
        }
    }

    /// Couleur de remplissage effective (noir par défaut).
    #[must_use]
    pub fn fill_color(&self) -> Rgb {
        self.fill.unwrap_or([0.0, 0.0, 0.0])
    }
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn rect_object(r: Rect) -> Object {
    Object::Array(vec![
        Object::Real(r.x0),
        Object::Real(r.y0),
        Object::Real(r.x1),
        Object::Real(r.y1),
    ])
}

/// `/QuadPoints` d'un rectangle (§12.5.6.10 : haut-gauche, haut-droit,
/// bas-gauche, bas-droit).
fn quad_points(r: Rect) -> Object {
    Object::Array(
        [r.x0, r.y1, r.x1, r.y1, r.x0, r.y0, r.x1, r.y0]
            .iter()
            .map(|v| Object::Real(*v))
            .collect(),
    )
}

/// Flux de formulaire (`/Subtype /Form`) prêt à être ajouté au document.
fn form_stream(bbox: Rect, content: String, resources: Option<Dict>) -> Object {
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Form")));
    d.insert(Name::new("BBox"), rect_object(bbox));
    if let Some(res) = resources {
        d.insert(Name::new("Resources"), Object::Dict(res));
    }
    let raw = content.into_bytes();
    d.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
    );
    Object::Stream { dict: d, raw }
}

/// Apparence de prévisualisation : cadre rouge fin, comme Acrobat tant que la
/// biffure n'est pas appliquée.
fn preview_appearance(rect: Rect) -> Object {
    let content = format!(
        "1 0 0 RG 1 w {} {} {} {} re S",
        fmt(rect.x0 + 0.5),
        fmt(rect.y0 + 0.5),
        fmt((rect.width() - 1.0).max(0.0)),
        fmt((rect.height() - 1.0).max(0.0))
    );
    form_stream(rect, content, None)
}

/// Dictionnaire de police Helvetica WinAnsi (texte de remplacement).
fn helvetica_dict() -> Object {
    let mut f = Dict::new();
    f.insert(Name::new("Type"), Object::Name(Name::new("Font")));
    f.insert(Name::new("Subtype"), Object::Name(Name::new("Type1")));
    f.insert(Name::new("BaseFont"), Object::Name(Name::new("Helvetica")));
    f.insert(
        Name::new("Encoding"),
        Object::Name(Name::new("WinAnsiEncoding")),
    );
    Object::Dict(f)
}

/// Apparence de remplacement (`/RO`, §12.5.6.23) : le rectangle plein et,
/// éventuellement, le texte de remplacement centré.
fn overlay_appearance(doc: &Document, rect: Rect, fill: Rgb, text: Option<&str>) -> Object {
    let mut content = format!(
        "{} {} {} rg {} {} {} {} re f\n",
        fmt(fill[0]),
        fmt(fill[1]),
        fmt(fill[2]),
        fmt(rect.x0),
        fmt(rect.y0),
        fmt(rect.width()),
        fmt(rect.height())
    );
    let mut resources = None;
    if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
        let font_ref = doc.add(helvetica_dict());
        let mut fonts = Dict::new();
        fonts.insert(Name::new("AkRdHelv"), Object::Reference(font_ref));
        let mut res = Dict::new();
        res.insert(Name::new("Font"), Object::Dict(fonts));
        resources = Some(res);
        content.push_str(&overlay_text_operators(rect, fill, text, "AkRdHelv"));
    }
    form_stream(rect, content, resources)
}

/// Opérateurs de texte du remplacement, centré dans la zone, à la plus
/// grande taille qui tienne (Helvetica, 12 pt au maximum).
fn overlay_text_operators(rect: Rect, fill: Rgb, text: &str, font: &str) -> String {
    let width = helvetica_width(text);
    if width <= 0.0 || rect.width() <= 2.0 || rect.height() <= 2.0 {
        return String::new();
    }
    let size = ((rect.width() - 2.0) / width)
        .min(rect.height() * 0.7)
        .min(12.0);
    if size < 2.0 {
        return String::new();
    }
    // Texte blanc sur fond sombre, noir sur fond clair.
    let luma = 0.299 * fill[0] + 0.587 * fill[1] + 0.114 * fill[2];
    let ink = if luma < 0.5 { 1.0 } else { 0.0 };
    let x = rect.x0 + (rect.width() - width * size) / 2.0;
    let y = rect.y0 + (rect.height() - size * 0.72) / 2.0 + size * 0.05;
    let mut encoded = Vec::new();
    write_object(&Object::String(win_ansi(text)), &mut encoded);
    format!(
        "BT /{font} {} Tf {ink} {ink} {ink} rg {} {} Td {} Tj ET\n",
        fmt(size),
        fmt(x),
        fmt(y),
        String::from_utf8_lossy(&encoded)
    )
}

/// Crée les annotations `/Redact` correspondant aux marques.
///
/// Les annotations portent `/QuadPoints` (la zone), `/IC` (couleur de
/// remplissage), `/OverlayText` et `/Q` (texte de remplacement centré),
/// `/RO` (apparence exacte qui sera peinte) et une apparence `/AP /N` de
/// prévisualisation en cadre rouge. Rien n'est détruit : c'est
/// [`apply_redactions`] qui applique.
///
/// # Errors
/// Pages illisibles, ou page qui n'est pas un objet indirect.
pub fn mark_redactions(doc: &Document, marks: &[RedactionMark]) -> Result<Vec<ObjectRef>> {
    let pages = collect_pages(doc)?;
    let mut created: Vec<(usize, ObjectRef)> = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let on_page: Vec<(usize, &RedactionMark)> = marks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.page == index)
            .collect();
        if on_page.is_empty() {
            continue;
        }
        let page_ref = page
            .reference
            .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
        let mut annots = match page.dict.get(&Name::new("Annots")) {
            Some(o) => doc
                .resolve(o)?
                .as_array()
                .map(<[Object]>::to_vec)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        for (order, mark) in on_page {
            let r = doc.add(Object::Dict(redact_annotation(doc, page_ref, mark)));
            annots.push(Object::Reference(r));
            created.push((order, r));
        }
        let mut dict = page.dict.clone();
        dict.insert(Name::new("Annots"), Object::Array(annots));
        doc.set(page_ref, Object::Dict(dict));
    }
    created.sort_by_key(|(order, _)| *order);
    Ok(created.into_iter().map(|(_, r)| r).collect())
}

fn redact_annotation(doc: &Document, page_ref: ObjectRef, mark: &RedactionMark) -> Dict {
    let fill = mark.fill_color();
    let mut d = Dict::new();
    d.insert(Name::new("Type"), Object::Name(Name::new("Annot")));
    d.insert(Name::new("Subtype"), Object::Name(Name::new("Redact")));
    d.insert(Name::new("P"), Object::Reference(page_ref));
    d.insert(Name::new("Rect"), rect_object(mark.rect));
    d.insert(Name::new("QuadPoints"), quad_points(mark.rect));
    d.insert(
        Name::new("IC"),
        Object::Array(fill.iter().map(|v| Object::Real(*v)).collect()),
    );
    // Couleur du cadre de prévisualisation (§12.5.6.23 : /C est la couleur du
    // contour affiché avant application).
    d.insert(
        Name::new("C"),
        Object::Array(vec![
            Object::Real(1.0),
            Object::Real(0.0),
            Object::Real(0.0),
        ]),
    );
    d.insert(Name::new("F"), Object::Integer(4)); // Print
    d.insert(
        Name::new("M"),
        Object::String(crate::annotations::pdf_date_now().into_bytes()),
    );
    if let Some(text) = mark.overlay_text.as_ref().filter(|t| !t.is_empty()) {
        d.insert(Name::new("OverlayText"), Object::String(win_ansi(text)));
        d.insert(Name::new("Q"), Object::Integer(1)); // centré
        d.insert(
            Name::new("DA"),
            Object::String(b"/AkRdHelv 0 Tf 1 1 1 rg".to_vec()),
        );
        d.insert(Name::new("Repeat"), Object::Bool(false));
    }
    let ro = doc.add(overlay_appearance(
        doc,
        mark.rect,
        fill,
        mark.overlay_text.as_deref(),
    ));
    d.insert(Name::new("RO"), Object::Reference(ro));
    let ap = doc.add(preview_appearance(mark.rect));
    let mut apd = Dict::new();
    apd.insert(Name::new("N"), Object::Reference(ap));
    d.insert(Name::new("AP"), Object::Dict(apd));
    d
}

// ---------------------------------------------------------------------------
// 2. Recherche par motif
// ---------------------------------------------------------------------------

/// Motif de recherche. Chaque motif prédéfini est reconnu par un petit
/// analyseur écrit à la main (pas de moteur d'expressions régulières), avec
/// la validation propre au format quand elle existe (Luhn, modulo 97).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Texte littéral, insensible à la casse.
    Literal(String),
    /// Adresse électronique `nom@domaine.tld`.
    Email,
    /// Numéro de téléphone français (`01 23 45 67 89`, `+33 1 23 45 67 89`).
    PhoneFr,
    /// IBAN, validé par le modulo 97 (ISO 13616).
    Iban,
    /// Numéro de carte bancaire de 13 à 19 chiffres, validé par la clé de Luhn.
    CreditCard,
    /// Numéro de sécurité sociale français (15 chiffres, clé validée).
    SocialSecurityFr,
    /// Date française (`31/12/2024`, `31-12-2024`, `31 décembre 2024`).
    DateFr,
    /// Adresse IPv4 (`192.168.0.1`).
    IpAddress,
}

impl Pattern {
    /// Motif désigné par son nom en ligne de commande.
    #[must_use]
    pub fn parse(name: &str) -> Option<Pattern> {
        match name.trim().to_ascii_lowercase().as_str() {
            "email" | "courriel" => Some(Pattern::Email),
            "telephone" | "téléphone" | "tel" => Some(Pattern::PhoneFr),
            "iban" => Some(Pattern::Iban),
            "carte" | "cb" => Some(Pattern::CreditCard),
            "securite-sociale" | "sécurité-sociale" | "nir" => Some(Pattern::SocialSecurityFr),
            "date" => Some(Pattern::DateFr),
            "ip" => Some(Pattern::IpAddress),
            _ => None,
        }
    }

    /// Nom lisible du motif.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Pattern::Literal(s) => format!("« {s} »"),
            Pattern::Email => "adresse électronique".into(),
            Pattern::PhoneFr => "téléphone".into(),
            Pattern::Iban => "IBAN".into(),
            Pattern::CreditCard => "carte bancaire".into(),
            Pattern::SocialSecurityFr => "sécurité sociale".into(),
            Pattern::DateFr => "date".into(),
            Pattern::IpAddress => "adresse IP".into(),
        }
    }

    /// Plages `[début, fin[` (en caractères) reconnues dans une ligne.
    #[must_use]
    pub fn scan(&self, chars: &[char]) -> Vec<(usize, usize)> {
        match self {
            Pattern::Literal(needle) => scan_literal(chars, needle),
            Pattern::Email => scan_email(chars),
            Pattern::PhoneFr => scan_phone_fr(chars),
            Pattern::Iban => scan_iban(chars),
            Pattern::CreditCard => scan_credit_card(chars),
            Pattern::SocialSecurityFr => scan_nir(chars),
            Pattern::DateFr => scan_date_fr(chars),
            Pattern::IpAddress => scan_ipv4(chars),
        }
    }
}

/// Cherche les occurrences des motifs dans tout le document et produit une
/// marque par occurrence (remplissage noir, sans texte de remplacement).
///
/// # Errors
/// Pages ou contenu illisibles.
pub fn find_redaction_candidates(
    doc: &Document,
    patterns: &[Pattern],
) -> Result<Vec<RedactionMark>> {
    let mut out = Vec::new();
    if patterns.is_empty() {
        return Ok(out);
    }
    for page in &collect_pages(doc)? {
        let text = crate::text::extract_page_text(doc, page)?;
        for line in &text.lines {
            let (chars, boxes) = line_characters(line);
            for pattern in patterns {
                for (start, end) in pattern.scan(&chars) {
                    let Some(first) = boxes.get(start) else {
                        continue;
                    };
                    let mut r = *first;
                    for b in boxes.iter().take(end).skip(start + 1) {
                        r = r.union(b);
                    }
                    // Petite marge : les boîtes de glyphes sont approchées.
                    let pad = (r.height() * 0.05).max(0.2);
                    out.push(RedactionMark::new(
                        page.index,
                        Rect::new(r.x0 - pad, r.y0 - pad, r.x1 + pad, r.y1 + pad),
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// Caractères d'une ligne et boîte de chacun (les espaces entre mots
/// reçoivent la boîte de l'intervalle).
fn line_characters(line: &crate::text::Line) -> (Vec<char>, Vec<Rect>) {
    let mut chars = Vec::new();
    let mut boxes = Vec::new();
    for (i, w) in line.words.iter().enumerate() {
        if i > 0 {
            let prev = line.words[i - 1].bbox;
            chars.push(' ');
            boxes.push(Rect::new(prev.x1, prev.y0, w.bbox.x0, prev.y1));
        }
        for g in &w.glyphs {
            for c in g.text.chars() {
                chars.push(c);
                boxes.push(g.bbox);
            }
        }
    }
    (chars, boxes)
}

fn scan_literal(chars: &[char], needle: &str) -> Vec<(usize, usize)> {
    let pat: Vec<char> = needle.to_lowercase().chars().collect();
    if pat.is_empty() || chars.len() < pat.len() {
        return Vec::new();
    }
    let lower: Vec<char> = chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + pat.len() <= lower.len() {
        if lower[i..i + pat.len()] == pat[..] {
            out.push((i, i + pat.len()));
            i += pat.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Caractère qui ne doit pas border une correspondance numérique.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

fn scan_email(chars: &[char]) -> Vec<(usize, usize)> {
    let local = |c: char| c.is_alphanumeric() || "._%+-'".contains(c);
    let domain = |c: char| c.is_alphanumeric() || c == '.' || c == '-';
    let mut out = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        if *c != '@' {
            continue;
        }
        let mut start = i;
        while start > 0 && local(chars[start - 1]) {
            start -= 1;
        }
        while start < i && !chars[start].is_alphanumeric() {
            start += 1;
        }
        if start == i {
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && domain(chars[end]) {
            end += 1;
        }
        while end > i + 1 && !chars[end - 1].is_alphanumeric() {
            end -= 1;
        }
        let host: String = chars[i + 1..end].iter().collect();
        let Some((name, tld)) = host.rsplit_once('.') else {
            continue;
        };
        if name.is_empty() || tld.len() < 2 || !tld.chars().all(char::is_alphabetic) {
            continue;
        }
        out.push((start, end));
    }
    out
}

/// Lit une suite de chiffres pouvant contenir des séparateurs isolés
/// (espace, point, tiret, barre oblique). Retourne les chiffres et la fin.
fn read_digits(chars: &[char], from: usize, separators: &str) -> (String, usize) {
    let mut digits = String::new();
    let mut i = from;
    let mut last_was_separator = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            digits.push(c);
            last_was_separator = false;
            i += 1;
        } else if separators.contains(c) && !last_was_separator && !digits.is_empty() {
            last_was_separator = true;
            i += 1;
        } else {
            break;
        }
    }
    if last_was_separator {
        i -= 1;
    }
    (digits, i)
}

const SEPARATORS: &str = " .-/\u{a0}\u{202f}";

fn scan_phone_fr(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i > 0 && is_word_char(chars[i - 1]) {
            i += 1;
            continue;
        }
        let (prefix_len, after) = if chars[i] == '+' { (1, i + 1) } else { (0, i) };
        let (digits, end) = read_digits(chars, after, SEPARATORS);
        // `+33 1 23 45 67 89`, `0033…` ou `01 23 45 67 89`.
        let national = if prefix_len == 1 || digits.starts_with("00") {
            let rest = digits.strip_prefix("0033").or_else(|| {
                if prefix_len == 1 {
                    digits.strip_prefix("33")
                } else {
                    None
                }
            });
            rest.filter(|r| r.len() == 9 && !r.starts_with('0'))
        } else {
            digits
                .strip_prefix('0')
                .filter(|r| r.len() == 9 && !r.starts_with('0'))
        };
        if national.is_some() && (end >= chars.len() || !is_word_char(chars[end])) {
            out.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Modulo 97 d'un IBAN compact (ISO 13616 : les quatre premiers caractères
/// passent à la fin, les lettres valent 10 à 35).
fn iban_mod97(compact: &str) -> Option<u32> {
    if compact.len() < 5 {
        return None;
    }
    let rearranged: String = compact[4..].chars().chain(compact[..4].chars()).collect();
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let value = if c.is_ascii_digit() {
            u32::from(c as u8 - b'0')
        } else if c.is_ascii_uppercase() {
            u32::from(c as u8 - b'A') + 10
        } else {
            return None;
        };
        rem = if value < 10 {
            (rem * 10 + value) % 97
        } else {
            (rem * 100 + value) % 97
        };
    }
    Some(rem)
}

fn scan_iban(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= chars.len() {
        let head_ok = chars[i].is_ascii_uppercase()
            && chars[i + 1].is_ascii_uppercase()
            && chars[i + 2].is_ascii_digit()
            && chars[i + 3].is_ascii_digit()
            && (i == 0 || !is_word_char(chars[i - 1]));
        if !head_ok {
            i += 1;
            continue;
        }
        let mut compact = String::new();
        let mut j = i;
        let mut last_was_space = false;
        while j < chars.len() && compact.len() < 34 {
            let c = chars[j];
            if c.is_ascii_alphanumeric() {
                compact.push(c.to_ascii_uppercase());
                last_was_space = false;
                j += 1;
            } else if (c == ' ' || c == '\u{a0}') && !last_was_space && !compact.is_empty() {
                last_was_space = true;
                j += 1;
            } else {
                break;
            }
        }
        if last_was_space {
            j -= 1;
        }
        // Un IBAN fait de 15 à 34 caractères ; on raccourcit tant que la clé
        // ne tombe pas juste (un IBAN peut être suivi d'un autre mot collé).
        let mut matched = None;
        let mut len = compact.len();
        while len >= 15 {
            if iban_mod97(&compact[..len]) == Some(1) {
                matched = Some(len);
                break;
            }
            len -= 1;
        }
        if let Some(len) = matched {
            // Retrouve la position de fin dans la ligne d'origine.
            let mut seen = 0;
            let mut end = i;
            while end < j && seen < len {
                if chars[end].is_ascii_alphanumeric() {
                    seen += 1;
                }
                end += 1;
            }
            out.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Clé de Luhn (ISO/IEC 7812).
fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let Some(d) = c.to_digit(10) else {
            return false;
        };
        let d = if i % 2 == 1 {
            let dd = d * 2;
            if dd > 9 {
                dd - 9
            } else {
                dd
            }
        } else {
            d
        };
        sum += d;
    }
    sum % 10 == 0
}

fn scan_credit_card(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() || (i > 0 && is_word_char(chars[i - 1])) {
            i += 1;
            continue;
        }
        let (digits, end) = read_digits(chars, i, " -\u{a0}");
        if (13..=19).contains(&digits.len())
            && luhn_ok(&digits)
            && (end >= chars.len() || !is_word_char(chars[end]))
        {
            out.push((i, end));
            i = end;
            continue;
        }
        i = end.max(i + 1);
    }
    out
}

fn scan_nir(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if (chars[i] != '1' && chars[i] != '2') || (i > 0 && is_word_char(chars[i - 1])) {
            i += 1;
            continue;
        }
        // 15 caractères significatifs, la Corse pouvant écrire 2A ou 2B.
        let mut body = String::new();
        let mut j = i;
        let mut last_was_space = false;
        while j < chars.len() && body.len() < 15 {
            let c = chars[j].to_ascii_uppercase();
            if c.is_ascii_digit() || (body.len() == 5 && (c == 'A' || c == 'B')) {
                body.push(c);
                last_was_space = false;
                j += 1;
            } else if (c == ' ' || c == '\u{a0}') && !last_was_space && !body.is_empty() {
                last_was_space = true;
                j += 1;
            } else {
                break;
            }
        }
        if body.len() == 15 && nir_key_ok(&body) && (j >= chars.len() || !is_word_char(chars[j])) {
            out.push((i, j));
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// Clé du NIR : `97 - (numéro mod 97)`, la Corse remplaçant 2A par 19 et 2B
/// par 18 après soustraction d'un million.
fn nir_key_ok(body: &str) -> bool {
    let (number, key) = body.split_at(13);
    let Ok(key) = key.parse::<u64>() else {
        return false;
    };
    let corsica = number.contains('A') || number.contains('B');
    let digits: String = number.replace('A', "19").replace('B', "18");
    let Ok(mut n) = digits.parse::<u64>() else {
        return false;
    };
    if corsica {
        n = n.saturating_sub(1_000_000);
    }
    97 - (n % 97) == key
}

/// Noms de mois français reconnus par [`Pattern::DateFr`].
const MONTHS_FR: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

fn scan_date_fr(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() || (i > 0 && is_word_char(chars[i - 1])) {
            i += 1;
            continue;
        }
        if let Some(end) = date_numeric(chars, i).or_else(|| date_textual(chars, i)) {
            out.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Suite de 1 à `max` chiffres à partir de `from`.
fn take_digits(chars: &[char], from: usize, max: usize) -> Option<(u32, usize)> {
    let mut end = from;
    while end < chars.len() && end - from < max && chars[end].is_ascii_digit() {
        end += 1;
    }
    if end == from {
        return None;
    }
    let value: String = chars[from..end].iter().collect();
    value.parse().ok().map(|v| (v, end))
}

fn date_numeric(chars: &[char], start: usize) -> Option<usize> {
    let (day, i) = take_digits(chars, start, 2)?;
    let sep = *chars.get(i)?;
    if !"/-.".contains(sep) {
        return None;
    }
    let (month, i) = take_digits(chars, i + 1, 2)?;
    if chars.get(i) != Some(&sep) {
        return None;
    }
    let (year, end) = take_digits(chars, i + 1, 4)?;
    let year_len = end - i - 1;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) || !(year_len == 2 || year_len == 4) {
        return None;
    }
    if year_len == 4 && !(1000..=3000).contains(&year) {
        return None;
    }
    if end < chars.len() && is_word_char(chars[end]) {
        return None;
    }
    Some(end)
}

fn date_textual(chars: &[char], start: usize) -> Option<usize> {
    let (day, mut i) = take_digits(chars, start, 2)?;
    if !(1..=31).contains(&day) {
        return None;
    }
    // « 1er janvier »
    let rest: String = chars[i..].iter().collect::<String>().to_lowercase();
    if rest.starts_with("er ") {
        i += 2;
    }
    if chars.get(i) != Some(&' ') {
        return None;
    }
    i += 1;
    let lower: String = chars[i..].iter().collect::<String>().to_lowercase();
    let month = MONTHS_FR.iter().find(|m| lower.starts_with(**m))?;
    i += month.chars().count();
    if chars.get(i) != Some(&' ') {
        return None;
    }
    let (year, end) = take_digits(chars, i + 1, 4)?;
    if end - i - 1 != 4 || !(1000..=3000).contains(&year) {
        return None;
    }
    if end < chars.len() && is_word_char(chars[end]) {
        return None;
    }
    Some(end)
}

fn scan_ipv4(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit()
            || (i > 0 && (is_word_char(chars[i - 1]) || chars[i - 1] == '.'))
        {
            i += 1;
            continue;
        }
        let mut pos = i;
        let mut ok = true;
        for group in 0..4 {
            if group > 0 {
                if chars.get(pos) != Some(&'.') {
                    ok = false;
                    break;
                }
                pos += 1;
            }
            match take_digits(chars, pos, 3) {
                Some((v, end)) if v <= 255 => pos = end,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && (pos >= chars.len() || (!is_word_char(chars[pos]) && chars[pos] != '.')) {
            out.push((i, pos));
            i = pos;
            continue;
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// 3. Application
// ---------------------------------------------------------------------------

/// Bilan d'une application de biffure.
#[derive(Debug, Clone, Default)]
pub struct RedactionReport {
    /// Nombre de zones traitées.
    pub zones: usize,
    /// Glyphes retirés du flux de contenu.
    pub glyphs_removed: usize,
    /// Images noircies ou retirées.
    pub images_modified: usize,
    /// Annotations supprimées (hors marques de biffure consommées).
    pub annotations_removed: usize,
    /// Ce qui n'a pas pu être garanti.
    pub warnings: Vec<String>,
}

impl RedactionReport {
    fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }
}

/// Zone de biffure extraite d'une annotation `/Redact`.
struct Zone {
    rect: Rect,
    fill: Rgb,
    overlay: Option<String>,
    /// Apparence `/RO` à peindre telle quelle, si l'annotation en fournit une.
    overlay_form: Option<Object>,
}

/// Applique toutes les annotations `/Redact` du document.
///
/// Pour chaque zone : les glyphes couverts sont retirés du flux (découpe des
/// `Tj` / `TJ` avec correction du positionnement), les pixels couverts des
/// images sont mis à zéro (l'image entièrement couverte est retirée du
/// contenu et des ressources), les tracés entièrement contenus dans la zone
/// sont supprimés, les annotations qui la touchent aussi, puis le rectangle
/// de remplissage et le texte de remplacement sont dessinés. Les objets
/// devenus inatteignables sont supprimés pour qu'aucune trace ne subsiste
/// même sans `drop_unreferenced`.
///
/// # Errors
/// Pages illisibles, ou page qui n'est pas un objet indirect.
pub fn apply_redactions(doc: &Document) -> Result<RedactionReport> {
    let mut report = RedactionReport::default();
    for page in &collect_pages(doc)? {
        let zones = collect_zones(doc, page);
        if zones.is_empty() {
            continue;
        }
        report.zones += zones.len();
        apply_on_page(doc, page, &zones, &mut report)?;
    }
    if report.zones > 0 {
        sweep_unreachable(doc);
    }
    Ok(report)
}

fn numbers(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<f64>> {
    let o = doc.dict_get(dict, key).ok().flatten()?;
    let a = o.as_array()?;
    Some(
        a.iter()
            .filter_map(|v| doc.resolve(v).ok().and_then(|x| x.as_f64()))
            .collect(),
    )
}

fn collect_zones(doc: &Document, page: &Page) -> Vec<Zone> {
    let mut out = Vec::new();
    let Some(annots) = page.dict.get(&Name::new("Annots")) else {
        return out;
    };
    let Ok(annots) = doc.resolve(annots) else {
        return out;
    };
    let Some(list) = annots.as_array() else {
        return out;
    };
    for item in list {
        let Ok(annot) = doc.resolve(item) else {
            continue;
        };
        let Some(annot) = annot.as_dict() else {
            continue;
        };
        if annot.get(&Name::new("Subtype")).and_then(Object::as_name) != Some(&Name::new("Redact"))
        {
            continue;
        }
        let fill = match numbers(doc, annot, "IC").as_deref() {
            Some([g]) => [*g; 3],
            Some([r, g, b]) => [*r, *g, *b],
            Some([c, m, y, k]) => [
                (1.0 - c) * (1.0 - k),
                (1.0 - m) * (1.0 - k),
                (1.0 - y) * (1.0 - k),
            ],
            _ => [0.0; 3],
        };
        let overlay = doc
            .dict_get(annot, "OverlayText")
            .ok()
            .flatten()
            .and_then(|o| match &*o {
                Object::String(s) => Some(acrux_document::text::decode_text_string(s)),
                _ => None,
            });
        let overlay_form = annot.get(&Name::new("RO")).cloned();
        // Une annotation peut porter plusieurs quadrilatères (§12.5.6.23).
        let quads = numbers(doc, annot, "QuadPoints").unwrap_or_default();
        let mut rects: Vec<Rect> = Vec::new();
        for q in quads.chunks_exact(8) {
            let xs = [q[0], q[2], q[4], q[6]];
            let ys = [q[1], q[3], q[5], q[7]];
            let r = Rect::new(
                xs.iter().copied().fold(f64::MAX, f64::min),
                ys.iter().copied().fold(f64::MAX, f64::min),
                xs.iter().copied().fold(f64::MIN, f64::max),
                ys.iter().copied().fold(f64::MIN, f64::max),
            );
            if !r.is_empty() {
                rects.push(r);
            }
        }
        if rects.is_empty() {
            if let Some(r) = numbers(doc, annot, "Rect").filter(|r| r.len() == 4) {
                rects.push(Rect::new(r[0], r[1], r[2], r[3]));
            }
        }
        for (i, rect) in rects.into_iter().enumerate() {
            out.push(Zone {
                rect,
                fill,
                overlay: overlay.clone(),
                // L'apparence `/RO` couvre l'annotation entière : on ne la
                // peint que pour le premier quadrilatère.
                overlay_form: if i == 0 { overlay_form.clone() } else { None },
            });
        }
    }
    out
}

/// Ressources effectives d'une page (attributs hérités déjà fusionnés par
/// `collect_pages`).
fn page_resources(doc: &Document, page: &Page) -> Dict {
    doc.dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default()
}

// Une passe : reecriture du contenu, reconstruction des ressources,
// habillage, remplacement du flux, purge des annotations. La decouper
// obligerait a faire circuler une demi-douzaine de valeurs intermediaires.
#[allow(clippy::too_many_lines)]
fn apply_on_page(
    doc: &Document,
    page: &Page,
    zones: &[Zone],
    report: &mut RedactionReport,
) -> Result<()> {
    let page_ref = page
        .reference
        .ok_or_else(|| Error::Corrupt("la page doit être un objet indirect".into()))?;
    let rects: Vec<Rect> = zones.iter().map(|z| z.rect).collect();
    let resources = page_resources(doc, page);
    let content = page_content(doc, page);

    let mut counter = 0usize;
    let mut rewriter = Rewriter {
        doc,
        zones: &rects,
        report,
        fonts: HashMap::new(),
        depth: 0,
        ops: 0,
        counter: &mut counter,
    };
    let rewritten = rewriter.rewrite(&content, &resources, Matrix::IDENTITY);

    // Nouveau dictionnaire de ressources : les XObjects devenus inutiles
    // disparaissent, les images recadrées et formulaires réécrits arrivent.
    let mut new_resources = resources.clone();
    let source_xobjects = doc
        .dict_get(&resources, "XObject")
        .ok()
        .flatten()
        .and_then(|x| x.as_dict().cloned())
        .unwrap_or_default();
    let mut xobjects = Dict::new();
    for (k, v) in &source_xobjects {
        if rewritten.used_xobjects.contains(k) {
            xobjects.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in &rewritten.new_xobjects {
        xobjects.insert(k.clone(), v.clone());
    }

    // Habillage : contenu équilibré, puis remplissage et texte de remplacement.
    let mut out = Vec::with_capacity(rewritten.content.len() + 256);
    out.extend_from_slice(b"q\n");
    out.extend_from_slice(&rewritten.content);
    out.extend_from_slice(b"\nQ\n");
    let mut font_name: Option<Name> = None;
    for zone in zones {
        match zone.overlay_form.as_ref() {
            Some(form) if is_form_xobject(doc, form) => {
                let name = fresh_name(&source_xobjects, &mut counter);
                xobjects.insert(name.clone(), form.clone());
                out.extend_from_slice(b"q /");
                out.extend_from_slice(&name.0);
                out.extend_from_slice(b" Do Q\n");
            }
            _ => {
                let f = zone.fill;
                let painted = format!(
                    "q {} {} {} rg {} {} {} {} re f Q\n",
                    fmt(f[0]),
                    fmt(f[1]),
                    fmt(f[2]),
                    fmt(zone.rect.x0),
                    fmt(zone.rect.y0),
                    fmt(zone.rect.width()),
                    fmt(zone.rect.height())
                );
                out.extend_from_slice(painted.as_bytes());
                if let Some(text) = zone.overlay.as_ref().filter(|t| !t.trim().is_empty()) {
                    let name = font_name
                        .get_or_insert_with(|| ensure_helvetica(doc, &mut new_resources))
                        .clone();
                    let ops = overlay_text_operators(
                        zone.rect,
                        f,
                        text,
                        &String::from_utf8_lossy(&name.0),
                    );
                    out.extend_from_slice(b"q ");
                    out.extend_from_slice(ops.as_bytes());
                    out.extend_from_slice(b"Q\n");
                }
            }
        }
    }

    if xobjects.is_empty() {
        new_resources.remove(&Name::new("XObject"));
    } else {
        new_resources.insert(Name::new("XObject"), Object::Dict(xobjects));
    }

    // Remplacement du contenu : les anciens flux sont supprimés, pas conservés.
    let mut dict = page.dict.clone();
    for r in content_refs(doc, page) {
        doc.delete(r);
    }
    let mut stream_dict = Dict::new();
    stream_dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(out.len()).unwrap_or(0)),
    );
    let stream = doc.add(Object::Stream {
        dict: stream_dict,
        raw: out,
    });
    dict.insert(Name::new("Contents"), Object::Reference(stream));
    dict.insert(Name::new("Resources"), Object::Dict(new_resources));

    // Annotations : marques consommées, et tout ce qui touche une zone.
    let annots = match dict.get(&Name::new("Annots")) {
        Some(o) => doc
            .resolve(o)
            .ok()
            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut kept = Vec::new();
    for item in annots {
        let (is_redact, rect) = match doc.resolve(&item).ok().and_then(|a| a.as_dict().cloned()) {
            Some(a) => {
                let subtype = a
                    .get(&Name::new("Subtype"))
                    .and_then(Object::as_name)
                    .cloned();
                let r = numbers(doc, &a, "Rect")
                    .filter(|r| r.len() == 4)
                    .map(|r| Rect::new(r[0], r[1], r[2], r[3]));
                (subtype == Some(Name::new("Redact")), r)
            }
            None => (false, None),
        };
        let touches = rect.is_some_and(|r| rects.iter().any(|z| !z.intersect(&r).is_empty()));
        if is_redact || touches {
            if !is_redact {
                report.annotations_removed += 1;
            }
            if let Object::Reference(r) = item {
                delete_annotation(doc, r);
            }
        } else {
            kept.push(item);
        }
    }
    if kept.is_empty() {
        dict.remove(&Name::new("Annots"));
    } else {
        dict.insert(Name::new("Annots"), Object::Array(kept));
    }
    doc.set(page_ref, Object::Dict(dict));
    Ok(())
}

fn delete_annotation(doc: &Document, r: ObjectRef) {
    if let Ok(o) = doc.get(r) {
        if let Some(d) = o.as_dict() {
            // `/RO` n'est pas supprimé ici : l'apparence de remplacement
            // d'une marque de biffure vient d'être placée dans le contenu.
            // Inutilisée, elle devient inatteignable et le balayage l'enlève.
            for key in ["AP", "Popup", "IRT"] {
                if let Some(Object::Reference(child)) = d.get(&Name::new(key)) {
                    doc.delete(*child);
                }
            }
        }
    }
    doc.delete(r);
}

fn is_form_xobject(doc: &Document, obj: &Object) -> bool {
    doc.resolve(obj).is_ok_and(|r| {
        matches!(&*r, Object::Stream { dict, .. }
            if dict.get(&Name::new("Subtype")) == Some(&Object::Name(Name::new("Form"))))
    })
}

/// Références des flux de contenu d'une page.
fn content_refs(doc: &Document, page: &Page) -> Vec<ObjectRef> {
    let mut out = Vec::new();
    let Some(contents) = page.dict.get(&Name::new("Contents")) else {
        return out;
    };
    if let Object::Reference(r) = contents {
        out.push(*r);
    }
    if let Ok(resolved) = doc.resolve(contents) {
        if let Some(items) = resolved.as_array() {
            for item in items {
                if let Object::Reference(r) = item {
                    out.push(*r);
                }
            }
        }
    }
    out
}

/// Ajoute Helvetica aux ressources et retourne son nom.
fn ensure_helvetica(doc: &Document, resources: &mut Dict) -> Name {
    let mut fonts = doc
        .dict_get(resources, "Font")
        .ok()
        .flatten()
        .and_then(|f| f.as_dict().cloned())
        .unwrap_or_default();
    let name = Name::new("AkRdHelv");
    if !fonts.contains_key(&name) {
        let r = doc.add(helvetica_dict());
        fonts.insert(name.clone(), Object::Reference(r));
    }
    resources.insert(Name::new("Font"), Object::Dict(fonts));
    name
}

/// Nom de ressource libre `AkRdN`.
fn fresh_name(existing: &Dict, counter: &mut usize) -> Name {
    loop {
        let name = Name::new(&format!("AkRd{counter}"));
        *counter += 1;
        if !existing.contains_key(&name) {
            return name;
        }
    }
}

/// Supprime les objets qu'aucune référence n'atteint depuis le trailer.
/// Indispensable après une biffure : l'ancien flux de contenu et les images
/// remplacées ne doivent plus exister, même si l'appelant enregistre sans
/// `drop_unreferenced`.
fn sweep_unreachable(doc: &Document) -> usize {
    let reachable = doc.reachable_objects();
    let mut removed = 0;
    for number in doc.object_numbers() {
        if !reachable.contains(&number) {
            doc.delete(ObjectRef {
                number,
                generation: 0,
            });
            removed += 1;
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// 3b. Réécriture du flux de contenu
// ---------------------------------------------------------------------------

/// État graphique et texte suivi pendant la réécriture (§8.4, §9.3).
#[derive(Clone)]
struct State {
    ctm: Matrix,
    tm: Matrix,
    tlm: Matrix,
    font: Option<Rc<LoadedFont>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    hscale: f64,
    leading: f64,
    rise: f64,
}

impl State {
    fn new(ctm: Matrix) -> Self {
        Self {
            ctm,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            hscale: 1.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

/// Résultat de la réécriture d'un flux.
struct Rewritten {
    content: Vec<u8>,
    /// Entrées à ajouter au `/XObject` des ressources de ce niveau.
    new_xobjects: Vec<(Name, Object)>,
    /// Noms d'XObjects encore employés par le contenu réécrit.
    used_xobjects: BTreeSet<Name>,
}

/// XObject invoqué par `Do` : son nom dans les ressources, l'objet résolu
/// et son dictionnaire.
struct Invoked<'x> {
    name: &'x Name,
    object: Object,
    dict: Dict,
}

/// Sorties du niveau de contenu courant, regroupées pour limiter le nombre
/// d'arguments passés aux gestionnaires d'opérateurs.
struct Sink<'s> {
    out: &'s mut Vec<u8>,
    new_xobjects: &'s mut Vec<(Name, Object)>,
    used: &'s mut BTreeSet<Name>,
}

impl Sink<'_> {
    /// Conserve l'opération telle quelle, ainsi que la ressource qu'elle nomme.
    fn keep(&mut self, name: &Name, op: &Operation) {
        self.used.insert(name.clone());
        write_operation(op, self.out);
    }

    /// Remplace la ressource nommée par une nouvelle, sous un nom libre.
    fn replace(&mut self, resources: &Dict, counter: &mut usize, value: Object) {
        let fresh = fresh_name(resources, counter);
        self.out.push(b'/');
        self.out.extend_from_slice(&fresh.0);
        self.out.extend_from_slice(b" Do\n");
        self.new_xobjects.push((fresh, value));
    }
}

/// Dictionnaire de ressources dont le `/XObject` ne garde que les noms encore
/// employés par le contenu réécrit, augmenté des objets créés au passage.
fn merge_xobjects(doc: &Document, mut resources: Dict, rewritten: &Rewritten) -> Dict {
    let source = doc
        .dict_get(&resources, "XObject")
        .ok()
        .flatten()
        .and_then(|x| x.as_dict().cloned())
        .unwrap_or_default();
    let mut xobjects = Dict::new();
    for (k, v) in &source {
        if rewritten.used_xobjects.contains(k) {
            xobjects.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in &rewritten.new_xobjects {
        xobjects.insert(k.clone(), v.clone());
    }
    if xobjects.is_empty() {
        resources.remove(&Name::new("XObject"));
    } else {
        resources.insert(Name::new("XObject"), Object::Dict(xobjects));
    }
    resources
}

struct Rewriter<'a> {
    doc: &'a Document,
    zones: &'a [Rect],
    report: &'a mut RedactionReport,
    fonts: HashMap<String, Option<Rc<LoadedFont>>>,
    depth: usize,
    ops: usize,
    counter: &'a mut usize,
}

/// Vrai si la zone couvre assez du rectangle pour le considérer biffé.
fn covered(zones: &[Rect], b: &Rect, ratio: f64) -> bool {
    let area = b.width() * b.height();
    zones.iter().any(|z| {
        let i = z.intersect(b);
        if i.is_empty() {
            return false;
        }
        if area <= 1e-9 {
            return true;
        }
        i.width() * i.height() / area >= ratio
    })
}

fn intersects(zones: &[Rect], b: &Rect) -> bool {
    zones.iter().any(|z| !z.intersect(b).is_empty())
}

fn fully_covered(zones: &[Rect], b: &Rect) -> bool {
    zones
        .iter()
        .any(|z| z.x0 <= b.x0 && z.y0 <= b.y0 && z.x1 >= b.x1 && z.y1 >= b.y1)
}

/// Sérialise une opération en syntaxe de contenu.
fn write_operation(op: &Operation, out: &mut Vec<u8>) {
    if let Some(img) = &op.inline_image {
        write_inline_image(&img.dict, &img.data, out);
        return;
    }
    for o in &op.operands {
        write_object(o, out);
        out.push(b' ');
    }
    out.extend_from_slice(&op.operator);
    out.push(b'\n');
}

fn write_inline_image(dict: &Dict, data: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"BI");
    for (k, v) in dict {
        out.push(b' ');
        write_object(&Object::Name(k.clone()), out);
        out.push(b' ');
        write_object(v, out);
    }
    out.extend_from_slice(b" ID ");
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nEI\n");
}

/// Chaîne unique d'un opérateur de texte, présentée comme un item de `TJ`.
fn single_string(operand: Option<&Object>) -> Option<Vec<Object>> {
    match operand {
        Some(s @ Object::String(_)) => Some(vec![s.clone()]),
        _ => None,
    }
}

/// Plages d'octets de chaque glyphe d'une chaîne montrée. `None` quand les
/// codes n'ont pas tous la même longueur (CMap à longueurs mixtes).
fn code_ranges(font: &LoadedFont, bytes: &[u8], count: usize) -> Option<usize> {
    if count == 0 {
        return Some(1);
    }
    if !font.is_composite() && bytes.len() == count {
        return Some(1);
    }
    if bytes.len() == count {
        return Some(1);
    }
    if bytes.len() == 2 * count {
        return Some(2);
    }
    None
}

impl Rewriter<'_> {
    fn font(&mut self, resources: &Dict, name: &Name) -> Option<Rc<LoadedFont>> {
        let entry = self
            .doc
            .dict_get(resources, "Font")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().and_then(|d| d.get(name).cloned()))?;
        let key = match &entry {
            Object::Reference(r) => format!("ref:{}", r.number),
            o => format!("direct:{}", acrux_document::writer::to_string(o)),
        };
        if let Some(f) = self.fonts.get(&key) {
            return f.clone();
        }
        let loaded = self
            .doc
            .resolve(&entry)
            .ok()
            .and_then(|r| r.as_dict().cloned())
            .and_then(|d| LoadedFont::load(self.doc, &d).ok())
            .map(Rc::new);
        self.fonts.insert(key, loaded.clone());
        loaded
    }

    /// Réécrit un flux de contenu. `ctm` est la matrice active à l'entrée.
    #[allow(clippy::too_many_lines)] // un bras par famille d'opérateurs, comme l'interpréteur
    fn rewrite(&mut self, content: &[u8], resources: &Dict, ctm: Matrix) -> Rewritten {
        let mut out = Vec::with_capacity(content.len());
        let mut new_xobjects: Vec<(Name, Object)> = Vec::new();
        let mut used_xobjects = BTreeSet::new();
        let mut st = State::new(ctm);
        let mut stack: Vec<State> = Vec::new();
        let mut path: Vec<u8> = Vec::new();
        let mut path_bbox: Option<Rect> = None;
        let mut path_clip = false;
        let mut lexer = ContentLexer::new(content);
        while let Ok(Some(op)) = lexer.next_operation() {
            self.ops += 1;
            if self.ops > MAX_OPS {
                self.report
                    .warn("budget d'opérateurs dépassé : flux tronqué");
                break;
            }
            let nums: Vec<f64> = op.operands.iter().filter_map(Object::as_f64).collect();
            let n = |i: usize| nums.get(i).copied().unwrap_or(0.0);
            match op.operator.as_slice() {
                // --- Construction de chemin : mise en tampon (§8.5.2).
                b"m" | b"l" if nums.len() >= 2 => {
                    add_point(&mut path_bbox, st.ctm.apply(Point::new(n(0), n(1))));
                    write_operation(&op, &mut path);
                }
                b"c" if nums.len() >= 6 => {
                    for i in 0..3 {
                        add_point(
                            &mut path_bbox,
                            st.ctm.apply(Point::new(n(2 * i), n(2 * i + 1))),
                        );
                    }
                    write_operation(&op, &mut path);
                }
                b"v" | b"y" if nums.len() >= 4 => {
                    for i in 0..2 {
                        add_point(
                            &mut path_bbox,
                            st.ctm.apply(Point::new(n(2 * i), n(2 * i + 1))),
                        );
                    }
                    write_operation(&op, &mut path);
                }
                b"h" => write_operation(&op, &mut path),
                b"re" if nums.len() >= 4 => {
                    for (dx, dy) in [(0.0, 0.0), (n(2), 0.0), (n(2), n(3)), (0.0, n(3))] {
                        add_point(
                            &mut path_bbox,
                            st.ctm.apply(Point::new(n(0) + dx, n(1) + dy)),
                        );
                    }
                    write_operation(&op, &mut path);
                }
                b"W" | b"W*" => {
                    path_clip = true;
                    write_operation(&op, &mut path);
                }
                // --- Peinture : le chemin entièrement couvert disparaît.
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                    let erase = !path_clip
                        && path_bbox.is_some_and(|b| fully_covered(self.zones, &b))
                        && op.operator != b"n";
                    if !erase {
                        out.extend_from_slice(&path);
                        write_operation(&op, &mut out);
                    }
                    path.clear();
                    path_bbox = None;
                    path_clip = false;
                }
                // --- État graphique.
                b"q" => {
                    stack.push(st.clone());
                    write_operation(&op, &mut out);
                }
                b"Q" => {
                    // Un `Q` orphelin dépilerait l'habillage ajouté autour du
                    // contenu : il est supprimé.
                    if let Some(s) = stack.pop() {
                        let (tm, tlm) = (st.tm, st.tlm);
                        st = s;
                        st.tm = tm;
                        st.tlm = tlm;
                        write_operation(&op, &mut out);
                    }
                }
                b"cm" if nums.len() >= 6 => {
                    st.ctm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5)).then(&st.ctm);
                    write_operation(&op, &mut out);
                }
                // --- État texte (§9.3).
                b"BT" => {
                    st.tm = Matrix::IDENTITY;
                    st.tlm = Matrix::IDENTITY;
                    write_operation(&op, &mut out);
                }
                b"Tc" => {
                    st.char_spacing = n(0);
                    write_operation(&op, &mut out);
                }
                b"Tw" => {
                    st.word_spacing = n(0);
                    write_operation(&op, &mut out);
                }
                b"Tz" => {
                    st.hscale = n(0) / 100.0;
                    write_operation(&op, &mut out);
                }
                b"TL" => {
                    st.leading = n(0);
                    write_operation(&op, &mut out);
                }
                b"Ts" => {
                    st.rise = n(0);
                    write_operation(&op, &mut out);
                }
                b"Tf" => {
                    st.size = n(0);
                    if let Some(Object::Name(name)) = op.operands.first() {
                        st.font = self.font(resources, name);
                    }
                    write_operation(&op, &mut out);
                }
                b"Td" => {
                    st.tlm = Matrix::translate(n(0), n(1)).then(&st.tlm);
                    st.tm = st.tlm;
                    write_operation(&op, &mut out);
                }
                b"TD" => {
                    st.leading = -n(1);
                    st.tlm = Matrix::translate(n(0), n(1)).then(&st.tlm);
                    st.tm = st.tlm;
                    write_operation(&op, &mut out);
                }
                b"Tm" if nums.len() >= 6 => {
                    st.tlm = Matrix::new(n(0), n(1), n(2), n(3), n(4), n(5));
                    st.tm = st.tlm;
                    write_operation(&op, &mut out);
                }
                b"T*" => {
                    st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                    st.tm = st.tlm;
                    write_operation(&op, &mut out);
                }
                b"Tj" | b"TJ" | b"'" | b"\"" => self.show(&mut st, &op, &mut out),
                // --- XObjects et images en ligne.
                b"Do" => {
                    let mut sink = Sink {
                        out: &mut out,
                        new_xobjects: &mut new_xobjects,
                        used: &mut used_xobjects,
                    };
                    self.do_xobject(&st, &op, resources, &mut sink);
                }
                b"BI" => self.inline_image(&st, &op, resources, &mut out),
                _ => write_operation(&op, &mut out),
            }
        }
        // Les `q` non refermés le sont, pour que l'habillage reste équilibré.
        for _ in 0..stack.len() {
            out.extend_from_slice(b"Q\n");
        }
        Rewritten {
            content: out,
            new_xobjects,
            used_xobjects,
        }
    }

    /// Réécrit une opération de texte en retirant les glyphes couverts.
    #[allow(clippy::too_many_lines)] // les quatre opérateurs de texte partagent tout le traitement
    fn show(&mut self, st: &mut State, op: &Operation, out: &mut Vec<u8>) {
        let mut prefix = String::new();
        // Items montres ; `prefix` recoit ce que l'operateur faisait en plus
        // de montrer du texte, a reemettre devant le `TJ` reconstruit.
        let shown: Option<Vec<Object>> = match op.operator.as_slice() {
            b"Tj" => single_string(op.operands.first()),
            b"TJ" => match op.operands.last() {
                Some(Object::Array(a)) => Some(a.clone()),
                _ => None,
            },
            b"'" => {
                st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                st.tm = st.tlm;
                prefix.push_str("T*\n");
                single_string(op.operands.last())
            }
            b"\"" => {
                let aw = op.operands.first().and_then(Object::as_f64).unwrap_or(0.0);
                let ac = op.operands.get(1).and_then(Object::as_f64).unwrap_or(0.0);
                st.word_spacing = aw;
                st.char_spacing = ac;
                st.tlm = Matrix::translate(0.0, -st.leading).then(&st.tlm);
                st.tm = st.tlm;
                let _ = writeln!(prefix, "{} Tw {} Tc T*", fmt(aw), fmt(ac));
                single_string(op.operands.get(2))
            }
            _ => None,
        };
        let Some(items) = shown else {
            write_operation(op, out);
            return;
        };
        let Some(font) = st.font.clone() else {
            self.report
                .warn("police absente des ressources : du texte n'a pas pu être analysé");
            write_operation(op, out);
            return;
        };
        let denom = st.size * st.hscale;
        let mut result: Vec<Object> = Vec::new();
        let mut current: Vec<u8> = Vec::new();
        let mut pending = 0.0f64;
        let mut removed = 0usize;
        for item in &items {
            match item {
                Object::String(bytes) => {
                    let glyphs = font.decode(bytes);
                    let step = code_ranges(&font, bytes, glyphs.len());
                    let mut hit = Vec::with_capacity(glyphs.len());
                    let mut total = 0.0;
                    for g in &glyphs {
                        let trm = Matrix::new(st.size * st.hscale, 0.0, 0.0, st.size, 0.0, st.rise)
                            .then(&st.tm)
                            .then(&st.ctm);
                        let p0 = trm.apply(Point::new(0.0, -0.2));
                        let p1 = trm.apply(Point::new(g.width.max(0.0), 0.8));
                        let bbox = Rect::new(p0.x, p0.y, p1.x, p1.y);
                        let mut tx = g.width * st.size + st.char_spacing;
                        if g.is_space {
                            tx += st.word_spacing;
                        }
                        tx *= st.hscale;
                        hit.push(covered(self.zones, &bbox, MIN_GLYPH_COVERAGE));
                        total += tx;
                        st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
                        if hit.last() == Some(&true) && step.is_some() {
                            pending += tx;
                        }
                    }
                    match step {
                        Some(step) => {
                            for (i, is_hit) in hit.iter().enumerate() {
                                if *is_hit {
                                    removed += 1;
                                    continue;
                                }
                                if pending != 0.0 {
                                    flush_gap(&mut result, &mut current, &mut pending, denom);
                                }
                                let from = i * step;
                                if let Some(slice) = bytes.get(from..from + step) {
                                    current.extend_from_slice(slice);
                                }
                            }
                        }
                        None if hit.iter().any(|h| *h) => {
                            // Codes de longueur variable : la chaîne entière part.
                            self.report.warn(
                                "codes de longueur variable : la chaîne a été retirée en entier",
                            );
                            pending += total;
                            removed += glyphs.len().max(1);
                        }
                        None => {
                            if pending != 0.0 {
                                flush_gap(&mut result, &mut current, &mut pending, denom);
                            }
                            current.extend_from_slice(bytes);
                        }
                    }
                }
                other => {
                    if let Some(adj) = other.as_f64() {
                        let tx = -adj / 1000.0 * denom;
                        pending += tx;
                        st.tm = Matrix::translate(tx, 0.0).then(&st.tm);
                    }
                }
            }
        }
        if removed == 0 {
            write_operation(op, out);
            return;
        }
        if !current.is_empty() {
            result.push(Object::String(std::mem::take(&mut current)));
        }
        if pending != 0.0 && denom.abs() > 1e-9 {
            result.push(Object::Real(-pending * 1000.0 / denom));
        }
        self.report.glyphs_removed += removed;
        out.extend_from_slice(prefix.as_bytes());
        write_object(&Object::Array(result), out);
        out.extend_from_slice(b" TJ\n");
    }

    fn do_xobject(&mut self, st: &State, op: &Operation, resources: &Dict, sink: &mut Sink<'_>) {
        let Some(Object::Name(name)) = op.operands.first() else {
            write_operation(op, sink.out);
            return;
        };
        let entry = self
            .doc
            .dict_get(resources, "XObject")
            .ok()
            .flatten()
            .and_then(|x| x.as_dict().and_then(|d| d.get(name).cloned()));
        let Some(entry) = entry else {
            write_operation(op, sink.out);
            return;
        };
        let Ok(resolved) = self.doc.resolve(&entry) else {
            sink.keep(name, op);
            return;
        };
        let Object::Stream { dict, .. } = &*resolved else {
            sink.keep(name, op);
            return;
        };
        let subtype = dict
            .get(&Name::new("Subtype"))
            .and_then(Object::as_name)
            .map(|n| n.0.clone());
        let xobject = Invoked {
            name,
            dict: dict.clone(),
            object: (*resolved).clone(),
        };
        match subtype.as_deref() {
            Some(b"Image") => self.do_image(st, op, resources, sink, &xobject),
            Some(b"Form") => self.do_form(st, op, resources, sink, &xobject),
            _ => sink.keep(name, op),
        }
    }

    /// Image XObject : inchangée, retirée si entièrement couverte, sinon
    /// remplacée par une copie dont les pixels couverts valent zéro.
    fn do_image(
        &mut self,
        st: &State,
        op: &Operation,
        resources: &Dict,
        sink: &mut Sink<'_>,
        xobject: &Invoked<'_>,
    ) {
        let placed = st.ctm.transform_rect(&Rect::new(0.0, 0.0, 1.0, 1.0));
        if !intersects(self.zones, &placed) {
            sink.keep(xobject.name, op);
            return;
        }
        if fully_covered(self.zones, &placed) {
            // Entièrement couverte : elle sort du contenu et des ressources,
            // l'objet devient inatteignable et disparaît à l'enregistrement.
            self.report.images_modified += 1;
            return;
        }
        if let Some(new_obj) =
            self.blacken_xobject(&xobject.object, &xobject.dict, st.ctm, resources)
        {
            let r = self.doc.add(new_obj);
            sink.replace(resources, self.counter, Object::Reference(r));
            self.report.images_modified += 1;
        } else {
            self.report.warn(format!(
                "image /{} illisible : seulement recouverte, non modifiée",
                String::from_utf8_lossy(&xobject.name.0)
            ));
            sink.keep(xobject.name, op);
        }
    }

    /// XObject de formulaire : son contenu est réécrit récursivement dans une
    /// copie, pour ne pas toucher les autres pages qui l'emploieraient.
    fn do_form(
        &mut self,
        st: &State,
        op: &Operation,
        resources: &Dict,
        sink: &mut Sink<'_>,
        xobject: &Invoked<'_>,
    ) {
        let (name, dict) = (xobject.name, &xobject.dict);
        let form_matrix = numbers(self.doc, dict, "Matrix")
            .filter(|m| m.len() == 6)
            .map_or(Matrix::IDENTITY, |m| {
                Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
            });
        let inner_ctm = form_matrix.then(&st.ctm);
        let bbox = numbers(self.doc, dict, "BBox")
            .filter(|b| b.len() == 4)
            .map(|b| inner_ctm.transform_rect(&Rect::new(b[0], b[1], b[2], b[3])));
        if bbox.is_some_and(|b| !intersects(self.zones, &b)) {
            sink.keep(name, op);
            return;
        }
        if self.depth >= MAX_DEPTH {
            self.report
                .warn("formulaires trop imbriqués : contenu laissé tel quel");
            sink.keep(name, op);
            return;
        }
        let Ok(inner) = self.doc.stream_data(&xobject.object) else {
            self.report
                .warn("flux de formulaire illisible : contenu laissé tel quel");
            sink.keep(name, op);
            return;
        };
        let own_resources = self
            .doc
            .dict_get(dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned());
        let inner_resources = own_resources.clone().unwrap_or_else(|| resources.clone());
        self.depth += 1;
        let rewritten = self.rewrite(&inner.data, &inner_resources, inner_ctm);
        self.depth -= 1;
        let mut new_dict = dict.clone();
        // Les ressources propres au formulaire sont élaguées ; celles héritées
        // de la page ne le sont pas (elles servent à d'autres contenus).
        if let Some(res) = own_resources {
            new_dict.insert(
                Name::new("Resources"),
                Object::Dict(merge_xobjects(self.doc, res, &rewritten)),
            );
        } else if !rewritten.new_xobjects.is_empty() {
            new_dict.insert(
                Name::new("Resources"),
                Object::Dict(merge_xobjects(self.doc, Dict::new(), &rewritten)),
            );
        }
        let raw = acrux_codecs::flate::compress(&rewritten.content, 6);
        new_dict.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
        new_dict.remove(&Name::new("DecodeParms"));
        new_dict.insert(
            Name::new("Length"),
            Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
        );
        let r = self.doc.add(Object::Stream {
            dict: new_dict,
            raw,
        });
        sink.replace(resources, self.counter, Object::Reference(r));
    }

    fn inline_image(&mut self, st: &State, op: &Operation, resources: &Dict, out: &mut Vec<u8>) {
        let Some(img) = &op.inline_image else {
            write_operation(op, out);
            return;
        };
        let placed = st.ctm.transform_rect(&Rect::new(0.0, 0.0, 1.0, 1.0));
        if !intersects(self.zones, &placed) {
            write_operation(op, out);
            return;
        }
        if fully_covered(self.zones, &placed) {
            self.report.images_modified += 1;
            return;
        }
        let source = Object::Stream {
            dict: img.dict.clone(),
            raw: img.data.clone(),
        };
        if let Some((dict, data)) = self.blacken_inline(&source, &img.dict, st.ctm, resources) {
            write_inline_image(&dict, &data, out);
            self.report.images_modified += 1;
        } else {
            self.report
                .warn("image en ligne illisible : seulement recouverte, non modifiée");
            write_operation(op, out);
        }
    }

    /// Décode une image, noircit les pixels couverts et retourne l'image
    /// résultante, prête à être réencodée.
    fn blacken(
        &mut self,
        source: &Object,
        dict: &Dict,
        ctm: Matrix,
        resources: &Dict,
    ) -> Option<DecodedImage> {
        let decoded = self.doc.stream_data(source).ok()?;
        let filter = decoded
            .image_filter
            .as_ref()
            .map(|(n, p)| (n.as_slice(), p));
        let mut image =
            decode_image(self.doc, dict, &decoded.data, filter, Some(resources)).ok()?;
        let inverse = ctm.invert()?;
        let columns = image.width as usize;
        let rows = image.height as usize;
        if columns == 0 || rows == 0 {
            return None;
        }
        let mut alpha = image.alpha.take();
        for zone in self.zones {
            // Bornes en espace image, obtenues en ramenant la zone dans le
            // carré unité que la matrice courante projette sur la page.
            let unit = inverse.transform_rect(zone);
            let clamp = |v: f64, max: usize| -> usize {
                if v <= 0.0 {
                    0
                } else if v >= max as f64 {
                    max
                } else {
                    v as usize
                }
            };
            let x0 = clamp((unit.x0 * columns as f64).floor(), columns);
            let x1 = clamp((unit.x1 * columns as f64).ceil(), columns);
            let y0 = clamp(((1.0 - unit.y1) * rows as f64).floor(), rows);
            let y1 = clamp(((1.0 - unit.y0) * rows as f64).ceil(), rows);
            for row in y0..y1 {
                for column in x0..x1 {
                    let u = (column as f64 + 0.5) / columns as f64;
                    let v = 1.0 - (row as f64 + 0.5) / rows as f64;
                    if !zone.contains(ctm.apply(Point::new(u, v))) {
                        continue;
                    }
                    let index = row * columns + column;
                    if image.is_stencil {
                        // Pochoir : le motif couvert n'est plus peint du tout.
                        if let Some(slot) = alpha.as_mut().and_then(|a| a.get_mut(index)) {
                            *slot = 0;
                        }
                    } else {
                        for channel in 0..3 {
                            if let Some(slot) = image.rgb.get_mut(index * 3 + channel) {
                                *slot = 0;
                            }
                        }
                        // Noir opaque : la transparence ne doit pas laisser
                        // réapparaître ce qui se trouve dessous.
                        if let Some(slot) = alpha.as_mut().and_then(|a| a.get_mut(index)) {
                            *slot = 255;
                        }
                    }
                }
            }
        }
        image.alpha = alpha;
        Some(image)
    }

    /// Nouvel XObject image (RVB 8 bits Flate, ou pochoir 1 bit).
    fn blacken_xobject(
        &mut self,
        source: &Object,
        dict: &Dict,
        ctm: Matrix,
        resources: &Dict,
    ) -> Option<Object> {
        let image = self.blacken(source, dict, ctm, resources)?;
        let mut d = Dict::new();
        d.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
        d.insert(Name::new("Subtype"), Object::Name(Name::new("Image")));
        d.insert(Name::new("Width"), Object::Integer(i64::from(image.width)));
        d.insert(
            Name::new("Height"),
            Object::Integer(i64::from(image.height)),
        );
        if let Some(interpolate) = dict.get(&Name::new("Interpolate")) {
            d.insert(Name::new("Interpolate"), interpolate.clone());
        }
        let raw = if image.is_stencil {
            d.insert(Name::new("ImageMask"), Object::Bool(true));
            d.insert(Name::new("BitsPerComponent"), Object::Integer(1));
            acrux_codecs::flate::compress(&stencil_bits(&image), 6)
        } else {
            d.insert(
                Name::new("ColorSpace"),
                Object::Name(Name::new("DeviceRGB")),
            );
            d.insert(Name::new("BitsPerComponent"), Object::Integer(8));
            if let Some(alpha) = image.alpha.as_ref().filter(|a| a.iter().any(|v| *v != 255)) {
                let mut s = Dict::new();
                s.insert(Name::new("Type"), Object::Name(Name::new("XObject")));
                s.insert(Name::new("Subtype"), Object::Name(Name::new("Image")));
                s.insert(Name::new("Width"), Object::Integer(i64::from(image.width)));
                s.insert(
                    Name::new("Height"),
                    Object::Integer(i64::from(image.height)),
                );
                s.insert(
                    Name::new("ColorSpace"),
                    Object::Name(Name::new("DeviceGray")),
                );
                s.insert(Name::new("BitsPerComponent"), Object::Integer(8));
                let packed = acrux_codecs::flate::compress(alpha, 6);
                s.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
                s.insert(
                    Name::new("Length"),
                    Object::Integer(i64::try_from(packed.len()).unwrap_or(0)),
                );
                let smask = self.doc.add(Object::Stream {
                    dict: s,
                    raw: packed,
                });
                d.insert(Name::new("SMask"), Object::Reference(smask));
            }
            acrux_codecs::flate::compress(&image.rgb, 6)
        };
        d.insert(Name::new("Filter"), Object::Name(Name::new("FlateDecode")));
        d.insert(
            Name::new("Length"),
            Object::Integer(i64::try_from(raw.len()).unwrap_or(0)),
        );
        Some(Object::Stream { dict: d, raw })
    }

    /// Nouvelle image en ligne (§8.9.7) : RVB 8 bits Flate, ou pochoir 1 bit.
    fn blacken_inline(
        &mut self,
        source: &Object,
        dict: &Dict,
        ctm: Matrix,
        resources: &Dict,
    ) -> Option<(Dict, Vec<u8>)> {
        let image = self.blacken(source, dict, ctm, resources)?;
        let mut d = Dict::new();
        d.insert(Name::new("W"), Object::Integer(i64::from(image.width)));
        d.insert(Name::new("H"), Object::Integer(i64::from(image.height)));
        d.insert(
            Name::new("BPC"),
            Object::Integer(if image.is_stencil { 1 } else { 8 }),
        );
        let data = if image.is_stencil {
            d.insert(Name::new("IM"), Object::Bool(true));
            stencil_bits(&image)
        } else {
            d.insert(Name::new("CS"), Object::Name(Name::new("RGB")));
            if image
                .alpha
                .as_ref()
                .is_some_and(|a| a.iter().any(|v| *v != 255))
            {
                self.report
                    .warn("transparence d'une image en ligne perdue lors de la biffure");
            }
            image.rgb.clone()
        };
        d.insert(Name::new("F"), Object::Name(Name::new("Fl")));
        Some((d, acrux_codecs::flate::compress(&data, 6)))
    }
}

/// Bits d'un pochoir : 0 = peint (convention par défaut, sans `/Decode`).
fn stencil_bits(image: &DecodedImage) -> Vec<u8> {
    let w = image.width as usize;
    let h = image.height as usize;
    let row = w.div_ceil(8);
    let mut bits = vec![0xFFu8; row * h];
    let Some(alpha) = image.alpha.as_ref() else {
        return bits;
    };
    for y in 0..h {
        for x in 0..w {
            if alpha.get(y * w + x).copied().unwrap_or(0) >= 128 {
                if let Some(byte) = bits.get_mut(y * row + x / 8) {
                    *byte &= !(1 << (7 - (x % 8)));
                }
            }
        }
    }
    bits
}

fn add_point(bbox: &mut Option<Rect>, p: Point) {
    let r = Rect::new(p.x, p.y, p.x, p.y);
    *bbox = Some(bbox.map_or(r, |b| b.union(&r)));
}

/// Écrit la chaîne en cours puis la correction de positionnement accumulée.
fn flush_gap(result: &mut Vec<Object>, current: &mut Vec<u8>, pending: &mut f64, denom: f64) {
    if !current.is_empty() {
        result.push(Object::String(std::mem::take(current)));
    }
    if denom.abs() > 1e-9 {
        result.push(Object::Real(-*pending * 1000.0 / denom));
    }
    *pending = 0.0;
}

// ---------------------------------------------------------------------------
// 4. Nettoyage des données cachées
// ---------------------------------------------------------------------------

/// Catégories de données cachées à retirer.
#[derive(Debug, Clone, Copy, Default)]
// Une case à cocher par catégorie : c'est exactement la boîte de dialogue
// « Supprimer les informations masquées » d'Acrobat.
#[allow(clippy::struct_excessive_bools)]
pub struct SanitizeOptions {
    /// `/Info` et flux XMP `/Metadata`, y compris ceux des pages.
    pub metadata: bool,
    /// Pièces jointes (`/Names /EmbeddedFiles`, `/Filespec`, `/FileAttachment`).
    pub attachments: bool,
    /// JavaScript (`/Names /JavaScript`, `/AA`, `/OpenAction`).
    pub javascript: bool,
    /// Calques désactivés (`/OCProperties /D /OFF`) : leur contenu est retiré,
    /// puis le document est aplati (plus de `/OCProperties`).
    pub hidden_layers: bool,
    /// Commentaires et marquages (toutes les annotations sauf les champs).
    pub comments: bool,
    /// Formulaires : `/AcroForm`, XFA et widgets.
    pub forms: bool,
    /// Texte invisible (mode de rendu 3), typiquement des couches d'OCR.
    pub invisible_text: bool,
    /// Objets qu'aucune référence n'atteint (contenu hors page).
    pub unreachable: bool,
    /// Révisions antérieures : impose une réécriture complète du fichier.
    pub previous_revisions: bool,
}

impl SanitizeOptions {
    /// Toutes les catégories.
    #[must_use]
    pub fn all() -> Self {
        Self {
            metadata: true,
            attachments: true,
            javascript: true,
            hidden_layers: true,
            comments: true,
            forms: true,
            invisible_text: true,
            unreachable: true,
            previous_revisions: true,
        }
    }

    /// Vrai si aucune catégorie n'est demandée.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !(self.metadata
            || self.attachments
            || self.javascript
            || self.hidden_layers
            || self.comments
            || self.forms
            || self.invisible_text
            || self.unreachable
            || self.previous_revisions)
    }
}

/// Ce que le nettoyage a retiré.
#[derive(Debug, Clone, Default)]
pub struct SanitizeReport {
    /// Description de chaque élément retiré, dans l'ordre du traitement.
    pub removed: Vec<String>,
    /// Objets supprimés par le balayage des objets inatteignables.
    pub objects_removed: usize,
    /// Le fichier doit être réécrit entièrement (`save_full_with`) pour que
    /// le nettoyage soit effectif : un ajout incrémental laisserait les
    /// anciennes révisions en place.
    pub requires_full_rewrite: bool,
    /// Ce qui n'a pas pu être traité.
    pub warnings: Vec<String>,
}

impl SanitizeReport {
    fn note(&mut self, message: impl Into<String>) {
        self.removed.push(message.into());
    }
}

fn catalog_ref(doc: &Document) -> Result<ObjectRef> {
    match doc.trailer().get(&Name::new("Root")) {
        Some(Object::Reference(r)) => Ok(*r),
        _ => Err(Error::Corrupt(
            "catalogue absent ou direct : nettoyage impossible".into(),
        )),
    }
}

/// Retire les données cachées demandées.
///
/// Le résultat doit être enregistré par `save_full_with(SaveOptions {
/// drop_unreferenced: true, .. })` : c'est cette réécriture complète qui fait
/// disparaître les révisions antérieures.
///
/// # Errors
/// Catalogue ou pages illisibles.
#[allow(clippy::too_many_lines)] // une section par catégorie, chacune courte
pub fn sanitize(doc: &Document, options: &SanitizeOptions) -> Result<SanitizeReport> {
    let mut report = SanitizeReport {
        requires_full_rewrite: true,
        ..SanitizeReport::default()
    };
    if options.is_empty() {
        return Ok(report);
    }
    let root = catalog_ref(doc)?;
    let mut catalog = doc.catalog()?;
    let pages = collect_pages(doc)?;

    if options.metadata {
        if let Some(Object::Reference(r)) = doc.trailer().get(&Name::new("Info")) {
            doc.delete(*r);
            report.note("dictionnaire d'informations /Info");
        }
        doc.set_trailer_entry("Info", Object::Null);
        if let Some(Object::Reference(r)) = catalog.get(&Name::new("Metadata")) {
            doc.delete(*r);
        }
        if catalog.remove(&Name::new("Metadata")).is_some() {
            report.note("métadonnées XMP du document");
        }
        catalog.remove(&Name::new("PieceInfo"));
        for page in &pages {
            let mut dict = page.dict.clone();
            let mut touched = false;
            if let Some(Object::Reference(r)) = dict.get(&Name::new("Metadata")) {
                doc.delete(*r);
            }
            touched |= dict.remove(&Name::new("Metadata")).is_some();
            touched |= dict.remove(&Name::new("PieceInfo")).is_some();
            if touched {
                if let Some(page_ref) = page.reference {
                    doc.set(page_ref, Object::Dict(dict));
                    report.note(format!("métadonnées de la page {}", page.index + 1));
                }
            }
        }
    }

    if options.attachments {
        let mut removed = false;
        if let Some(names) = names_dict(doc, &catalog) {
            if names.contains_key(&Name::new("EmbeddedFiles")) {
                let mut n = names;
                n.remove(&Name::new("EmbeddedFiles"));
                catalog.insert(Name::new("Names"), Object::Dict(n));
                removed = true;
            }
        }
        if catalog.remove(&Name::new("AF")).is_some() {
            removed = true;
        }
        if removed {
            report.note("pièces jointes du document (/Names /EmbeddedFiles, /AF)");
        }
        let n = remove_annotations(doc, &pages, &|subtype| subtype == b"FileAttachment");
        if n > 0 {
            report.note(format!("{n} annotation(s) pièce jointe"));
        }
        // Les descripteurs de fichier restants (portfolios, /EF orphelins).
        let mut files = 0;
        for number in doc.object_numbers() {
            let r = ObjectRef {
                number,
                generation: 0,
            };
            let Ok(obj) = doc.get(r) else { continue };
            let Some(d) = obj.as_dict() else { continue };
            if d.get(&Name::new("Type")) == Some(&Object::Name(Name::new("Filespec"))) {
                if let Some(Object::Reference(ef)) = d.get(&Name::new("EF")) {
                    doc.delete(*ef);
                }
                doc.delete(r);
                files += 1;
            }
        }
        if files > 0 {
            report.note(format!("{files} descripteur(s) de fichier /Filespec"));
        }
    }

    if options.javascript {
        let mut removed = Vec::new();
        if let Some(names) = names_dict(doc, &catalog) {
            if names.contains_key(&Name::new("JavaScript")) {
                let mut n = names;
                n.remove(&Name::new("JavaScript"));
                catalog.insert(Name::new("Names"), Object::Dict(n));
                removed.push("/Names /JavaScript".to_string());
            }
        }
        if catalog.remove(&Name::new("AA")).is_some() {
            removed.push("/AA du document".into());
        }
        let open_is_js = catalog
            .get(&Name::new("OpenAction"))
            .and_then(|o| doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .is_some_and(|d| {
                d.get(&Name::new("S")) == Some(&Object::Name(Name::new("JavaScript")))
            });
        if open_is_js {
            catalog.remove(&Name::new("OpenAction"));
            removed.push("/OpenAction JavaScript".into());
        }
        for page in &pages {
            let mut dict = page.dict.clone();
            if dict.remove(&Name::new("AA")).is_some() {
                if let Some(page_ref) = page.reference {
                    doc.set(page_ref, Object::Dict(dict));
                    removed.push(format!("/AA de la page {}", page.index + 1));
                }
            }
        }
        let n = strip_object_javascript(doc);
        if n > 0 {
            removed.push(format!("{n} action(s) JavaScript d'objet"));
        }
        for item in removed {
            report.note(format!("JavaScript : {item}"));
        }
    }

    if options.forms {
        if catalog.remove(&Name::new("AcroForm")).is_some() {
            report.note("formulaire AcroForm (et XFA)");
        }
        let n = remove_annotations(doc, &pages, &|subtype| subtype == b"Widget");
        if n > 0 {
            report.note(format!("{n} champ(s) de formulaire"));
        }
    }

    if options.comments {
        let n = remove_annotations(doc, &pages, &|subtype| {
            !matches!(subtype, b"Widget" | b"Link")
        });
        if n > 0 {
            report.note(format!("{n} commentaire(s) et marquage(s)"));
        }
    }

    if options.hidden_layers || options.invisible_text {
        let hidden = if options.hidden_layers {
            hidden_layers(doc, &catalog)
        } else {
            HashSet::new()
        };
        let mut pages_touched = 0;
        for page in &pages {
            if clean_page_content(doc, page, &hidden, options.invisible_text) {
                pages_touched += 1;
            }
        }
        if pages_touched > 0 {
            let what = match (options.hidden_layers, options.invisible_text) {
                (true, true) => "calques désactivés et texte invisible",
                (true, false) => "calques désactivés",
                _ => "texte invisible",
            };
            report.note(format!("{what} : {pages_touched} page(s) réécrite(s)"));
        }
        if options.hidden_layers && catalog.remove(&Name::new("OCProperties")).is_some() {
            report.note("contenu optionnel aplati (/OCProperties retiré)");
        }
    }

    doc.set(root, Object::Dict(catalog));

    if options.unreachable || options.previous_revisions {
        report.objects_removed = sweep_unreachable(doc);
        if report.objects_removed > 0 {
            report.note(format!(
                "{} objet(s) inatteignable(s) (contenu hors page, révisions antérieures)",
                report.objects_removed
            ));
        }
    }
    if options.previous_revisions {
        report.note("révisions antérieures : réécriture complète du fichier exigée");
    }
    Ok(report)
}

fn names_dict(doc: &Document, catalog: &Dict) -> Option<Dict> {
    doc.dict_get(catalog, "Names")
        .ok()
        .flatten()
        .and_then(|n| n.as_dict().cloned())
}

/// Supprime des pages les annotations dont le `/Subtype` satisfait le prédicat.
fn remove_annotations(doc: &Document, pages: &[Page], keep_out: &dyn Fn(&[u8]) -> bool) -> usize {
    let mut removed = 0;
    for page in pages {
        let Some(page_ref) = page.reference else {
            continue;
        };
        let Some(annots) = page.dict.get(&Name::new("Annots")) else {
            continue;
        };
        let Some(list) = doc
            .resolve(annots)
            .ok()
            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        else {
            continue;
        };
        let mut kept = Vec::new();
        for item in list {
            let subtype = doc
                .resolve(&item)
                .ok()
                .and_then(|a| a.as_dict().cloned())
                .and_then(|d| {
                    d.get(&Name::new("Subtype"))
                        .and_then(Object::as_name)
                        .cloned()
                });
            let drop = subtype.as_ref().is_some_and(|s| keep_out(&s.0));
            if drop {
                removed += 1;
                if let Object::Reference(r) = item {
                    delete_annotation(doc, r);
                }
            } else {
                kept.push(item);
            }
        }
        let mut dict = page.dict.clone();
        if kept.is_empty() {
            dict.remove(&Name::new("Annots"));
        } else {
            dict.insert(Name::new("Annots"), Object::Array(kept));
        }
        doc.set(page_ref, Object::Dict(dict));
    }
    removed
}

/// Retire `/AA` et les actions `/A` de type JavaScript de tous les objets.
fn strip_object_javascript(doc: &Document) -> usize {
    let mut count = 0;
    for number in doc.object_numbers() {
        let r = ObjectRef {
            number,
            generation: 0,
        };
        let Ok(obj) = doc.get(r) else { continue };
        let Some(d) = obj.as_dict() else { continue };
        let mut dict = d.clone();
        let mut touched = dict.remove(&Name::new("AA")).is_some();
        let a_is_js = dict
            .get(&Name::new("A"))
            .and_then(|o| doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .is_some_and(|a| {
                a.get(&Name::new("S")) == Some(&Object::Name(Name::new("JavaScript")))
            });
        if a_is_js {
            dict.remove(&Name::new("A"));
            touched = true;
        }
        if touched {
            count += 1;
            match &*obj {
                Object::Stream { raw, .. } => doc.set(
                    r,
                    Object::Stream {
                        dict,
                        raw: raw.clone(),
                    },
                ),
                _ => doc.set(r, Object::Dict(dict)),
            }
        }
    }
    count
}

/// Numéros des groupes de contenu optionnel désactivés (`/OCProperties /D /OFF`).
fn hidden_layers(doc: &Document, catalog: &Dict) -> HashSet<u32> {
    let mut out = HashSet::new();
    let Some(ocp) = doc
        .dict_get(catalog, "OCProperties")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
    else {
        return out;
    };
    let Some(d) = doc
        .dict_get(&ocp, "D")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
    else {
        return out;
    };
    if let Some(off) = doc
        .dict_get(&d, "OFF")
        .ok()
        .flatten()
        .and_then(|o| o.as_array().map(<[Object]>::to_vec))
    {
        for item in off {
            if let Object::Reference(r) = item {
                out.insert(r.number);
            }
        }
    }
    out
}

/// Vrai si l'objet désigne un groupe de contenu optionnel désactivé, soit
/// directement (`/OCG`), soit par appartenance (`/OCMD /OCGs`).
fn is_hidden_oc(doc: &Document, obj: &Object, hidden: &HashSet<u32>) -> bool {
    if hidden.is_empty() {
        return false;
    }
    if let Object::Reference(r) = obj {
        if hidden.contains(&r.number) {
            return true;
        }
    }
    let Ok(resolved) = doc.resolve(obj) else {
        return false;
    };
    let Some(d) = resolved.as_dict() else {
        return false;
    };
    match d.get(&Name::new("OCGs")) {
        Some(Object::Reference(r)) => hidden.contains(&r.number),
        Some(Object::Array(items)) => items
            .iter()
            .any(|i| matches!(i, Object::Reference(r) if hidden.contains(&r.number))),
        _ => false,
    }
}

/// Réécrit le contenu d'une page en supprimant les blocs de calques
/// désactivés et, si demandé, le texte en mode de rendu 3 (invisible).
/// Retourne vrai si la page a changé.
// Un bras par famille d'opérateurs, comme les autres passes de contenu.
#[allow(clippy::too_many_lines)]
fn clean_page_content(
    doc: &Document,
    page: &Page,
    hidden: &HashSet<u32>,
    drop_invisible: bool,
) -> bool {
    if hidden.is_empty() && !drop_invisible {
        return false;
    }
    let resources = page_resources(doc, page);
    let content = page_content(doc, page);
    let properties = doc
        .dict_get(&resources, "Properties")
        .ok()
        .flatten()
        .and_then(|p| p.as_dict().cloned())
        .unwrap_or_default();
    let xobjects = doc
        .dict_get(&resources, "XObject")
        .ok()
        .flatten()
        .and_then(|x| x.as_dict().cloned())
        .unwrap_or_default();
    let mut out = Vec::with_capacity(content.len());
    let mut lexer = ContentLexer::new(&content);
    let mut render_mode = 0i64;
    let mut render_stack: Vec<i64> = Vec::new();
    let mut skip_depth = 0usize;
    let mut marked_depth = 0usize;
    let mut changed = false;
    let mut ops = 0usize;
    while let Ok(Some(op)) = lexer.next_operation() {
        ops += 1;
        if ops > MAX_OPS {
            break;
        }
        match op.operator.as_slice() {
            b"BDC" | b"BMC" => {
                marked_depth += 1;
                if skip_depth == 0 {
                    let hides = op.operands.first() == Some(&Object::Name(Name::new("OC")))
                        && op.operands.get(1).is_some_and(|o| {
                            let target = match o {
                                Object::Name(n) => properties.get(n).cloned(),
                                other => Some(other.clone()),
                            };
                            target.is_some_and(|t| is_hidden_oc(doc, &t, hidden))
                        });
                    if hides {
                        skip_depth = marked_depth;
                        changed = true;
                        continue;
                    }
                }
            }
            b"EMC" => {
                if skip_depth == marked_depth && skip_depth > 0 {
                    skip_depth = 0;
                    marked_depth -= 1;
                    continue;
                }
                marked_depth = marked_depth.saturating_sub(1);
            }
            b"q" => render_stack.push(render_mode),
            b"Q" => {
                if let Some(m) = render_stack.pop() {
                    render_mode = m;
                }
            }
            b"Tr" => {
                render_mode = op
                    .operands
                    .first()
                    .and_then(Object::as_f64)
                    .map_or(0, |v| v as i64);
            }
            b"Do" if skip_depth == 0 => {
                if let Some(Object::Name(name)) = op.operands.first() {
                    let hides = xobjects
                        .get(name)
                        .and_then(|x| doc.resolve(x).ok())
                        .and_then(|x| x.as_dict().and_then(|d| d.get(&Name::new("OC")).cloned()))
                        .is_some_and(|oc| is_hidden_oc(doc, &oc, hidden));
                    if hides {
                        changed = true;
                        continue;
                    }
                }
            }
            b"Tj" | b"TJ" | b"'" | b"\""
                if skip_depth == 0 && drop_invisible && render_mode == 3 =>
            {
                changed = true;
                // Le positionnement doit rester juste pour la suite : le
                // déplacement de ligne de `'` et `"` est conservé.
                if op.operator == b"'" || op.operator == b"\"" {
                    out.extend_from_slice(b"T*\n");
                }
                continue;
            }
            _ => {}
        }
        if skip_depth == 0 {
            write_operation(&op, &mut out);
        }
    }
    if !changed {
        return false;
    }
    let Some(page_ref) = page.reference else {
        return false;
    };
    for r in content_refs(doc, page) {
        doc.delete(r);
    }
    let mut dict = page.dict.clone();
    let mut stream_dict = Dict::new();
    stream_dict.insert(
        Name::new("Length"),
        Object::Integer(i64::try_from(out.len()).unwrap_or(0)),
    );
    let stream = doc.add(Object::Stream {
        dict: stream_dict,
        raw: out,
    });
    dict.insert(Name::new("Contents"), Object::Reference(stream));
    doc.set(page_ref, Object::Dict(dict));
    true
}

// ---------------------------------------------------------------------------
// Métriques et encodage du texte de remplacement
// ---------------------------------------------------------------------------

/// Largeurs Helvetica (AFM Adobe), codes 32 à 126, en millièmes d'em.
/// Table reprise de la spécification des 14 polices standard ; `forms.rs`
/// possède la sienne (module indépendant, pas de dépendance croisée).
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Largeur d'un texte en em (Helvetica).
fn helvetica_width(text: &str) -> f64 {
    text.chars()
        .map(|c| {
            let code = win_ansi_byte(c);
            let i = usize::from(code).saturating_sub(32);
            f64::from(HELVETICA_WIDTHS.get(i).copied().unwrap_or(556)) / 1000.0
        })
        .sum()
}

/// Octet WinAnsi d'un caractère (`?` si absent de l'encodage).
fn win_ansi_byte(c: char) -> u8 {
    match c {
        ' '..='~' | '\u{a0}'..='\u{ff}' => c as u8,
        '€' => 0x80,
        '‚' => 0x82,
        'ƒ' => 0x83,
        '„' => 0x84,
        '…' => 0x85,
        '†' => 0x86,
        '‡' => 0x87,
        'ˆ' => 0x88,
        '‰' => 0x89,
        'Š' => 0x8A,
        '‹' => 0x8B,
        'Œ' => 0x8C,
        'Ž' => 0x8E,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '˜' => 0x98,
        '™' => 0x99,
        'š' => 0x9A,
        '›' => 0x9B,
        'œ' => 0x9C,
        'ž' => 0x9E,
        'Ÿ' => 0x9F,
        _ => b'?',
    }
}

/// Encode un texte en WinAnsi pour une chaîne PDF.
fn win_ansi(text: &str) -> Vec<u8> {
    text.chars().map(win_ansi_byte).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::too_many_lines)]
mod tests {
    use super::*;
    use acrux_document::SaveOptions;

    /// Assemble un PDF à partir des corps d'objets (1 0 obj, 2 0 obj…), avec
    /// une table xref correcte. Même style que `acrux_document::protect`.
    fn build_pdf(objects: &[String], trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream_object(dict: &str, data: &str) -> String {
        format!(
            "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        )
    }

    /// Image 4 × 4 en ASCIIHex : entièrement rouge.
    const RED_IMAGE_HEX: &str = "ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000ff0000>";

    /// Page 300 × 200 : trois lignes sensibles, une image rouge, une annotation.
    fn sample_pdf() -> Vec<u8> {
        let content = concat!(
            "BT /F1 12 Tf 20 170 Td (Nom: Jean Dupont) Tj ET\n",
            "BT /F1 12 Tf 20 150 Td (Secret MOTDEPASSE ici) Tj ET\n",
            "BT /F1 12 Tf 20 130 Td (Courriel jean.dupont@example.org) Tj ET\n",
            "q 40 0 0 40 20 40 cm /Im0 Do Q\n",
        );
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 6 0 R >> >> /Annots [7 0 R] >>"
                .to_string(),
            stream_object("", content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            stream_object(
                "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 /Filter /ASCIIHexDecode",
                RED_IMAGE_HEX,
            ),
            "<< /Type /Annot /Subtype /Square /Rect [60 144 120 166] /C [1 0 0] >>".to_string(),
        ];
        build_pdf(&objects, "")
    }

    /// Octets bruts du fichier plus le contenu décodé de tous ses flux.
    fn searchable_bytes(data: &[u8]) -> Vec<u8> {
        let doc = Document::from_bytes(data.to_vec()).unwrap();
        let mut out = data.to_vec();
        for number in doc.object_numbers() {
            let r = ObjectRef {
                number,
                generation: 0,
            };
            let Ok(obj) = doc.get(r) else { continue };
            if matches!(&*obj, Object::Stream { .. }) {
                if let Ok(d) = doc.stream_data(&obj) {
                    out.push(b'\n');
                    out.extend_from_slice(&d.data);
                }
            }
        }
        out
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        let needle = needle.as_bytes();
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn page_text(doc: &Document) -> String {
        let page = collect_pages(doc).unwrap().remove(0);
        let text = crate::text::extract_page_text(doc, &page).unwrap();
        text.lines
            .iter()
            .map(crate::text::Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(doc: &Document) -> acrux_graphics::Bitmap {
        let page = collect_pages(doc).unwrap().remove(0);
        acrux_render::render_page(
            doc,
            &page,
            1.0,
            &acrux_render::RenderOptions {
                annotations: true,
                background: Some(acrux_graphics::Color::WHITE),
                ..acrux_render::RenderOptions::default()
            },
        )
        .bitmap
    }

    // ---------------------------------------------------------------- texte

    #[test]
    fn redacted_text_leaves_no_trace_in_the_file() {
        let original = Document::from_bytes(sample_pdf()).unwrap();
        let before = render(&original);

        let doc = Document::from_bytes(sample_pdf()).unwrap();
        let marks =
            find_redaction_candidates(&doc, &[Pattern::Literal("MOTDEPASSE".into())]).unwrap();
        assert_eq!(marks.len(), 1, "une occurrence attendue");
        assert_eq!(marks[0].page, 0);
        let refs = mark_redactions(&doc, &marks).unwrap();
        assert_eq!(refs.len(), 1);
        // Marquée mais pas appliquée : le texte est toujours là.
        assert!(page_text(&doc).contains("MOTDEPASSE"));

        let report = apply_redactions(&doc).unwrap();
        assert_eq!(report.zones, 1);
        assert_eq!(
            report.glyphs_removed, 10,
            "les dix codes de MOTDEPASSE doivent partir"
        );
        assert_eq!(
            report.annotations_removed, 1,
            "le carré qui touche la zone est supprimé"
        );
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: true,
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        // 1. Garantie de non-récupération : ni en clair, ni dans un flux décompressé.
        let searchable = searchable_bytes(&saved);
        assert!(!contains(&searchable, "MOTDEPASSE"));
        // 2. Le reste du texte est intact et toujours lisible.
        let reloaded = Document::from_bytes(saved).unwrap();
        let text = page_text(&reloaded);
        assert!(text.contains("Jean Dupont"), "{text}");
        assert!(text.contains("Secret"), "{text}");
        assert!(text.contains("ici"), "{text}");
        assert!(!text.contains("MOTDEPASSE"), "{text}");
        assert!(text.contains("jean.dupont@example.org"), "{text}");
        // 3. L'annotation a disparu.
        let page = collect_pages(&reloaded).unwrap().remove(0);
        assert!(crate::annotations::list_annotations(&reloaded, &page)
            .unwrap()
            .is_empty());
        // 4. Rendu : noir dans la zone, identique ailleurs.
        let after = render(&reloaded);
        let zone = marks[0].rect;
        let inside = after
            .pixel((zone.x0 as u32) + 3, 200 - (zone.y0 as u32) - 3)
            .unwrap();
        assert_eq!(&inside[..3], &[0, 0, 0], "remplissage noir attendu");
        // La première ligne (y PDF ≈ 168..182, soit y device 18..32) ne bouge pas.
        for y in 0..34 {
            for x in 0..300 {
                assert_eq!(
                    before.pixel(x, y),
                    after.pixel(x, y),
                    "pixel ({x}, {y}) modifié hors zone"
                );
            }
        }
    }

    #[test]
    fn positioning_of_the_remaining_text_is_preserved() {
        let original = Document::from_bytes(sample_pdf()).unwrap();
        let before =
            crate::text::extract_page_text(&original, &collect_pages(&original).unwrap().remove(0))
                .unwrap();
        let word_before = before
            .words()
            .iter()
            .find(|w| w.text == "ici")
            .map(|w| w.bbox)
            .unwrap();

        let doc = Document::from_bytes(sample_pdf()).unwrap();
        let marks =
            find_redaction_candidates(&doc, &[Pattern::Literal("MOTDEPASSE".into())]).unwrap();
        mark_redactions(&doc, &marks).unwrap();
        apply_redactions(&doc).unwrap();
        let after =
            crate::text::extract_page_text(&doc, &collect_pages(&doc).unwrap().remove(0)).unwrap();
        let word_after = after
            .words()
            .iter()
            .find(|w| w.text == "ici")
            .map(|w| w.bbox)
            .unwrap();
        assert!(
            (word_before.x0 - word_after.x0).abs() < 0.01,
            "le mot suivant a bougé : {word_before:?} → {word_after:?}"
        );
    }

    // ---------------------------------------------------------------- images

    #[test]
    fn partially_covered_image_is_cut_pixel_by_pixel() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        // L'image occupe [20, 40] – [60, 80] ; la zone en couvre la moitié gauche.
        mark_redactions(
            &doc,
            &[RedactionMark::new(0, Rect::new(18.0, 38.0, 40.0, 82.0))],
        )
        .unwrap();
        let report = apply_redactions(&doc).unwrap();
        assert_eq!(report.images_modified, 1);
        assert_eq!(report.glyphs_removed, 0, "aucun texte dans cette zone");

        let saved = doc
            .save_full_with(&SaveOptions {
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        // L'image d'origine (rouge uni, en ASCIIHex) n'est plus dans le fichier.
        assert!(!contains(&saved, &RED_IMAGE_HEX[..24]));

        let reloaded = Document::from_bytes(saved).unwrap();
        let page = collect_pages(&reloaded).unwrap().remove(0);
        let resources = page_resources(&reloaded, &page);
        let xobjects = reloaded
            .dict_get(&resources, "XObject")
            .unwrap()
            .unwrap()
            .as_dict()
            .cloned()
            .unwrap();
        // La copie recadrée et l'apparence `/RO` de la marque appliquée.
        assert_eq!(xobjects.len(), 2, "{xobjects:?}");
        let image = xobjects
            .values()
            .map(|v| reloaded.resolve(v).unwrap())
            .find(|o| {
                o.as_dict()
                    .and_then(|d| d.get(&Name::new("Subtype")))
                    .and_then(Object::as_name)
                    == Some(&Name::new("Image"))
            })
            .unwrap();
        let dict = image.as_dict().cloned().unwrap();
        let data = reloaded.stream_data(&image).unwrap();
        let decoded = decode_image(&reloaded, &dict, &data.data, None, None).unwrap();
        assert_eq!((decoded.width, decoded.height), (4, 4));
        // Colonnes 0 et 1 noircies (x < 40 pt), colonnes 2 et 3 intactes.
        assert_eq!(&decoded.rgb[0..3], &[0, 0, 0]);
        assert_eq!(&decoded.rgb[3..6], &[0, 0, 0]);
        assert_eq!(&decoded.rgb[6..9], &[255, 0, 0]);
        assert_eq!(&decoded.rgb[9..12], &[255, 0, 0]);
    }

    #[test]
    fn fully_covered_image_leaves_the_resources() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        mark_redactions(
            &doc,
            &[RedactionMark::new(0, Rect::new(10.0, 30.0, 90.0, 90.0))],
        )
        .unwrap();
        let report = apply_redactions(&doc).unwrap();
        assert_eq!(report.images_modified, 1);
        let saved = doc
            .save_full_with(&SaveOptions {
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        assert!(!contains(&saved, &RED_IMAGE_HEX[..24]));
        let reloaded = Document::from_bytes(saved).unwrap();
        let page = collect_pages(&reloaded).unwrap().remove(0);
        let resources = page_resources(&reloaded, &page);
        let remaining = reloaded
            .dict_get(&resources, "XObject")
            .unwrap()
            .and_then(|x| x.as_dict().cloned())
            .unwrap_or_default();
        assert!(
            !remaining.values().any(|v| {
                reloaded.resolve(v).unwrap().as_dict().and_then(|d| {
                    d.get(&Name::new("Subtype"))
                        .and_then(Object::as_name)
                        .cloned()
                }) == Some(Name::new("Image"))
            }),
            "l'image doit sortir des ressources : {remaining:?}"
        );
    }

    // ------------------------------------------------------- tracés et texte

    #[test]
    fn covered_paths_are_erased_and_others_kept() {
        let content = "0 0 1 rg 20 20 20 20 re f 100 100 40 40 re f\n";
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_string(),
            stream_object("", content),
        ];
        let doc = Document::from_bytes(build_pdf(&objects, "")).unwrap();
        mark_redactions(
            &doc,
            &[RedactionMark::new(0, Rect::new(10.0, 10.0, 50.0, 50.0))],
        )
        .unwrap();
        apply_redactions(&doc).unwrap();
        let page = collect_pages(&doc).unwrap().remove(0);
        let new_content = page_content(&doc, &page);
        let text = String::from_utf8_lossy(&new_content);
        assert!(
            !text.contains("20 20 20 20 re"),
            "le carré couvert doit disparaître : {text}"
        );
        assert!(
            text.contains("100 100 40 40 re"),
            "le carré hors zone reste : {text}"
        );
    }

    #[test]
    fn text_inside_a_form_xobject_is_redacted_too() {
        let inner = "BT /F1 12 Tf 10 10 Td (Secret MOTDEPASSE ici) Tj ET
";
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R              /Resources << /XObject << /Fm0 6 0 R >> >> >>"
                .to_string(),
            stream_object("", "q 1 0 0 1 10 140 cm /Fm0 Do Q
"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            stream_object(
                "/Type /XObject /Subtype /Form /BBox [0 0 280 30]                  /Resources << /Font << /F1 5 0 R >> >>",
                inner,
            ),
        ];
        let doc = Document::from_bytes(build_pdf(&objects, "")).unwrap();
        let marks =
            find_redaction_candidates(&doc, &[Pattern::Literal("MOTDEPASSE".into())]).unwrap();
        assert_eq!(marks.len(), 1, "le texte du formulaire doit être trouvé");
        mark_redactions(&doc, &marks).unwrap();
        let report = apply_redactions(&doc).unwrap();
        assert_eq!(report.glyphs_removed, 10);
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: true,
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        let searchable = searchable_bytes(&saved);
        assert!(!contains(&searchable, "MOTDEPASSE"));
        // Le reste du formulaire est conservé et toujours extractible.
        let reloaded = Document::from_bytes(saved).unwrap();
        let text = page_text(&reloaded);
        assert!(text.contains("Secret"), "{text}");
        assert!(text.contains("ici"), "{text}");
    }

    #[test]
    fn overlay_text_is_painted_over_the_zone() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        let mut mark = RedactionMark::new(0, Rect::new(60.0, 144.0, 150.0, 164.0));
        mark.overlay_text = Some("CONFIDENTIEL".into());
        mark.fill = Some([0.1, 0.1, 0.1]);
        mark_redactions(&doc, &[mark]).unwrap();
        apply_redactions(&doc).unwrap();
        let bitmap = render(&doc);
        // Le fond est presque noir, et le texte blanc laisse des pixels clairs.
        let mut dark = 0;
        let mut light = 0;
        for y in 200 - 164..200 - 144 {
            for x in 60..150 {
                let p = bitmap.pixel(x, y).unwrap();
                if p[0] > 200 {
                    light += 1;
                } else if p[0] < 60 {
                    dark += 1;
                }
            }
        }
        assert!(dark > 500, "fond sombre attendu, {dark} pixels");
        assert!(light > 20, "texte de remplacement attendu, {light} pixels");
    }

    // ---------------------------------------------------------------- motifs

    fn scan(pattern: &Pattern, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        pattern
            .scan(&chars)
            .into_iter()
            .map(|(a, b)| chars[a..b].iter().collect())
            .collect()
    }

    #[test]
    fn email_pattern() {
        assert_eq!(
            scan(&Pattern::Email, "écrire à jean.dupont@example.org, merci"),
            vec!["jean.dupont@example.org"]
        );
        assert_eq!(
            scan(&Pattern::Email, "a+b@sous.domaine.fr."),
            vec!["a+b@sous.domaine.fr"]
        );
        assert!(scan(&Pattern::Email, "@x.fr").is_empty());
        assert!(scan(&Pattern::Email, "a@b").is_empty());
    }

    #[test]
    fn phone_pattern() {
        assert_eq!(
            scan(&Pattern::PhoneFr, "tel 01 23 45 67 89 fin"),
            vec!["01 23 45 67 89"]
        );
        assert_eq!(scan(&Pattern::PhoneFr, "0123456789"), vec!["0123456789"]);
        assert_eq!(
            scan(&Pattern::PhoneFr, "+33 1 23 45 67 89"),
            vec!["+33 1 23 45 67 89"]
        );
        assert_eq!(
            scan(&Pattern::PhoneFr, "06.12.34.56.78"),
            vec!["06.12.34.56.78"]
        );
        assert!(scan(&Pattern::PhoneFr, "01 23 45 67 8").is_empty());
    }

    #[test]
    fn iban_pattern() {
        // IBAN de test de la documentation française (clé valide).
        assert_eq!(
            scan(
                &Pattern::Iban,
                "compte FR14 2004 1010 0505 0001 3M02 606 ok"
            ),
            vec!["FR14 2004 1010 0505 0001 3M02 606"]
        );
        assert_eq!(
            scan(&Pattern::Iban, "DE89370400440532013000"),
            vec!["DE89370400440532013000"]
        );
        // Une clé fausse n'est pas reconnue.
        assert!(scan(&Pattern::Iban, "DE89370400440532013001").is_empty());
        assert_eq!(iban_mod97("FR1420041010050500013M02606"), Some(1));
    }

    #[test]
    fn credit_card_pattern() {
        assert_eq!(
            scan(&Pattern::CreditCard, "carte 4539 1488 0343 6467 fin"),
            vec!["4539 1488 0343 6467"]
        );
        assert!(scan(&Pattern::CreditCard, "4539 1488 0343 6468").is_empty());
        assert!(luhn_ok("4539148803436467"));
        assert!(!luhn_ok("4539148803436468"));
    }

    #[test]
    fn social_security_pattern() {
        // 1 80 01 75 123 456 avec la clé 97 - (numéro mod 97).
        let number = "1800175123456";
        let key = 97 - number.parse::<u64>().unwrap() % 97;
        let full = format!("{number}{key:02}");
        assert_eq!(scan(&Pattern::SocialSecurityFr, &full), vec![full.clone()]);
        let wrong = format!("{number}{:02}", (key + 1) % 97);
        assert!(scan(&Pattern::SocialSecurityFr, &wrong).is_empty());
        assert!(nir_key_ok(&full));
    }

    #[test]
    fn date_pattern() {
        assert_eq!(
            scan(&Pattern::DateFr, "le 31/12/2024 ok"),
            vec!["31/12/2024"]
        );
        assert_eq!(scan(&Pattern::DateFr, "1-2-24"), vec!["1-2-24"]);
        assert_eq!(
            scan(&Pattern::DateFr, "né le 3 décembre 1975 à Lyon"),
            vec!["3 décembre 1975"]
        );
        assert_eq!(
            scan(&Pattern::DateFr, "1er janvier 2000"),
            vec!["1er janvier 2000"]
        );
        assert!(scan(&Pattern::DateFr, "32/13/2024").is_empty());
    }

    #[test]
    fn ip_pattern() {
        assert_eq!(
            scan(&Pattern::IpAddress, "de 192.168.0.1 vers"),
            vec!["192.168.0.1"]
        );
        assert!(scan(&Pattern::IpAddress, "999.1.1.1").is_empty());
        assert!(scan(&Pattern::IpAddress, "1.2.3").is_empty());
    }

    #[test]
    fn pattern_names_and_labels() {
        assert_eq!(Pattern::parse("email"), Some(Pattern::Email));
        assert_eq!(Pattern::parse("carte"), Some(Pattern::CreditCard));
        assert_eq!(Pattern::parse("inconnu"), None);
        assert_eq!(Pattern::Iban.label(), "IBAN");
    }

    #[test]
    fn find_and_apply_by_pattern() {
        let doc = Document::from_bytes(sample_pdf()).unwrap();
        let marks = find_redaction_candidates(&doc, &[Pattern::Email]).unwrap();
        assert_eq!(marks.len(), 1);
        mark_redactions(&doc, &marks).unwrap();
        let report = apply_redactions(&doc).unwrap();
        assert_eq!(report.glyphs_removed, "jean.dupont@example.org".len());
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: true,
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        assert!(!contains(
            &searchable_bytes(&saved),
            "jean.dupont@example.org"
        ));
        assert!(contains(&searchable_bytes(&saved), "Jean Dupont"));
    }

    // ------------------------------------------------------------- nettoyage

    fn hidden_data_pdf() -> Vec<u8> {
        let content = concat!(
            "BT /F1 12 Tf 20 170 Td (Visible) Tj ET\n",
            "/OC /MC0 BDC BT /F1 12 Tf 20 150 Td (CALQUE CACHE) Tj ET EMC\n",
            "BT 3 Tr /F1 12 Tf 20 130 Td (TEXTE INVISIBLE) Tj ET\n",
        );
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert\\(1\\)) >> \
             /Names << /JavaScript << /Names [(script) 8 0 R] >> \
             /EmbeddedFiles << /Names [(piece) 9 0 R] >> >> \
             /OCProperties << /OCGs [7 0 R] /D << /OFF [7 0 R] >> >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> /Properties << /MC0 7 0 R >> >> /Annots [6 0 R] >>"
                .to_string(),
            stream_object("", content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (COMMENTAIRE PRIVE) >>"
                .to_string(),
            "<< /Type /OCG /Name (Calque cache) >>".to_string(),
            "<< /S /JavaScript /JS (app.alert\\(2\\)) >>".to_string(),
            "<< /Type /Filespec /F (secret.txt) /EF << /F 10 0 R >> >>".to_string(),
            stream_object("", "CONTENU DE LA PIECE JOINTE"),
            stream_object("", "BT (ORPHELIN HORS PAGE) Tj ET"),
            "<< /Title (TITRE CONFIDENTIEL) /Author (Jean Dupont) >>".to_string(),
        ];
        build_pdf(&objects, "/Info 12 0 R")
    }

    #[test]
    fn sanitize_removes_every_hidden_trace() {
        let doc = Document::from_bytes(hidden_data_pdf()).unwrap();
        let report = sanitize(&doc, &SanitizeOptions::all()).unwrap();
        assert!(report.requires_full_rewrite);
        assert!(
            report.removed.len() >= 6,
            "rapport trop court : {:?}",
            report.removed
        );
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: true,
                drop_unreferenced: true,
                ..SaveOptions::default()
            })
            .unwrap();
        let searchable = searchable_bytes(&saved);
        for secret in [
            "TITRE CONFIDENTIEL",
            "Jean Dupont",
            "app.alert",
            "CONTENU DE LA PIECE JOINTE",
            "secret.txt",
            "CALQUE CACHE",
            "TEXTE INVISIBLE",
            "COMMENTAIRE PRIVE",
            "ORPHELIN HORS PAGE",
        ] {
            assert!(!contains(&searchable, secret), "« {secret} » subsiste");
        }
        // Le contenu visible, lui, est intact.
        let reloaded = Document::from_bytes(saved).unwrap();
        assert!(page_text(&reloaded).contains("Visible"));
        let catalog = reloaded.catalog().unwrap();
        assert!(!catalog.contains_key(&Name::new("OCProperties")));
        assert!(!catalog.contains_key(&Name::new("OpenAction")));
        assert!(reloaded.info().is_none());
    }

    #[test]
    fn sanitize_options_are_independent() {
        let doc = Document::from_bytes(hidden_data_pdf()).unwrap();
        let report = sanitize(
            &doc,
            &SanitizeOptions {
                metadata: true,
                ..SanitizeOptions::default()
            },
        )
        .unwrap();
        assert!(!report.removed.is_empty());
        // Le JavaScript et les calques sont restés.
        let catalog = doc.catalog().unwrap();
        assert!(catalog.contains_key(&Name::new("OpenAction")));
        assert!(catalog.contains_key(&Name::new("OCProperties")));
        assert!(!doc.trailer().contains_key(&Name::new("Info")));
        assert!(SanitizeOptions::default().is_empty());
        assert!(sanitize(&doc, &SanitizeOptions::default())
            .unwrap()
            .removed
            .is_empty());
    }

    // ------------------------------------------------------------ utilitaires

    #[test]
    fn helvetica_metrics_and_encoding() {
        assert!((helvetica_width("Hello") - 2.278).abs() < 1e-9);
        assert_eq!(win_ansi("été"), vec![0xE9, 0x74, 0xE9]);
        assert_eq!(win_ansi("œ€"), vec![0x9C, 0x80]);
        assert_eq!(win_ansi("→"), b"?".to_vec());
    }

    #[test]
    fn covered_uses_a_minimum_overlap() {
        let zones = [Rect::new(0.0, 0.0, 10.0, 10.0)];
        assert!(covered(&zones, &Rect::new(1.0, 1.0, 2.0, 2.0), 0.2));
        // Un glyphe qui n'est qu'effleuré est conservé.
        assert!(!covered(&zones, &Rect::new(9.9, 0.0, 20.0, 10.0), 0.2));
        assert!(fully_covered(&zones, &Rect::new(1.0, 1.0, 2.0, 2.0)));
        assert!(!fully_covered(&zones, &Rect::new(1.0, 1.0, 20.0, 2.0)));
        assert!(intersects(&zones, &Rect::new(9.0, 0.0, 20.0, 10.0)));
    }

    /// Écrit `tests/corpus/synthese/biffure-avant-application.pdf`.
    /// `cargo test -p acrux-features -- --ignored generate_redaction_corpus`.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    fn generate_redaction_corpus() {
        let doc = Document::from_bytes(corpus_pdf()).unwrap();
        let mut marks =
            find_redaction_candidates(&doc, &[Pattern::Iban, Pattern::Email, Pattern::PhoneFr])
                .unwrap();
        for m in &mut marks {
            m.overlay_text = Some("CONFIDENTIEL".into());
        }
        // Une zone posée à la main sur la moitié gauche de l'image.
        marks.push(RedactionMark::new(0, Rect::new(28.0, 28.0, 61.0, 92.0)));
        // Une zone rouge avec texte de remplacement sur le titre du dossier.
        let mut numero = RedactionMark::new(0, Rect::new(180.0, 168.0, 300.0, 186.0));
        numero.fill = Some([0.6, 0.0, 0.0]);
        numero.overlay_text = Some("RETIRÉ".into());
        marks.push(numero);
        mark_redactions(&doc, &marks).unwrap();
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: false,
                ..SaveOptions::default()
            })
            .unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/biffure-avant-application.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }

    /// Page 400 × 200 documentée dans `tests/corpus/synthese/README.md`.
    fn corpus_pdf() -> Vec<u8> {
        let content = concat!(
            "0.95 0.95 0.95 rg 0 0 400 200 re f\n",
            "0 0 0 rg BT /F2 13 Tf 20 172 Td (Fiche client) Tj ET\n",
            "BT /F1 11 Tf 180 172 Td (Dossier 2024-00871) Tj ET\n",
            "BT /F1 10 Tf 110 140 Td (IBAN FR14 2004 1010 0505 0001 3M02 606) Tj ET\n",
            "BT /F1 10 Tf 110 122 Td (Courriel jean.dupont@example.org) Tj ET\n",
            "BT /F1 10 Tf 110 104 Td (Telephone 01 23 45 67 89) Tj ET\n",
            "BT /F1 10 Tf 110 86 Td (Adresse 12 rue des Lilas, Lyon) Tj ET\n",
            "q 60 0 0 60 30 30 cm /Im0 Do Q\n",
            "0.2 0.2 0.2 RG 0.7 w 20 60 m 380 60 l S\n",
            "BT /F1 8 Tf 110 44 Td (Document de synthese : zones de biffure marquees, non appliquees.) Tj ET\n",
        );
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 200] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R /F2 7 0 R >> /XObject << /Im0 6 0 R >> >> >>"
                .to_string(),
            stream_object("", content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_string(),
            stream_object(
                "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 /Filter /ASCIIHexDecode",
                "2040a02040a02040a02040a0c06020c06020c06020c0602020a04020a04020a04020a040\
                 e0e020e0e020e0e020e0e020>",
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
                .to_string(),
        ];
        build_pdf(&objects, "")
    }
}
