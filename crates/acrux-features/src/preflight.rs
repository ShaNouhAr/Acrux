//! Contrôle en amont (« preflight ») : vérification d'un document contre un
//! profil normalisé, correctifs automatiques et aperçu de sortie.
//!
//! | Profil | Norme | Ce qu'il garantit |
//! |---|---|---|
//! | PDF/A-1b | ISO 19005-1 | archivage long terme, PDF 1.4, aucune transparence |
//! | PDF/A-2b | ISO 19005-2 | idem, PDF 1.7, transparence et JPEG 2000 admis |
//! | PDF/A-3b | ISO 19005-3 | idem A-2, pièces jointes de tout type admises |
//! | PDF/X-1a | ISO 15930-1 | échange d'imprimerie, CMJN + tons directs, pas de transparence |
//! | PDF/X-4 | ISO 15930-7 | échange d'imprimerie moderne, transparence et ICC admis |
//! | PDF/UA-1 | ISO 14289-1 | accessibilité (délègue à [`crate::accessibility`]) |
//!
//! Trois services :
//!
//! - [`check_profile`] : rend un [`PreflightReport`], une liste de problèmes
//!   avec leur règle, leur page et le fait qu'ils soient réparables ;
//! - [`fix`] : applique les correctifs sans perte (métadonnées XMP, intention
//!   de sortie sRVB, retrait du JavaScript et des pièces jointes,
//!   `/Interpolate false`, `/TrimBox`, `/DisplayDocTitle`, incorporation des
//!   polices depuis les polices système) et re-vérifie ;
//! - [`separations`] et [`ink_coverage`] : aperçu de sortie, les quatre
//!   plaques CMJN plus les tons directs, et le taux d'encre total.
//!
//! Ce que le module **ne sait pas** faire : aplatir la transparence, convertir
//! un espace colorimétrique en conservant l'apparence, retirer un chiffrement
//! sans le mot de passe, ni transformer une police non incorporée dont aucune
//! équivalente n'existe sur la machine.

mod fix;
mod scan;
mod separations;

use std::fmt::Write as _;

use acrux_core::Result;
use acrux_document::text::decode_text_string;
use acrux_document::{collect_pages, Document, Name, Object};

use crate::accessibility::{check_accessibility, Severity};
use scan::Scan;

pub use fix::{fix, srgb_icc_profile, xmp_packet, FixOptions, FixReport};
pub use separations::{ink_coverage, preview_size, separations, Separation};

/// Profil de conformité visé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// ISO 19005-1 niveau B (archivage, PDF 1.4).
    PdfA1b,
    /// ISO 19005-2 niveau B (archivage, PDF 1.7).
    PdfA2b,
    /// ISO 19005-3 niveau B (archivage avec pièces jointes libres).
    PdfA3b,
    /// ISO 15930-1 (échange d'imprimerie « aveugle », CMJN).
    PdfX1a,
    /// ISO 15930-7 (échange d'imprimerie avec transparence et ICC).
    PdfX4,
    /// ISO 14289-1 (accessibilité universelle).
    PdfUa1,
}

impl Profile {
    /// Lit un nom de profil de la ligne de commande.
    #[must_use]
    pub fn parse(name: &str) -> Option<Profile> {
        match name.to_ascii_lowercase().replace('_', "-").as_str() {
            "pdfa-1b" | "pdf/a-1b" | "pdfa1b" => Some(Profile::PdfA1b),
            "pdfa-2b" | "pdf/a-2b" | "pdfa2b" => Some(Profile::PdfA2b),
            "pdfa-3b" | "pdf/a-3b" | "pdfa3b" => Some(Profile::PdfA3b),
            "pdfx-1a" | "pdf/x-1a" | "pdfx1a" => Some(Profile::PdfX1a),
            "pdfx-4" | "pdf/x-4" | "pdfx4" => Some(Profile::PdfX4),
            "pdfua-1" | "pdf/ua-1" | "pdfua1" => Some(Profile::PdfUa1),
            _ => None,
        }
    }

