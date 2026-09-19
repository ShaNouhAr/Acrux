//! Inventaire d'un document pour le contrôle en amont : tout ce que les
//! règles des profils ont besoin de savoir, collecté en une seule passe.
//!
//! On parcourt le catalogue, chaque page, ses ressources (récursivement à
//! travers les XObjects de formulaire) et ses flux de contenu. Le parcours est
//! borné : profondeur, nombre d'objets visités et nombre d'éléments retenus.

use std::collections::{BTreeSet, HashSet};

use acrux_document::{Dict, Document, Name, Object, ObjectRef, Page};
use acrux_render::content::ContentLexer;
use acrux_render::font::LoadedFont;
use acrux_render::page::page_content;

/// Profondeur maximale de récursion dans les ressources.
const MAX_DEPTH: usize = 8;
/// Nombre maximal d'éléments retenus par catégorie.
const MAX_ITEMS: usize = 5_000;

/// Une police employée par le document.
#[derive(Debug, Clone)]
pub struct FontUse {
    /// Nom de base, préfixe de sous-ensemble compris.
    pub name: String,
    /// Le programme de glyphes est incorporé (`/FontFile`, `/FontFile2`, `/FontFile3`).
    pub embedded: bool,
    /// Le nom porte un préfixe de sous-ensemble `ABCDEF+`.
    pub subset: bool,
    /// Le programme incorporé a pu être lu et fournit des glyphes.
    pub complete_subset: bool,
    /// Première page où la police apparaît.
    pub page: Option<usize>,
}

/// Une image employée par le document.
#[derive(Debug, Clone)]
pub struct ImageUse {
    /// `/Interpolate true`.
    pub interpolate: bool,
    /// Page où l'image apparaît.
    pub page: Option<usize>,
}

/// Une intention de sortie du catalogue.
#[derive(Debug, Clone)]
pub struct OutputIntentUse {
    /// `/S` : `GTS_PDFA1`, `GTS_PDFX`…
    pub subtype: String,
    /// `/DestOutputProfile` présent (profil ICC incorporé).
    pub has_profile: bool,
}

/// Inventaire complet.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    /// Polices employées.
    pub fonts: Vec<FontUse>,
    /// Images employées.
    pub images: Vec<ImageUse>,
    /// Intentions de sortie déclarées.
    pub output_intents: Vec<OutputIntentUse>,
    /// Emplois de la transparence : `(page, raison)`.
    pub transparency: Vec<(usize, String)>,
    /// Actions rencontrées : `(page, type)`. `None` = niveau document.
    pub actions: Vec<(Option<usize>, String)>,
    /// Espaces colorimétriques dépendants du périphérique employés.
    pub device_spaces: BTreeSet<String>,
    /// Noms des tons directs (`/Separation`) et des encres `/DeviceN`.
    pub separations: BTreeSet<String>,
    /// Nombre de fichiers incorporés.
    pub embedded_files: usize,
    /// Nombre de flux dont les données sont dans un fichier externe (`/F`).
    pub external_streams: usize,
    /// La surimpression est employée (`/OP` ou `/op` vrai).
    pub overprint: bool,
    /// Contenu du flux `/Metadata` du catalogue, en texte.
    pub metadata: Option<String>,
}

struct Walker<'a> {
    doc: &'a Document,
    scan: Scan,
    seen: HashSet<u32>,
}

impl Scan {
    /// Parcourt le document et rend son inventaire.
    #[must_use]
    pub fn run(doc: &Document, pages: &[Page]) -> Scan {
        let mut w = Walker {
            doc,
            scan: Scan::default(),
            seen: HashSet::new(),
        };
        w.catalog();
        for page in pages {
            w.page(page);
        }
        w.external_streams();
        w.scan
    }
}

