//! Lecture et **mise à jour chirurgicale** d'un paquet XMP (RDF/XML,
//! XMP Specification Part 1 §7 ; PDF §14.3.2).
//!
//! Un paquet XMP contient bien plus que les huit champs de `/Info` :
//! droits d'auteur, historique des révisions, identifiants d'un logiciel de
//! mise en page, schémas propres à un métier. Réécrire le paquet de zéro
//! pour corriger un titre détruirait tout cela. Ce module fait donc
//! l'inverse : il **remplace le seul élément visé** et laisse le reste du
//! fichier octet pour octet identique.
//!
//! Ce n'est pas un analyseur XML général et cela n'a pas à l'être : un
//! paquet XMP est produit par des machines, sans espaces de noms redéfinis
//! en cours de route ni éléments de même nom imbriqués. On repère donc les
//! éléments par leur nom qualifié (`dc:title`), en sautant correctement les
//! commentaires, les instructions de traitement, les sections CDATA et les
//! valeurs d'attributs entre guillemets.

use std::fmt::Write as _;

/// Forme RDF d'une propriété (XMP Part 1 §7.5 à §7.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    /// Valeur simple : `<pdf:Producer>texte</pdf:Producer>`.
    Simple,
    /// Texte localisé : `<dc:title><rdf:Alt><rdf:li xml:lang="x-default">…`.
    AltText,
    /// Suite ordonnée : `<dc:creator><rdf:Seq><rdf:li>…`.
    SeqText,
}

/// Espace de noms associé à un préfixe connu, pour pouvoir créer le bloc
/// `rdf:Description` qui manque.
pub(crate) fn namespace_of(prefix: &str) -> Option<&'static str> {
    Some(match prefix {
        "dc" => "http://purl.org/dc/elements/1.1/",
        "xmp" => "http://ns.adobe.com/xap/1.0/",
        "pdf" => "http://ns.adobe.com/pdf/1.3/",
        "xmpMM" => "http://ns.adobe.com/xap/1.0/mm/",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Balayage
// ---------------------------------------------------------------------------

/// Une balise repérée dans le document.
#[derive(Debug, Clone, Copy)]
struct Tag {
    /// Position du `<`.
    start: usize,
    /// Position juste après le `>`.
    end: usize,
    /// Bornes du nom qualifié dans la chaîne.
    name: (usize, usize),
    /// `</nom>`.
    closing: bool,
    /// `<nom … />`.
    self_closing: bool,
}

/// Balises du document, dans l'ordre, commentaires et CDATA exclus.
///
/// Renvoie une liste vide plutôt qu'une erreur si le paquet est tronqué :
/// une métadonnée mal formée ne doit jamais empêcher d'ouvrir un document.
fn tags(xml: &str) -> Vec<Tag> {
    let bytes = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &xml[i..];
        // Commentaire, CDATA, déclaration, instruction de traitement : sautés.
        if let Some(skip) = skip_special(rest) {
            i += skip;
            continue;
        }
        let closing = rest.starts_with("</");
        let name_start = i + if closing { 2 } else { 1 };
        let name_end = name_start
            + xml[name_start..]
                .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .unwrap_or(xml.len() - name_start);
        let Some(end) = end_of_tag(xml, i) else { break };
        let self_closing = xml[..end].ends_with("/>");
        out.push(Tag {
            start: i,
            end,
            name: (name_start, name_end),
            closing,
            self_closing,
        });
        i = end;
    }
    out
}

/// Longueur à sauter si `rest` commence par un commentaire, une section
/// CDATA, une déclaration `<!…>` ou une instruction `<?…?>`.
fn skip_special(rest: &str) -> Option<usize> {
    for (open, close) in [
        ("<!--", "-->"),
        ("<![CDATA[", "]]>"),
        ("<?", "?>"),
        ("<!", ">"),
    ] {
        if let Some(after) = rest.strip_prefix(open) {
            return Some(match after.find(close) {
                Some(p) => open.len() + p + close.len(),
                None => rest.len(),
            });
        }
    }
    None
}

/// Position juste après le `>` qui ferme la balise ouverte en `start`,
/// en ignorant les `>` situés dans une valeur d'attribut.
fn end_of_tag(xml: &str, start: usize) -> Option<usize> {
    let bytes = xml.as_bytes();
    let mut quote: Option<u8> = None;
    for (offset, &b) in bytes.iter().enumerate().skip(start) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(offset + 1),
                _ => {}
            },
        }
    }
    None
}