    /// Nom affiché.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Profile::PdfA1b => "PDF/A-1b",
            Profile::PdfA2b => "PDF/A-2b",
            Profile::PdfA3b => "PDF/A-3b",
            Profile::PdfX1a => "PDF/X-1a",
            Profile::PdfX4 => "PDF/X-4",
            Profile::PdfUa1 => "PDF/UA-1",
        }
    }

    /// Nom court pour la ligne de commande.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Profile::PdfA1b => "pdfa-1b",
            Profile::PdfA2b => "pdfa-2b",
            Profile::PdfA3b => "pdfa-3b",
            Profile::PdfX1a => "pdfx-1a",
            Profile::PdfX4 => "pdfx-4",
            Profile::PdfUa1 => "pdfua-1",
        }
    }

    /// Version PDF maximale admise (`None` = sans limite).
    #[must_use]
    pub fn max_version(self) -> Option<(u8, u8)> {
        match self {
            Profile::PdfA1b | Profile::PdfX1a => Some((1, 4)),
            Profile::PdfA2b | Profile::PdfA3b => Some((1, 7)),
            Profile::PdfX4 => Some((1, 6)),
            Profile::PdfUa1 => None,
        }
    }

    /// Vrai pour les profils d'archivage.
    #[must_use]
    pub fn is_pdfa(self) -> bool {
        matches!(self, Profile::PdfA1b | Profile::PdfA2b | Profile::PdfA3b)
    }

    /// Vrai pour les profils d'imprimerie.
    #[must_use]
    pub fn is_pdfx(self) -> bool {
        matches!(self, Profile::PdfX1a | Profile::PdfX4)
    }

    /// La transparence est-elle interdite ? (PDF/A-1 §6.4, PDF/X-1a §6.3)
    #[must_use]
    pub fn forbids_transparency(self) -> bool {
        matches!(self, Profile::PdfA1b | Profile::PdfX1a)
    }

    /// Les pièces jointes sont-elles interdites ? (PDF/A-1 §6.9, PDF/A-2 §6.8)
    #[must_use]
    pub fn forbids_embedded_files(self) -> bool {
        matches!(self, Profile::PdfA1b | Profile::PdfA2b)
    }

    /// Sous-type de l'intention de sortie attendue.
    #[must_use]
    pub fn output_intent_subtype(self) -> Option<&'static str> {
        match self {
            Profile::PdfA1b | Profile::PdfA2b | Profile::PdfA3b => Some("GTS_PDFA1"),
            Profile::PdfX1a | Profile::PdfX4 => Some("GTS_PDFX"),
            Profile::PdfUa1 => None,
        }
    }

    /// Partie et niveau de conformité pour l'identification XMP.
    #[must_use]
    pub fn xmp_identification(self) -> Option<(&'static str, u8, &'static str)> {
        match self {
            Profile::PdfA1b => Some(("pdfaid", 1, "B")),
            Profile::PdfA2b => Some(("pdfaid", 2, "B")),
            Profile::PdfA3b => Some(("pdfaid", 3, "B")),
            Profile::PdfX1a => Some(("pdfxid", 1, "PDF/X-1a:2003")),
            Profile::PdfX4 => Some(("pdfxid", 4, "PDF/X-4")),
            Profile::PdfUa1 => Some(("pdfuaid", 1, "")),
        }
    }
}

/// Un problème de conformité.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightIssue {
    /// Gravité.
    pub severity: Severity,
    /// Identifiant de règle.
    pub rule: &'static str,
    /// Message en français.
    pub message: String,
    /// Page concernée (0 = première), si la règle est localisée.
    pub page: Option<usize>,
    /// Vrai si [`fix`] sait le réparer sans perte.
    pub fixable: bool,
}

/// Rapport de conformité.
#[derive(Debug, Clone)]
pub struct PreflightReport {
    /// Profil vérifié.
    pub profile: Profile,
    /// Problèmes, triés par sévérité décroissante puis par page.
    pub issues: Vec<PreflightIssue>,
    /// Nombre de pages examinées.
    pub pages: usize,
}

impl PreflightReport {
    /// Nombre de problèmes d'une sévérité donnée.
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == severity)
            .count()
    }

    /// Vrai si le document est conforme (aucune erreur).
    #[must_use]
    pub fn is_conforming(&self) -> bool {
        self.count(Severity::Error) == 0
    }

    /// Vrai si la règle a produit au moins un problème.
    #[must_use]
    pub fn has(&self, rule: &str) -> bool {
        self.issues.iter().any(|i| i.rule == rule)
    }

    /// Résumé « PDF/A-1b : 3 erreurs, 1 avertissement ».
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} : {} erreur(s), {} avertissement(s), {} information(s)",
            self.profile.label(),
            self.count(Severity::Error),
            self.count(Severity::Warning),
            self.count(Severity::Info)
        )
    }

    /// Rapport JSON (écrit à la main : aucune dépendance).
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n");
        let _ = writeln!(out, "  \"profile\": \"{}\",", self.profile.key());
        let _ = writeln!(out, "  \"pages\": {},", self.pages);
        let _ = writeln!(out, "  \"conforming\": {},", self.is_conforming());
        let _ = writeln!(out, "  \"errors\": {},", self.count(Severity::Error));
        let _ = writeln!(out, "  \"warnings\": {},", self.count(Severity::Warning));
        out.push_str("  \"issues\": [\n");
        for (i, issue) in self.issues.iter().enumerate() {
            out.push_str("    {");
            let _ = write!(out, "\"severity\": \"{}\", ", issue.severity.label());
            let _ = write!(out, "\"rule\": \"{}\", ", issue.rule);
            let _ = write!(out, "\"fixable\": {}, ", issue.fixable);
            let _ = write!(out, "\"message\": \"{}\"", json_escape(&issue.message));
            if let Some(p) = issue.page {
                let _ = write!(out, ", \"page\": {}", p + 1);
            }
            out.push('}');
            if i + 1 < self.issues.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n}\n");
        out
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Accumulateur de problèmes, plafonné par règle pour ne pas noyer le lecteur.
const MAX_PER_RULE: usize = 8;

#[derive(Default)]
struct Collector {
    issues: Vec<PreflightIssue>,
    counts: std::collections::BTreeMap<&'static str, usize>,
}