impl Walker<'_> {
    fn catalog(&mut self) {
        let Ok(catalog) = self.doc.catalog() else {
            return;
        };
        // Métadonnées XMP.
        if let Some(meta) = self.doc.dict_get(&catalog, "Metadata").ok().flatten() {
            if let Ok(data) = self.doc.stream_data(&meta) {
                self.scan.metadata = Some(String::from_utf8_lossy(&data.data).into_owned());
            }
        }
        // Intentions de sortie.
        if let Some(intents) = self
            .doc
            .dict_get(&catalog, "OutputIntents")
            .ok()
            .flatten()
            .and_then(|o| o.as_array().map(<[Object]>::to_vec))
        {
            for item in &intents {
                let Ok(r) = self.doc.resolve(item) else {
                    continue;
                };
                let Some(d) = r.as_dict() else { continue };
                self.scan.output_intents.push(OutputIntentUse {
                    subtype: d
                        .get(&Name::new("S"))
                        .and_then(Object::as_name)
                        .map(Name::as_str)
                        .unwrap_or_default(),
                    has_profile: d.contains_key(&Name::new("DestOutputProfile")),
                });
            }
        }
        // Actions du document.
        if let Some(open) = self.doc.dict_get(&catalog, "OpenAction").ok().flatten() {
            if let Some(d) = open.as_dict() {
                self.action(None, d);
            }
        }
        self.additional_actions(None, &catalog);
        // Arbres de noms : JavaScript et fichiers incorporés.
        if let Some(names) = self
            .doc
            .dict_get(&catalog, "Names")
            .ok()
            .flatten()
            .and_then(|n| n.as_dict().cloned())
        {
            if names.contains_key(&Name::new("JavaScript")) {
                self.scan.actions.push((None, "JavaScript".to_string()));
            }
            if let Some(files) = self
                .doc
                .dict_get(&names, "EmbeddedFiles")
                .ok()
                .flatten()
                .and_then(|f| f.as_dict().cloned())
            {
                self.scan.embedded_files += count_name_tree(self.doc, &files, 0);
            }
        }
        // `/AcroForm /XFA` : formulaire XFA, incompatible avec tous les profils.
        if let Some(form) = self
            .doc
            .dict_get(&catalog, "AcroForm")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().cloned())
        {
            if form.contains_key(&Name::new("XFA")) {
                self.scan.actions.push((None, "XFA".to_string()));
            }
        }
    }

    fn page(&mut self, page: &Page) {
        let index = page.index;
        // Groupe de transparence de la page.
        if let Some(group) = self
            .doc
            .dict_get(&page.dict, "Group")
            .ok()
            .flatten()
            .and_then(|g| g.as_dict().cloned())
        {
            if group
                .get(&Name::new("S"))
                .and_then(Object::as_name)
                .is_some_and(|n| n.0 == b"Transparency")
            {
                self.push_transparency(index, "groupe de transparence de la page");
            }
        }
        self.additional_actions(Some(index), &page.dict);
        // Annotations : actions, pièces jointes, apparences.
        if let Some(annots) = self
            .doc
            .dict_get(&page.dict, "Annots")
            .ok()
            .flatten()
            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
        {
            for item in &annots {
                let Ok(r) = self.doc.resolve(item) else {
                    continue;
                };
                let Some(d) = r.as_dict().cloned() else {
                    continue;
                };
                if let Some(action) = self
                    .doc
                    .dict_get(&d, "A")
                    .ok()
                    .flatten()
                    .and_then(|a| a.as_dict().cloned())
                {
                    self.action(Some(index), &action);
                }
                self.additional_actions(Some(index), &d);
                let subtype = d
                    .get(&Name::new("Subtype"))
                    .and_then(Object::as_name)
                    .map(Name::as_str)
                    .unwrap_or_default();
                if subtype == "FileAttachment" {
                    self.scan.embedded_files += 1;
                }
            }
        }
        let resources = self
            .doc
            .dict_get(&page.dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
            .unwrap_or_default();
        self.resources(index, &resources, 0);
        let content = page_content(self.doc, page);
        self.content(index, &content);
    }

    fn action(&mut self, page: Option<usize>, action: &Dict) {
        let kind = action
            .get(&Name::new("S"))
            .and_then(Object::as_name)
            .map(Name::as_str)
            .unwrap_or_default();
        if matches!(
            kind.as_str(),
            "JavaScript" | "Launch" | "ImportData" | "SubmitForm"
        ) {
            self.scan.actions.push((page, kind));
        }
    }

    fn additional_actions(&mut self, page: Option<usize>, dict: &Dict) {
        let Some(aa) = self
            .doc
            .dict_get(dict, "AA")
            .ok()
            .flatten()
            .and_then(|a| a.as_dict().cloned())
        else {
            return;
        };
        for value in aa.values() {
            if let Ok(r) = self.doc.resolve(value) {
                if let Some(d) = r.as_dict() {
                    self.action(page, d);
                }
            }
        }
    }

    fn resources(&mut self, page: usize, resources: &Dict, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        if let Some(fonts) = self
            .doc
            .dict_get(resources, "Font")
            .ok()
            .flatten()
            .and_then(|f| f.as_dict().cloned())
        {
            for value in fonts.values() {
                self.font(page, value);
            }
        }
        if let Some(spaces) = self
            .doc
            .dict_get(resources, "ColorSpace")
            .ok()
            .flatten()
            .and_then(|s| s.as_dict().cloned())
        {
            for value in spaces.values() {
                self.colorspace(value, 0);
            }
        }
        if let Some(states) = self
            .doc
            .dict_get(resources, "ExtGState")
            .ok()
            .flatten()
            .and_then(|s| s.as_dict().cloned())
        {
            for value in states.values() {
                if let Ok(r) = self.doc.resolve(value) {
                    if let Some(d) = r.as_dict().cloned() {
                        self.ext_g_state(page, &d);
                    }
                }
            }
        }
        if let Some(xobjects) = self
            .doc
            .dict_get(resources, "XObject")
            .ok()
            .flatten()
            .and_then(|x| x.as_dict().cloned())
        {
            for value in xobjects.values() {
                self.xobject(page, value, depth);
            }
        }
    }

    fn font(&mut self, page: usize, entry: &Object) {
        if let Object::Reference(r) = entry {
            if !self.seen.insert(r.number) {
                return;
            }
        }
        let Ok(resolved) = self.doc.resolve(entry) else {
            return;
        };
        let Some(dict) = resolved.as_dict().cloned() else {
            return;
        };
        if self.scan.fonts.len() >= MAX_ITEMS {
            return;
        }
        let subtype = self
            .doc
            .dict_get(&dict, "Subtype")
            .ok()
            .flatten()
            .and_then(|o| o.as_name().map(Name::as_str))
            .unwrap_or_default();
        let name = self
            .doc
            .dict_get(&dict, "BaseFont")
            .ok()
            .flatten()
            .and_then(|o| o.as_name().map(Name::as_str))
            .unwrap_or_else(|| subtype.clone());
        // Type 3 : les glyphes sont des flux de contenu, toujours « incorporés ».
        if subtype == "Type3" {
            self.scan.fonts.push(FontUse {
                name,
                embedded: true,
                subset: false,
                complete_subset: true,
                page: Some(page),
            });
            return;
        }
        // Type 0 : le descripteur est sur la police descendante.
        let descriptor_owner = if subtype == "Type0" {
            self.doc
                .dict_get(&dict, "DescendantFonts")
                .ok()
                .flatten()
                .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
                .and_then(|d| self.doc.resolve(&d).ok().and_then(|r| r.as_dict().cloned()))
                .unwrap_or_else(|| dict.clone())
        } else {
            dict.clone()
        };
        let descriptor = self
            .doc
            .dict_get(&descriptor_owner, "FontDescriptor")
            .ok()
            .flatten()
            .and_then(|d| d.as_dict().cloned());
        let embedded = descriptor.as_ref().is_some_and(|d| {
            ["FontFile", "FontFile2", "FontFile3"]
                .iter()
                .any(|k| d.contains_key(&Name::new(k)))
        });
        let subset = name
            .split_once('+')
            .is_some_and(|(p, _)| p.len() == 6 && p.bytes().all(|b| b.is_ascii_uppercase()));
        let complete_subset =
            !embedded || LoadedFont::load(self.doc, &dict).is_ok_and(|f| !f.substituted);
        self.scan.fonts.push(FontUse {
            name,
            embedded,
            subset,
            complete_subset,
            page: Some(page),
        });
    }

    fn colorspace(&mut self, entry: &Object, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(resolved) = self.doc.resolve(entry) else {
            return;
        };
        match &*resolved {
            Object::Name(n) => {
                let name = n.as_str();
                if name.starts_with("Device") || name == "CalRGB" || name == "Lab" {
                    self.scan.device_spaces.insert(name);
                }
            }
            Object::Array(items) => {
                let Some(family) = items.first().and_then(Object::as_name).map(Name::as_str) else {
                    return;
                };
                match family.as_str() {
                    "Separation" => {
                        if let Some(name) = items.get(1).and_then(Object::as_name) {
                            self.scan.separations.insert(name.as_str());
                        }
                        if let Some(alt) = items.get(2) {
                            self.colorspace(alt, depth + 1);
                        }
                    }
                    "DeviceN" => {
                        if let Some(names) = items.get(1).and_then(Object::as_array) {
                            for n in names {
                                if let Some(n) = n.as_name() {
                                    self.scan.separations.insert(n.as_str());
                                }
                            }
                        }
                        if let Some(alt) = items.get(2) {
                            self.colorspace(alt, depth + 1);
                        }
                    }
                    "Indexed" => {
                        if let Some(base) = items.get(1) {
                            self.colorspace(base, depth + 1);
                        }
                    }
                    "CalRGB" | "Lab" => {
                        self.scan.device_spaces.insert(family);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ext_g_state(&mut self, page: usize, state: &Dict) {
        let number = |key: &str| {
            self.doc
                .dict_get(state, key)
                .ok()
                .flatten()
                .and_then(|o| o.as_f64())
        };
        if number("CA").is_some_and(|v| v < 1.0) || number("ca").is_some_and(|v| v < 1.0) {
            self.push_transparency(page, "opacité constante /CA ou /ca inférieure à 1");
        }
        if let Some(bm) = self.doc.dict_get(state, "BM").ok().flatten() {
            let names: Vec<String> = match &*bm {
                Object::Name(n) => vec![n.as_str()],
                Object::Array(a) => a
                    .iter()
                    .filter_map(|o| o.as_name().map(Name::as_str))
                    .collect(),
                _ => Vec::new(),
            };
            if names.iter().any(|n| n != "Normal" && n != "Compatible") {
                self.push_transparency(page, "mode de fusion /BM autre que /Normal");
            }
        }
        if let Some(smask) = self.doc.dict_get(state, "SMask").ok().flatten() {
            let none = matches!(&*smask, Object::Name(n) if n.0 == b"None");
            if !none {
                self.push_transparency(page, "masque souple /SMask dans un /ExtGState");
            }
        }
        for key in ["OP", "op"] {
            if matches!(
                self.doc.dict_get(state, key).ok().flatten().as_deref(),
                Some(Object::Bool(true))
            ) {
                self.scan.overprint = true;
            }
        }
    }

    fn xobject(&mut self, page: usize, entry: &Object, depth: usize) {
        if let Object::Reference(r) = entry {
            if !self.seen.insert(r.number) {
                return;
            }
        }
        let Ok(resolved) = self.doc.resolve(entry) else {
            return;
        };
        let Some(dict) = resolved.as_dict().cloned() else {
            return;
        };
        let subtype = self
            .doc
            .dict_get(&dict, "Subtype")
            .ok()
            .flatten()
            .and_then(|o| o.as_name().map(Name::as_str))
            .unwrap_or_default();
        if subtype == "Image" {
            if self.scan.images.len() < MAX_ITEMS {
                let interpolate = matches!(
                    self.doc
                        .dict_get(&dict, "Interpolate")
                        .ok()
                        .flatten()
                        .as_deref(),
                    Some(Object::Bool(true))
                );
                self.scan.images.push(ImageUse {
                    interpolate,
                    page: Some(page),
                });
            }
            if dict.contains_key(&Name::new("SMask")) {
                self.push_transparency(page, "image avec masque souple /SMask");
            }
            if let Some(cs) = dict.get(&Name::new("ColorSpace")) {
                self.colorspace(cs, 0);
            }
            return;
        }
        if subtype != "Form" {
            return;
        }
        if let Some(group) = self
            .doc
            .dict_get(&dict, "Group")
            .ok()
            .flatten()
            .and_then(|g| g.as_dict().cloned())
        {
            if group
                .get(&Name::new("S"))
                .and_then(Object::as_name)
                .is_some_and(|n| n.0 == b"Transparency")
            {
                self.push_transparency(page, "groupe de transparence d'un XObject de formulaire");
            }
        }
        if let Some(inner) = self
            .doc
            .dict_get(&dict, "Resources")
            .ok()
            .flatten()
            .and_then(|r| r.as_dict().cloned())
        {
            self.resources(page, &inner, depth + 1);
        }
        if let Ok(data) = self.doc.stream_data(&resolved) {
            self.content(page, &data.data);
        }
    }

    /// Opérateurs de couleur du flux : ils emploient des espaces dépendants du
    /// périphérique sans passer par les ressources (§8.6.8).
    fn content(&mut self, _page: usize, content: &[u8]) {
        let mut lexer = ContentLexer::new(content);
        let mut ops = 0usize;
        while let Ok(Some(op)) = lexer.next_operation() {
            ops += 1;
            if ops > 200_000 {
                return;
            }
            let space = match op.operator.as_slice() {
                b"g" | b"G" => "DeviceGray",
                b"rg" | b"RG" => "DeviceRGB",
                b"k" | b"K" => "DeviceCMYK",
                _ => continue,
            };
            self.scan.device_spaces.insert(space.to_string());
        }
    }

    fn push_transparency(&mut self, page: usize, reason: &str) {
        if self.scan.transparency.len() < MAX_ITEMS
            && !self
                .scan
                .transparency
                .iter()
                .any(|(p, r)| *p == page && r == reason)
        {
            self.scan.transparency.push((page, reason.to_string()));
        }
    }

    /// Flux dont les données sont dans un fichier externe (`/F`, §7.3.8.2).
    fn external_streams(&mut self) {
        for number in self.doc.object_numbers() {
            let r = ObjectRef {
                number,
                generation: 0,
            };
            let Ok(obj) = self.doc.get(r) else { continue };
            if let Object::Stream { dict, .. } = &*obj {
                if dict.contains_key(&Name::new("F")) {
                    self.scan.external_streams += 1;
                }
            }
        }
    }
}

/// Nombre de feuilles d'un arbre de noms (§7.9.6).
fn count_name_tree(doc: &Document, node: &Dict, depth: usize) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    if let Some(names) = doc
        .dict_get(node, "Names")
        .ok()
        .flatten()
        .and_then(|n| n.as_array().map(<[Object]>::len))
    {
        return names / 2;
    }
    let Some(kids) = doc
        .dict_get(node, "Kids")
        .ok()
        .flatten()
        .and_then(|k| k.as_array().map(<[Object]>::to_vec))
    else {
        return 0;
    };
    kids.iter()
        .filter_map(|k| doc.resolve(k).ok().and_then(|r| r.as_dict().cloned()))
        .map(|d| count_name_tree(doc, &d, depth + 1))
        .sum()
}