/// Bornes `(début du `<`, fin après le `>` fermant)` du premier élément
/// nommé `name`, contenu compris.
fn element_span(xml: &str, name: &str) -> Option<(usize, usize)> {
    let list = tags(xml);
    let mut it = list.iter().enumerate();
    let (index, open) = it.find(|(_, t)| !t.closing && &xml[t.name.0..t.name.1] == name)?;
    if open.self_closing {
        return Some((open.start, open.end));
    }
    let mut depth = 1usize;
    for t in &list[index + 1..] {
        if &xml[t.name.0..t.name.1] != name || t.self_closing {
            continue;
        }
        if t.closing {
            depth -= 1;
            if depth == 0 {
                return Some((open.start, t.end));
            }
        } else {
            depth += 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------

/// Valeur textuelle de la propriété `name` (« dc:title », « pdf:Producer »…).
///
/// Cherche d'abord la forme élément, puis la forme attribut d'un
/// `rdf:Description` (les deux sont légales et les deux se rencontrent).
/// Les enveloppes `rdf:Alt`, `rdf:Seq` et `rdf:Bag` sont traversées : seule
/// la première valeur `rdf:li` est rendue.
pub(crate) fn read_property(xml: &str, name: &str) -> Option<String> {
    if let Some((start, end)) = element_span(xml, name) {
        let inner = inner_text_of(&xml[start..end], name);
        return inner.map(|t| unescape(t.trim()));
    }
    read_attribute(xml, name)
}

/// Contenu d'un élément dont on connaît les bornes, enveloppes RDF ôtées.
fn inner_text_of<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let open_end = element.find('>')? + 1;
    if element[..open_end].ends_with("/>") {
        return Some("");
    }
    let close = element.rfind(&format!("</{name}"))?;
    let mut inner = &element[open_end..close];
    for wrapper in ["rdf:Alt", "rdf:Seq", "rdf:Bag"] {
        if let Some((s, e)) = element_span(inner, wrapper) {
            let sub = &inner[s..e];
            let open = sub.find('>')? + 1;
            let end = sub.rfind(&format!("</{wrapper}"))?;
            inner = &sub[open..end];
            break;
        }
    }
    if let Some((s, e)) = element_span(inner, "rdf:li") {
        let sub = &inner[s..e];
        let open = sub.find('>')? + 1;
        let end = sub.rfind("</rdf:li").unwrap_or(sub.len());
        inner = &sub[open..end.max(open)];
    }
    Some(inner)
}

/// Valeur d'un attribut `name="…"` porté par un `rdf:Description`.
fn read_attribute(xml: &str, name: &str) -> Option<String> {
    for tag in tags(xml) {
        if tag.closing || &xml[tag.name.0..tag.name.1] != "rdf:Description" {
            continue;
        }
        if let Some((_, _, value)) = attribute_span(&xml[tag.start..tag.end], name) {
            return Some(unescape(&value));
        }
    }
    None
}

/// `(début, fin, valeur)` de l'attribut `name` dans une balise ouvrante.
fn attribute_span(tag: &str, name: &str) -> Option<(usize, usize, String)> {
    let mut from = 0usize;
    while let Some(pos) = tag[from..].find(name) {
        let at = from + pos;
        from = at + name.len();
        // Le nom doit être précédé d'un espace et suivi de `=`.
        if !tag[..at].ends_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let after = tag[from..].trim_start();
        if !after.starts_with('=') {
            continue;
        }
        let eq = from + tag[from..].find('=')? + 1;
        let value_part = tag[eq..].trim_start();
        let quote = value_part.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value_start = eq + (tag[eq..].len() - value_part.len()) + 1;
        let value_end = value_start + tag[value_start..].find(quote)?;
        return Some((at, value_end + 1, tag[value_start..value_end].to_string()));
    }
    None
}

// ---------------------------------------------------------------------------
// Écriture
// ---------------------------------------------------------------------------

/// Écrit (ou retire, si `value` vaut `None`) la propriété `name` dans le
/// paquet, sans toucher à quoi que ce soit d'autre.
///
/// La forme attribut est d'abord supprimée : sans cela, un lecteur verrait
/// deux fois la même propriété avec deux valeurs différentes.
pub(crate) fn set_property(xml: &str, name: &str, value: Option<&str>, form: Form) -> String {
    let mut out = remove_attribute(xml, name);
    let rendered = value.map(|v| render_property(name, v, form));
    match (element_span(&out, name), rendered) {
        (Some((start, end)), Some(text)) => {
            // Indentation de l'élément remplacé, pour ne pas casser la mise en forme.
            let indent = indent_before(&out, start);
            out.replace_range(start..end, &text.replace('\n', &format!("\n{indent}")));
        }
        (Some((start, end)), None) => {
            let from = line_start(&out, start);
            let to = if out[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            out.replace_range(from..to, "");
        }
        (None, Some(text)) => insert_property(&mut out, name, &text),
        (None, None) => {}
    }
    out
}

/// Sérialisation d'une propriété dans la forme RDF qui lui convient.
fn render_property(name: &str, value: &str, form: Form) -> String {
    let escaped = escape(value);
    match form {
        Form::Simple => format!("<{name}>{escaped}</{name}>"),
        Form::AltText => format!(
            "<{name}>\n  <rdf:Alt>\n    <rdf:li xml:lang=\"x-default\">{escaped}</rdf:li>\n  </rdf:Alt>\n</{name}>"
        ),
        Form::SeqText => format!(
            "<{name}>\n  <rdf:Seq>\n    <rdf:li>{escaped}</rdf:li>\n  </rdf:Seq>\n</{name}>"
        ),
    }
}

/// Insère une propriété absente : dans le `rdf:Description` qui déclare déjà
/// son préfixe, sinon dans un nouveau bloc placé avant `</rdf:RDF>`.
fn insert_property(xml: &mut String, name: &str, text: &str) {
    let prefix = name.split(':').next().unwrap_or(name);
    let declaration = format!("xmlns:{prefix}=");
    for tag in tags(xml) {
        if tag.closing
            || tag.self_closing
            || &xml[tag.name.0..tag.name.1] != "rdf:Description"
            || !xml[tag.start..tag.end].contains(&declaration)
        {
            continue;
        }
        let Some((start, end)) = element_span(&xml[tag.start..], "rdf:Description") else {
            continue;
        };
        let close = tag.start
            + start
            + xml[tag.start + start..tag.start + end]
                .rfind("</rdf:Description")
                .unwrap_or(0);
        let indent = indent_before(xml, close);
        let body = text.replace('\n', &format!("\n{indent}  "));
        xml.insert_str(close, &format!("  {body}\n{indent}"));
        return;
    }
    // Aucun bloc ne déclare ce préfixe : on en crée un.
    let Some(namespace) = namespace_of(prefix) else {
        return;
    };
    let Some(close) = xml.find("</rdf:RDF>") else {
        return;
    };
    let indent = indent_before(xml, close);
    let body = text.replace('\n', &format!("\n{indent}    "));
    let mut block = String::new();
    let _ = write!(
        block,
        "  <rdf:Description rdf:about=\"\" xmlns:{prefix}=\"{namespace}\">\n{indent}    {body}\n{indent}  </rdf:Description>\n{indent}"
    );
    xml.insert_str(close, &block);
}

/// Retire l'attribut `name` de toutes les balises `rdf:Description`.
fn remove_attribute(xml: &str, name: &str) -> String {
    let mut out = xml.to_string();
    loop {
        let mut removed = false;
        for tag in tags(&out) {
            if tag.closing || &out[tag.name.0..tag.name.1] != "rdf:Description" {
                continue;
            }
            if let Some((start, end, _)) = attribute_span(&out[tag.start..tag.end], name) {
                let from = tag.start + start;
                let to = tag.start + end;
                // L'espace qui précède l'attribut part avec lui.
                let from = out[..from]
                    .rfind(|c: char| !c.is_whitespace())
                    .map_or(from, |p| p + 1);
                out.replace_range(from..to, "");
                removed = true;
                break;
            }
        }
        if !removed {
            return out;
        }
    }
}

/// Blancs en début de la ligne qui contient `position`.
fn indent_before(xml: &str, position: usize) -> String {
    let start = line_start(xml, position);
    xml[start..position]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Début de la ligne contenant `position`.
fn line_start(xml: &str, position: usize) -> usize {
    xml[..position].rfind('\n').map_or(0, |p| p + 1)
}

// ---------------------------------------------------------------------------
// Échappement
// ---------------------------------------------------------------------------

/// Échappe le texte pour du XML.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Inverse de [`escape`], balises internes éventuelles ôtées.
///
/// Les entités numériques `&#xNN;` et `&#NN;` sont résolues ; une entité
/// inconnue est laissée telle quelle plutôt que perdue.
pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(['&', '<']) {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        if rest.starts_with('<') {
            // Balise résiduelle (un `rdf:li` supplémentaire) : ignorée.
            match rest.find('>') {
                Some(p) => rest = &rest[p + 1..],
                None => return out,
            }
            continue;
        }
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        rest = &rest[end + 1..];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => push_code(&mut out, &e[2..], 16),
            e if e.starts_with('#') => push_code(&mut out, &e[1..], 10),
            e => {
                let _ = write!(out, "&{e};");
            }
        }
    }
    out.push_str(rest);
    out
}

/// Ajoute le caractère de point de code `digits` écrit en base `radix`.
fn push_code(out: &mut String, digits: &str, radix: u32) {
    match u32::from_str_radix(digits, radix)
        .ok()
        .and_then(char::from_u32)
    {
        Some(c) => out.push(c),
        None => out.push('\u{fffd}'),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // tests
mod tests {
    use super::*;

    const PACKET: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
  <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
      <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Ancien titre</rdf:li></rdf:Alt></dc:title>
      <dc:rights><rdf:Alt><rdf:li xml:lang="x-default">© Personne</rdf:li></rdf:Alt></dc:rights>
    </rdf:Description>
    <rdf:Description rdf:about="" xmlns:pdf="http://ns.adobe.com/pdf/1.3/" pdf:Producer="Vieil outil">
      <pdf:Trapped>False</pdf:Trapped>
    </rdf:Description>
  </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    #[test]
    fn reads_element_and_attribute_forms() {
        assert_eq!(
            read_property(PACKET, "dc:title").as_deref(),
            Some("Ancien titre")
        );
        assert_eq!(
            read_property(PACKET, "pdf:Producer").as_deref(),
            Some("Vieil outil")
        );
        assert_eq!(
            read_property(PACKET, "pdf:Trapped").as_deref(),
            Some("False")
        );
        assert_eq!(read_property(PACKET, "dc:creator"), None);
    }

    #[test]
    fn replacing_a_property_keeps_every_other_one() {
        let out = set_property(PACKET, "dc:title", Some("Nouveau"), Form::AltText);
        assert_eq!(read_property(&out, "dc:title").as_deref(), Some("Nouveau"));
        assert_eq!(
            read_property(&out, "dc:rights").as_deref(),
            Some("© Personne")
        );
        assert_eq!(read_property(&out, "pdf:Trapped").as_deref(), Some("False"));
        assert!(out.contains("dc:rights"));
    }

    #[test]
    fn the_attribute_form_is_replaced_by_an_element() {
        let out = set_property(PACKET, "pdf:Producer", Some("Acrux"), Form::Simple);
        assert!(!out.contains("pdf:Producer=\""));
        assert_eq!(
            read_property(&out, "pdf:Producer").as_deref(),
            Some("Acrux")
        );
        assert_eq!(read_property(&out, "pdf:Trapped").as_deref(), Some("False"));
    }

    #[test]
    fn an_absent_property_is_inserted_in_the_right_block() {
        let out = set_property(PACKET, "dc:creator", Some("Camille"), Form::SeqText);
        assert_eq!(
            read_property(&out, "dc:creator").as_deref(),
            Some("Camille")
        );
        // Insérée dans le bloc dc, pas dans un nouveau.
        assert_eq!(out.matches("xmlns:dc=").count(), 1);
    }

    #[test]
    fn an_unknown_prefix_gets_its_own_description_block() {
        let out = set_property(
            PACKET,
            "xmp:CreateDate",
            Some("2024-01-15T10:30:00Z"),
            Form::Simple,
        );
        assert_eq!(
            read_property(&out, "xmp:CreateDate").as_deref(),
            Some("2024-01-15T10:30:00Z")
        );
        assert!(out.contains("xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\""));
        assert_eq!(
            read_property(&out, "dc:rights").as_deref(),
            Some("© Personne")
        );
    }

    #[test]
    fn removing_a_property_leaves_the_others() {
        let out = set_property(PACKET, "dc:title", None, Form::AltText);
        assert_eq!(read_property(&out, "dc:title"), None);
        assert_eq!(
            read_property(&out, "dc:rights").as_deref(),
            Some("© Personne")
        );
    }

    #[test]
    fn escaping_round_trips() {
        let value = "a & b < c > d \" e ' f";
        let out = set_property(PACKET, "dc:title", Some(value), Form::AltText);
        assert_eq!(read_property(&out, "dc:title").as_deref(), Some(value));
    }

    #[test]
    fn comments_and_cdata_do_not_hide_elements() {
        let packet = "<rdf:RDF xmlns:rdf=\"x\"><!-- <dc:title>faux</dc:title> -->\
             <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
             <dc:title>vrai</dc:title></rdf:Description></rdf:RDF>";
        assert_eq!(read_property(packet, "dc:title").as_deref(), Some("vrai"));
    }

    #[test]
    fn numeric_entities_are_resolved() {
        assert_eq!(unescape("caf&#xE9; &#233;t&#xe9;"), "café été");
        assert_eq!(unescape("&inconnu;"), "&inconnu;");
    }
}