impl Collector {
    fn add(
        &mut self,
        severity: Severity,
        rule: &'static str,
        message: impl Into<String>,
        page: Option<usize>,
        fixable: bool,
    ) {
        let count = self.counts.entry(rule).or_insert(0);
        *count += 1;
        if *count > MAX_PER_RULE {
            return;
        }
        self.issues.push(PreflightIssue {
            severity,
            rule,
            message: message.into(),
            page,
            fixable,
        });
    }

    fn finish(mut self) -> Vec<PreflightIssue> {
        self.issues.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then(a.page.cmp(&b.page))
                .then(a.rule.cmp(b.rule))
        });
        self.issues
    }
}

/// Vérifie un document contre un profil.
///
/// # Errors
/// Catalogue ou arbre des pages illisible.
#[allow(clippy::too_many_lines)] // une règle par bras : le découper nuirait à la lecture
pub fn check_profile(doc: &Document, profile: Profile) -> Result<PreflightReport> {
    let pages = collect_pages(doc)?;
    let scan = Scan::run(doc, &pages);
    let mut c = Collector::default();

    // Version du fichier (PDF/A-1 §6.1.2, PDF/X-1a §6.1).
    if let Some((major, minor)) = profile.max_version() {
        let (m, n) = doc.version();
        if (m, n) > (major, minor) {
            c.add(
                Severity::Error,
                "version",
                format!(
                    "le fichier est en PDF {m}.{n}, {} exige au plus {major}.{minor}",
                    profile.label()
                ),
                None,
                false,
            );
        }
    }

    // Chiffrement : interdit par tous les profils.
    if doc.is_encrypted() {
        c.add(
            Severity::Error,
            "encryption",
            "le document est chiffré : aucun profil de conformité ne l'admet \
             (retirer le chiffrement avec `acr unprotect`)",
            None,
            false,
        );
    }

    // Polices incorporées (PDF/A-1 §6.3.4, PDF/X §6.2.5, PDF/UA §7.21.4.1).
    for font in &scan.fonts {
        if font.embedded {
            continue;
        }
        c.add(
            Severity::Error,
            "fonts-embedded",
            format!(
                "police « {} » non incorporée : le texte ne s'affichera pas à l'identique \
                 sur une autre machine",
                font.name
            ),
            font.page,
            true,
        );
    }
    for font in &scan.fonts {
        if font.embedded && font.subset && !font.complete_subset {
            c.add(
                Severity::Warning,
                "fonts-embedded",
                format!(
                    "police « {} » : sous-ensemble incomplet, le programme incorporé ne \
                     fournit pas tous les glyphes déclarés",
                    font.name
                ),
                font.page,
                false,
            );
        }
    }

    // Intention de sortie (PDF/A-1 §6.2.2, PDF/X §6.2.2).
    if let Some(subtype) = profile.output_intent_subtype() {
        match scan.output_intents.iter().find(|o| o.subtype == subtype) {
            None => c.add(
                Severity::Error,
                "output-intent",
                format!(
                    "aucune intention de sortie /OutputIntents de sous-type /{subtype} : \
                     les couleurs indépendantes du périphérique n'ont pas de référence"
                ),
                None,
                true,
            ),
            Some(intent) if !intent.has_profile && profile.is_pdfa() => c.add(
                Severity::Error,
                "output-intent",
                "l'intention de sortie n'incorpore pas son profil ICC (/DestOutputProfile)",
                None,
                true,
            ),
            Some(_) => {}
        }
        if scan.output_intents.len() > 1 {
            c.add(
                Severity::Warning,
                "output-intent",
                format!(
                    "{} intentions de sortie : elles doivent toutes désigner la même \
                     condition d'impression",
                    scan.output_intents.len()
                ),
                None,
                false,
            );
        }
    }

    // Transparence (PDF/A-1 §6.4, PDF/X-1a §6.3).
    if profile.forbids_transparency() {
        for (page, reason) in &scan.transparency {
            c.add(
                Severity::Error,
                "transparency",
                format!("transparence interdite par {} : {reason}", profile.label()),
                Some(*page),
                false,
            );
        }
    }

    // Actions dangereuses ou non déterministes.
    for (page, kind) in &scan.actions {
        let (rule, message) = match kind.as_str() {
            "JavaScript" => (
                "javascript",
                "action JavaScript : le rendu du document dépendrait d'un script".to_string(),
            ),
            "Launch" => (
                "launch-action",
                "action /Launch : lancement d'une application externe".to_string(),
            ),
            other => (
                "external-action",
                format!("action /{other} : sortie hors du document"),
            ),
        };
        let fixable = rule != "external-action";
        c.add(Severity::Error, rule, message, *page, fixable);
    }

    // Pièces jointes.
    if profile.forbids_embedded_files() && scan.embedded_files > 0 {
        c.add(
            Severity::Error,
            "embedded-files",
            format!(
                "{} fichier(s) incorporé(s) : {} ne les admet pas",
                scan.embedded_files,
                profile.label()
            ),
            None,
            true,
        );
    }

    // Flux externes (§7.3.8.2 : `/F` remplace les données par un fichier).
    if scan.external_streams > 0 {
        c.add(
            Severity::Error,
            "external-streams",
            format!(
                "{} flux référence un fichier externe (/F) : le document n'est pas autonome",
                scan.external_streams
            ),
            None,
            false,
        );
    }

    // Espaces colorimétriques dépendants du périphérique.
    let has_intent = profile
        .output_intent_subtype()
        .is_some_and(|s| scan.output_intents.iter().any(|o| o.subtype == s));
    if profile == Profile::PdfX1a {
        for space in &scan.device_spaces {
            if space == "DeviceRGB" || space == "CalRGB" || space == "Lab" {
                c.add(
                    Severity::Error,
                    "colorspaces",
                    format!(
                        "espace {space} employé : PDF/X-1a n'admet que le gris, le CMJN et \
                         les tons directs"
                    ),
                    None,
                    false,
                );
            }
        }
    } else if !has_intent && profile.output_intent_subtype().is_some() {
        for space in &scan.device_spaces {
            if space.starts_with("Device") {
                c.add(
                    Severity::Error,
                    "colorspaces",
                    format!(
                        "espace {space} employé sans intention de sortie : la couleur n'est \
                         pas définie sans appareil de référence"
                    ),
                    None,
                    true,
                );
            }
        }
    }

    // Images : interpolation interdite (PDF/A-1 §6.2.4, PDF/X §6.2.4).
    for image in &scan.images {
        if image.interpolate {
            c.add(
                Severity::Error,
                "interpolate",
                "image avec /Interpolate true : le lissage à l'affichage n'est pas déterministe",
                image.page,
                true,
            );
        }
    }

    // Boîtes de rognage (PDF/X §6.1.3).
    if profile.is_pdfx() {
        for page in &pages {
            let has_trim = page.dict.contains_key(&Name::new("TrimBox"))
                || page.dict.contains_key(&Name::new("ArtBox"));
            if !has_trim {
                c.add(
                    Severity::Error,
                    "trimbox",
                    "page sans /TrimBox ni /ArtBox : la zone à rogner n'est pas définie",
                    Some(page.index),
                    true,
                );
            }
        }
    }

    // Surimpression (PDF/X §6.2.9 : /OPM 1 attendu).
    if profile.is_pdfx() && scan.overprint {
        c.add(
            Severity::Info,
            "overprint",
            "surimpression employée : vérifier l'aperçu de sortie (`acr separations`) \
             et que /OPM vaut 1",
            None,
            false,
        );
    }

    // Métadonnées XMP (PDF/A-1 §6.7, PDF/X §6.5, PDF/UA §5).
    check_metadata(doc, profile, &scan, &mut c);

    // Accessibilité : PDF/UA délègue au vérificateur dédié.
    if profile == Profile::PdfUa1 {
        if let Ok(report) = check_accessibility(doc) {
            for issue in report.issues {
                if issue.severity == Severity::Info {
                    continue;
                }
                let fixable = matches!(issue.rule, "display-doc-title" | "document-lang");
                c.add(
                    issue.severity,
                    "accessibility",
                    format!("[{}] {}", issue.rule, issue.message),
                    issue.page,
                    fixable,
                );
            }
        }
    }

    Ok(PreflightReport {
        profile,
        issues: c.finish(),
        pages: pages.len(),
    })
}

/// `/Metadata` présent, identifiant le bon profil, et cohérent avec `/Info`.
fn check_metadata(doc: &Document, profile: Profile, scan: &Scan, c: &mut Collector) {
    let Some(xmp) = &scan.metadata else {
        c.add(
            Severity::Error,
            "xmp-metadata",
            "aucun flux /Metadata XMP au niveau du document",
            None,
            true,
        );
        return;
    };
    if let Some((prefix, part, conformance)) = profile.xmp_identification() {
        let part_tag = format!("{prefix}:part");
        let declared_part = xmp_value(xmp, &part_tag);
        if declared_part.as_deref() != Some(part.to_string().as_str()) {
            c.add(
                Severity::Error,
                "xmp-metadata",
                format!(
                    "le XMP ne déclare pas {part_tag}={part} (trouvé : {})",
                    declared_part.unwrap_or_else(|| "rien".into())
                ),
                None,
                true,
            );
        }
        if !conformance.is_empty() && prefix == "pdfaid" {
            let tag = format!("{prefix}:conformance");
            if xmp_value(xmp, &tag).as_deref() != Some(conformance) {
                c.add(
                    Severity::Error,
                    "xmp-metadata",
                    format!("le XMP ne déclare pas {tag}={conformance}"),
                    None,
                    true,
                );
            }
        }
    }
    // Cohérence avec /Info : le titre doit être le même (PDF/A-1 §6.7.3).
    let info_title = crate::accessibility::info_dict(doc).and_then(|info| {
        match doc.dict_get(&info, "Title").ok().flatten().as_deref() {
            Some(Object::String(s)) => Some(decode_text_string(s)),
            _ => None,
        }
    });
    if let Some(title) = info_title.filter(|t| !t.trim().is_empty()) {
        let xmp_title = xmp_value(xmp, "dc:title").or_else(|| xmp_value(xmp, "rdf:li"));
        if xmp_title.as_deref().map(str::trim) != Some(title.trim()) {
            c.add(
                Severity::Warning,
                "xmp-metadata",
                format!("le titre XMP (dc:title) ne correspond pas à /Info /Title « {title} »"),
                None,
                true,
            );
        }
    }
}

/// Valeur textuelle d'une balise XMP `<prefixe:nom>valeur</prefixe:nom>` ou
/// d'un attribut `prefixe:nom="valeur"`. Lecture volontairement naïve : le
/// XMP est du RDF/XML dont on ne veut ici qu'une poignée de champs.
fn xmp_value(xmp: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    if let Some(start) = xmp.find(&open) {
        let rest = &xmp[start + open.len()..];
        let end = rest.find(&format!("</{tag}>"))?;
        return Some(strip_tags(&rest[..end]));
    }
    let attribute = format!("{tag}=\"");
    if let Some(start) = xmp.find(&attribute) {
        let rest = &xmp[start + attribute.len()..];
        let end = rest.find('"')?;
        return Some(rest[..end].to_string());
    }
    None
}

/// Retire les balises d'un fragment XML (le `<rdf:Alt><rdf:li>` d'un titre).
fn strip_tags(fragment: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in fragment.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]
mod tests {
    use super::*;
    use crate::accessibility::tests::{build_pdf, stream_object};
    use acrux_document::SaveOptions;

    /// Force la version de l'en-tête : les profils la contrôlent.
    fn with_version(mut data: Vec<u8>, version: &str) -> Vec<u8> {
        data[5..8].copy_from_slice(version.as_bytes());
        data
    }

    fn open(data: Vec<u8>) -> Document {
        Document::from_bytes(data).unwrap()
    }

    /// Page 100 × 100 sans la moindre police : des aplats vectoriels.
    /// Sert de base aux essais de correction, où aucune police ne peut
    /// manquer.
    fn vector_pdf(version: &str, extra_catalog: &str, content: &str) -> Vec<u8> {
        let objects = vec![
            format!("<< /Type /Catalog /Pages 2 0 R {extra_catalog} >>"),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Resources << >> >>"
                .to_string(),
            stream_object("", content),
        ];
        with_version(build_pdf(&objects, ""), version)
    }

    #[test]
    fn profiles_parse_from_their_command_line_names() {
        assert_eq!(Profile::parse("pdfa-1b"), Some(Profile::PdfA1b));
        assert_eq!(Profile::parse("PDF/X-4"), Some(Profile::PdfX4));
        assert_eq!(Profile::parse("pdfua1"), Some(Profile::PdfUa1));
        assert_eq!(Profile::parse("pdfa-9z"), None);
        for p in [
            Profile::PdfA1b,
            Profile::PdfA2b,
            Profile::PdfA3b,
            Profile::PdfX1a,
            Profile::PdfX4,
            Profile::PdfUa1,
        ] {
            assert_eq!(Profile::parse(p.key()), Some(p), "{}", p.label());
        }
    }

    #[test]
    fn a_bare_document_fails_pdfa1b_on_the_expected_rules() {
        let doc = open(vector_pdf("1.4", "", "0 0 1 rg 10 10 80 80 re f\n"));
        let report = check_profile(&doc, Profile::PdfA1b).unwrap();
        assert!(!report.is_conforming());
        assert!(report.has("output-intent"), "{:#?}", report.issues);
        assert!(report.has("xmp-metadata"));
        assert!(
            report.has("colorspaces"),
            "DeviceRGB sans intention de sortie"
        );
        assert!(report.to_json().contains("\"profile\": \"pdfa-1b\""));
    }

    #[test]
    fn version_above_the_profile_is_an_error() {
        let doc = open(vector_pdf("1.7", "", "0 g 10 10 80 80 re f\n"));
        let report = check_profile(&doc, Profile::PdfA1b).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "version")
            .expect("version refusée attendue");
        assert!(issue.message.contains("1.7"), "{}", issue.message);
        assert!(!issue.fixable);
        // PDF/A-2b admet 1.7.
        let report = check_profile(&doc, Profile::PdfA2b).unwrap();
        assert!(!report.has("version"));
    }

    #[test]
    fn transparency_is_refused_by_pdfa1b_and_admitted_by_pdfa2b() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Resources << /ExtGState << /GS0 5 0 R >> >> >>"
                .to_string(),
            stream_object("", "/GS0 gs 0 g 10 10 80 80 re f\n"),
            "<< /Type /ExtGState /ca 0.5 /BM /Multiply >>".to_string(),
        ];
        let doc = open(with_version(build_pdf(&objects, ""), "1.4"));
        let report = check_profile(&doc, Profile::PdfA1b).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "transparency")
            .expect("transparence refusée attendue");
        assert_eq!(issue.page, Some(0));
        assert!(!issue.fixable, "l'aplatissement n'est pas implémenté");
        assert!(!check_profile(&doc, Profile::PdfA2b)
            .unwrap()
            .has("transparency"));
    }

    #[test]
    fn javascript_and_attachments_are_refused_then_removed() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /Names << /JavaScript 5 0 R \
             /EmbeddedFiles 6 0 R >> /OpenAction << /S /JavaScript /JS (app.alert\\(1\\)) >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Resources << >> >>"
                .to_string(),
            stream_object("", "0 g 10 10 80 80 re f\n"),
            "<< /Names [(script) << /S /JavaScript /JS (app.alert\\(1\\)) >>] >>".to_string(),
            "<< /Names [(piece.txt) << /Type /Filespec /F (piece.txt) >>] >>".to_string(),
        ];
        let doc = open(with_version(build_pdf(&objects, ""), "1.4"));
        let report = check_profile(&doc, Profile::PdfA1b).unwrap();
        assert!(report.has("javascript"), "{:#?}", report.issues);
        assert!(report.has("embedded-files"));

        let report = fix(&doc, Profile::PdfA1b, &FixOptions::default()).unwrap();
        assert!(
            report.applied.iter().any(|a| a.contains("JavaScript")),
            "{:?}",
            report.applied
        );
        assert!(!report.remaining.iter().any(|i| i.rule == "javascript"));
        assert!(!report.remaining.iter().any(|i| i.rule == "embedded-files"));
    }

    #[test]
    fn pdfx_requires_a_trim_box_and_no_interpolation() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Resources << /XObject << /Im0 5 0 R >> >> >>"
                .to_string(),
            stream_object("", "q 80 0 0 80 10 10 cm /Im0 Do Q\n"),
            stream_object(
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8 /Interpolate true /Filter /ASCIIHexDecode",
                "00ff00ff>",
            ),
        ];
        let doc = open(with_version(build_pdf(&objects, ""), "1.4"));
        let report = check_profile(&doc, Profile::PdfX1a).unwrap();
        assert!(report.has("trimbox"), "{:#?}", report.issues);
        assert!(report.has("interpolate"));

        let report = fix(&doc, Profile::PdfX1a, &FixOptions::default()).unwrap();
        assert!(!report.remaining.iter().any(|i| i.rule == "trimbox"));
        assert!(!report.remaining.iter().any(|i| i.rule == "interpolate"));
    }

    #[test]
    fn pdfx1a_refuses_rgb() {
        let doc = open(vector_pdf("1.4", "", "0 0 1 rg 10 10 80 80 re f\n"));
        let report = check_profile(&doc, Profile::PdfX1a).unwrap();
        let issue = report
            .issues
            .iter()
            .find(|i| i.rule == "colorspaces")
            .expect("DeviceRGB refusé attendu");
        assert!(issue.message.contains("DeviceRGB"), "{}", issue.message);
    }

    #[test]
    fn xmp_packet_declares_the_profile_and_repeats_info() {
        let doc = open(vector_pdf("1.4", "", "0 g 10 10 80 80 re f\n"));
        let info = doc.add(Object::Dict(acrux_document::Dict::new()));
        let mut d = acrux_document::Dict::new();
        d.insert(
            Name::new("Title"),
            Object::String(b"Plaquette & catalogue".to_vec()),
        );
        d.insert(
            Name::new("CreationDate"),
            Object::String(b"D:20260918120000+02'00'".to_vec()),
        );
        doc.set(info, Object::Dict(d));
        doc.set_trailer_entry("Info", Object::Reference(info));

        let packet = xmp_packet(&doc, Some(Profile::PdfA1b));
        assert!(packet.contains("<pdfaid:part>1</pdfaid:part>"), "{packet}");
        assert!(packet.contains("<pdfaid:conformance>B</pdfaid:conformance>"));
        assert!(packet.contains("Plaquette &amp; catalogue"), "{packet}");
        assert!(
            packet.contains("<xmp:CreateDate>2026-09-18T12:00:00+02:00</xmp:CreateDate>"),
            "{packet}"
        );
        assert!(packet.starts_with("<?xpacket begin="));
        assert!(packet.trim_end().ends_with("<?xpacket end=\"w\"?>"));

        let packet = xmp_packet(&doc, Some(Profile::PdfUa1));
        assert!(
            packet.contains("<pdfuaid:part>1</pdfuaid:part>"),
            "{packet}"
        );
        let packet = xmp_packet(&doc, Some(Profile::PdfX4));
        assert!(
            packet.contains("<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion>"),
            "{packet}"
        );
    }

    #[test]
    fn generated_srgb_profile_is_read_back_by_our_own_parser() {
        let bytes = srgb_icc_profile();
        let profile = acrux_graphics::IccProfile::parse(&bytes).expect("profil ICC lisible");
        assert_eq!(profile.version().0, 2);
        assert_eq!(&profile.data_space(), b"RGB ");
        assert_eq!(&profile.pcs(), b"XYZ ");
        assert_eq!(profile.components(), 3);
        assert!(profile.is_matrix_trc());
        assert_eq!(
            profile.description().as_deref(),
            Some("sRGB IEC61966-2.1"),
            "{:?}",
            profile.description()
        );
        assert!(profile.is_srgb_like());
        // Une conversion aller-retour : le profil doit reproduire sRGB.
        let white = profile.to_srgb(&[1.0, 1.0, 1.0]);
        assert!(white.iter().all(|c| (*c - 1.0).abs() < 0.02), "{white:?}");
        let black = profile.to_srgb(&[0.0, 0.0, 0.0]);
        assert!(black.iter().all(|c| c.abs() < 0.02), "{black:?}");
        let red = profile.to_srgb(&[1.0, 0.0, 0.0]);
        assert!(
            red[0] > 0.9 && red[1] < 0.15 && red[2] < 0.15,
            "rouge attendu : {red:?}"
        );
        let mid = profile.to_srgb(&[0.5, 0.5, 0.5]);
        assert!(
            mid.iter().all(|c| (*c - 0.5).abs() < 0.03),
            "gris moyen : {mid:?}"
        );
    }

    #[test]
    fn fix_makes_a_vector_document_conform_to_pdfa1b() {
        let doc = open(vector_pdf("1.4", "", "0 0 1 rg 10 10 80 80 re f\n"));
        assert!(!check_profile(&doc, Profile::PdfA1b)
            .unwrap()
            .is_conforming());

        let report = fix(
            &doc,
            Profile::PdfA1b,
            &FixOptions {
                title: Some("Plaquette".into()),
                ..FixOptions::default()
            },
        )
        .unwrap();
        assert!(
            report.is_conforming(),
            "corrections {:?}, restant {:#?}",
            report.applied,
            report.remaining
        );
        assert!(report
            .applied
            .iter()
            .any(|a| a.contains("intention de sortie")));
        assert!(report.applied.iter().any(|a| a.contains("XMP")));

        // La conformité survit à l'enregistrement et à une relecture.
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: false,
                ..SaveOptions::default()
            })
            .unwrap();
        let doc = open(saved);
        let again = check_profile(&doc, Profile::PdfA1b).unwrap();
        assert!(again.is_conforming(), "{:#?}", again.issues);
        // Le profil ICC incorporé est bien celui qu'on engendre.
        let catalog = doc.catalog().unwrap();
        let intents = doc
            .dict_get(&catalog, "OutputIntents")
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        assert_eq!(intents.len(), 1);
        let intent = doc.resolve(&intents[0]).unwrap().as_dict().unwrap().clone();
        let profile = doc.dict_get(&intent, "DestOutputProfile").unwrap().unwrap();
        let data = doc.stream_data(&profile).unwrap().data;
        assert!(acrux_graphics::IccProfile::parse(&data).is_ok());
    }

    #[test]
    fn fix_tags_a_document_for_pdfua() {
        let doc = open(crate::accessibility::tests::untagged_pdf());
        let report = fix(&doc, Profile::PdfUa1, &FixOptions::default()).unwrap();
        assert!(
            report.applied.iter().any(|a| a.contains("balisage")),
            "{:?}",
            report.applied
        );
        assert!(
            report.applied.iter().any(|a| a.contains("titre")),
            "{:?}",
            report.applied
        );
        // Plus aucun défaut d'accessibilité : la seule règle qui peut rester
        // est l'incorporation des polices, qui dépend des polices système.
        let blocking: Vec<&PreflightIssue> = report
            .remaining
            .iter()
            .filter(|i| i.severity == Severity::Error && i.rule != "fonts-embedded")
            .collect();
        assert!(blocking.is_empty(), "{blocking:#?}");
        let tree = crate::accessibility::read_structure(&doc).unwrap();
        assert!(tree.is_tagged());
        assert_eq!(tree.lang.as_deref(), Some("fr"));
    }

    #[test]
    fn separations_split_the_four_process_plates() {
        // Moitié basse en cyan pur, moitié haute en noir pur.
        let content = "1 0 0 0 k 0 0 100 50 re f\n0 0 0 1 k 0 50 100 50 re f\n";
        let doc = open(vector_pdf("1.4", "", content));
        let plates = separations(&doc, 0, 72.0).unwrap();
        let by_name = |name: &str| {
            plates
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("plaque {name} absente"))
        };
        let cyan = by_name("Cyan");
        assert_eq!((cyan.width, cyan.height), (100, 100));
        let at = |p: &Separation, x: usize, y: usize| p.coverage[y * p.width as usize + x];
        // Le pixel (50, 75) est dans la moitié basse de la page (l'image a son
        // origine en haut à gauche).
        assert!(at(cyan, 50, 75) > 200, "{}", at(cyan, 50, 75));
        assert_eq!(at(cyan, 50, 25), 0);
        let black = by_name("Black");
        assert!(at(black, 50, 25) > 200);
        assert_eq!(at(black, 50, 75), 0);
        assert!(by_name("Magenta").is_blank());
        assert!(by_name("Yellow").is_blank());
        assert!(
            cyan.average() > 40.0 && cyan.average() < 60.0,
            "{}",
            cyan.average()
        );
    }

    #[test]
    fn ink_coverage_detects_a_registration_black() {
        // Noir de repérage : les quatre encres à fond, 400 % de couverture.
        let doc = open(vector_pdf("1.4", "", "1 1 1 1 k 0 0 100 100 re f\n"));
        let total = ink_coverage(&doc, 0).unwrap();
        assert!(
            total > 350.0,
            "taux d'encre attendu proche de 400 % : {total}"
        );

        // Un simple noir seul reste à 100 %.
        let doc = open(vector_pdf("1.4", "", "0 0 0 1 k 0 0 100 100 re f\n"));
        let total = ink_coverage(&doc, 0).unwrap();
        assert!((90.0..=110.0).contains(&total), "{total}");
    }

    #[test]
    fn a_named_separation_gets_its_own_plate() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Resources << /ColorSpace << /CS0 5 0 R >> >> >>"
                .to_string(),
            stream_object("", "/CS0 cs 1 scn 20 20 60 60 re f\n"),
            "[/Separation /Pantone#20286#20C /DeviceCMYK 6 0 R]".to_string(),
            stream_object(
                "/FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [1 0.6 0 0] /N 1",
                "",
            ),
        ];
        let doc = open(with_version(build_pdf(&objects, ""), "1.4"));
        let plates = separations(&doc, 0, 72.0).unwrap();
        let spot = plates
            .iter()
            .find(|p| p.name == "Pantone 286 C")
            .unwrap_or_else(|| {
                panic!(
                    "plaque du ton direct absente : {:?}",
                    plates.iter().map(|p| &p.name).collect::<Vec<_>>()
                )
            });
        let at = |x: usize, y: usize| spot.coverage[y * spot.width as usize + x];
        assert!(at(50, 50) > 200, "{}", at(50, 50));
        assert_eq!(at(5, 5), 0);
        // Le ton direct est aussi vu par le rendu, via sa transformation de teinte.
        let cyan = plates.iter().find(|p| p.name == "Cyan").unwrap();
        assert!(cyan.coverage[50 * 100 + 50] > 200);
    }

    #[test]
    fn preview_size_follows_the_page_and_the_resolution() {
        let doc = open(vector_pdf("1.4", "", "0 g 10 10 80 80 re f\n"));
        assert_eq!(preview_size(&doc, 0, 72.0).unwrap(), (100, 100));
        assert_eq!(preview_size(&doc, 0, 144.0).unwrap(), (200, 200));
        assert!(preview_size(&doc, 5, 72.0).is_err());
    }

    /// Document de départ du corpus PDF/A : une page 400 × 260 entièrement
    /// vectorielle (aucune police à incorporer, donc aucun fichier de police
    /// propriétaire dans le dépôt), avec les défauts que `fix` sait réparer.
    fn pdfa_source_pdf() -> Vec<u8> {
        let content = concat!(
            // Fond et bandeau.
            "1 1 1 rg 0 0 400 260 re f\n",
            "0.11 0.29 0.53 rg 0 216 400 44 re f\n",
            "1 1 1 rg 20 232 120 12 re f 20 224 200 4 re f\n",
            // Disque en quatre Béziers (κ = 0,5523 ; centre 60/120, rayon 45).
            "0.90 0.35 0.20 rg 105 120 m 105 144.85 84.85 165 60 165 c ",
            "35.15 165 15 144.85 15 120 c 15 95.15 35.15 75 60 75 c ",
            "84.85 75 105 95.15 105 120 c f\n",
            // Triangle et rectangle plein.
            "0.20 0.55 0.35 rg 110 70 m 190 70 l 150 170 l f\n",
            "0.95 0.75 0.15 rg 210 70 80 100 re f\n",
            // Image dégradée, volontairement en /Interpolate true.
            "q 80 0 0 100 300 70 cm /Im0 Do Q\n",
            // Filets fins.
            "0.35 G 0.7 w 20 46 m 380 46 l S\n",
            "0.75 G 0.4 w 20 36 m 380 36 l S\n",
        );
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript \
             /JS (app.alert\\(\"bonjour\"\\);) >> >>"
                .to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 260] /Contents 4 0 R \
             /Resources << /XObject << /Im0 5 0 R >> >> >>"
                .to_string(),
            stream_object("", content),
            stream_object(
                "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 /Interpolate true /Filter /ASCIIHexDecode",
                "1030601040805020508060306090a040\
                 2050803060a04080c0503070b0609010\
                 3070b050a0e0709030b0f080d0306090>",
            ),
        ];
        with_version(build_pdf(&objects, ""), "1.4")
    }

    /// Écrit `tests/corpus/synthese/pdfa-1b-conforme.pdf`, produit par `fix`.
    /// `cargo test -p acrux-features --lib -- --ignored generate_pdfa_corpus`.
    #[test]
    #[ignore = "génère le fichier de corpus"]
    fn generate_pdfa_corpus() {
        let doc = open(pdfa_source_pdf());
        let report = fix(
            &doc,
            Profile::PdfA1b,
            &FixOptions {
                title: Some("Plaquette de synthèse PDF/A-1b".into()),
                ..FixOptions::default()
            },
        )
        .unwrap();
        for a in &report.applied {
            println!("  - {a}");
        }
        assert!(report.is_conforming(), "{:#?}", report.remaining);
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: false,
                ..SaveOptions::default()
            })
            .unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/corpus/synthese/pdfa-1b-conforme.pdf");
        std::fs::write(&path, saved).unwrap();
        println!("écrit : {}", path.display());
    }

    /// Le document que `fix` produit reste conforme après relecture.
    #[test]
    fn pdfa_corpus_is_conforming_after_fix_and_reload() {
        let doc = open(pdfa_source_pdf());
        assert!(!check_profile(&doc, Profile::PdfA1b)
            .unwrap()
            .is_conforming());
        let report = fix(
            &doc,
            Profile::PdfA1b,
            &FixOptions {
                title: Some("Plaquette de synthèse PDF/A-1b".into()),
                ..FixOptions::default()
            },
        )
        .unwrap();
        assert!(report.is_conforming(), "{:#?}", report.remaining);
        let saved = doc
            .save_full_with(&SaveOptions {
                compress_streams: false,
                ..SaveOptions::default()
            })
            .unwrap();
        let doc = open(saved);
        let again = check_profile(&doc, Profile::PdfA1b).unwrap();
        assert!(again.is_conforming(), "{:#?}", again.issues);
        assert_eq!(doc.version(), (1, 4));
    }
}
