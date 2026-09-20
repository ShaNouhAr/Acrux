//! Frappe en direct : le texte est mis en page **en mémoire**, et dessiné par
//! l'application pendant qu'on tape.
//!
//! # Pourquoi
//!
//! Recomposer un paragraphe dans le document demande d'en extraire le texte,
//! de balayer le flux et de le réécrire, puis de rendre la page entière : une
//! trentaine de millisecondes sur une page ordinaire, davantage sur une page
//! chargée. À chaque lettre, cela se sent — et la page se redessine sous les
//! doigts.
//!
//! Ici, rien de tout cela : la police du bloc est chargée **une fois** à
//! l'ouverture, et chaque frappe ne fait plus qu'une mise en page en mémoire,
//! de l'ordre de quelques microsecondes. L'application dessine elle-même les
//! glyphes ainsi placés, et le document n'est réécrit qu'à la sortie du bloc.
//!
//! La mise en page est celle de [`super::reflow`] : ce qu'on voit en tapant
//! est donc, au pixel près, ce qui sera écrit.
//!
//! # Les lettres qui manquent
//!
//! Une police incorporée dans un PDF est presque toujours **sous-ensemblée**
//! : elle ne porte que les lettres déjà présentes sur la page. Taper un « w »
//! dans un document qui n'en contient aucun ne donnerait donc rien à voir.
//!
//! Le temps de la frappe, ces glyphes sont empruntés à une police système de
//! la même famille ([`Face::Spare`]) — exactement celle dont l'écriture se
//! servira pour compléter la police du document. Ce qu'on voit est donc déjà
//! ce qu'on obtiendra.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use acrux_core::{Matrix, Path, Result};
use acrux_document::{Dict, Document, Name, Object, Page};
use acrux_fonts::TrueTypeFont;
use acrux_render::font::GlyphRef;

use super::encode;
use super::reflow::{caret_from_layout, lay_out_with, CaretMap, Geometry, ParagraphFrame};
use super::runs::Styles;

/// Où prendre le dessin d'un caractère dans une police donnée.
///
/// Un caractère en vaut parfois **deux** : une ligature que la police ne
/// connaît pas s'écrit en lettres (« ﬁ » devient « f » puis « i »), et il
/// faut alors dessiner les deux.
fn faces_in(font: &encode::Prepared, c: char) -> Vec<(Face, f64)> {
    let encoded = font.encode(&c.to_string());
    if !encoded.lost.is_empty() {
        return Vec::new();
    }
    font.font
        .decode(&encoded.bytes)
        .into_iter()
        .zip(encoded.widths.iter())
        .map(|(glyph, (width, _))| (Face::Own(glyph), *width))
        .collect()
}

/// Ce qu'une police doit connaître pour qu'on tape sans secours.
const COMMON: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 \
     éèêëàâäîïôöùûüçÉÈÀÇ,.;:!?'()-/%€$&@+=";

/// Police et mesures d'un bloc, gardées le temps d'une saisie.
pub struct LiveText {
    /// Police du document : table des codes, largeurs, contours.
    prepared: encode::Prepared,
    /// Une police par style du bloc, quand il en mêle plusieurs — du gras,
    /// de l'italique, un lien. Vide quand le bloc n'a qu'un style.
    runs: Vec<encode::Prepared>,
    /// Police de secours, pour les lettres absentes du sous-ensemble.
    spare: Option<Spare>,
    /// Encre du bloc.
    color: [f32; 3],
}

/// Police système de secours, et ses contours déjà tracés.
struct Spare {
    /// Police lue sur le disque.
    font: TrueTypeFont,
    /// Contours déjà demandés, par identifiant de glyphe.
    paths: RefCell<HashMap<u16, Option<Rc<Path>>>>,
}

/// Un texte mis en page, sans que rien ait été écrit.
#[derive(Debug, Clone, Default)]
pub struct LaidText {
    /// Position de chaque frontière de caractère.
    pub caret: CaretMap,
    /// Lignes dessinées.
    pub lines: Vec<LaidTextLine>,
}

/// Une ligne posée.
#[derive(Debug, Clone)]
pub struct LaidTextLine {
    /// Ce qui est dessiné, blancs de fin exclus.
    pub text: String,
    /// Début de la ligne, en espace de page.
    pub x: f64,
    /// Ligne de base, en espace de page.
    pub baseline: f64,
    /// Élargissement de chaque blanc pour justifier.
    pub gap: f64,
}

/// Ce qu'il faut pour poser un caractère : ses dessins et leurs avances, son
/// corps, son encre, et le style d'où il vient.
type Placement = (Vec<(Face, f64)>, f64, [f32; 3], Option<usize>);

/// Un glyphe placé, prêt à dessiner.
#[derive(Debug, Clone)]
pub struct PlacedGlyph {
    /// Où prendre son dessin.
    pub face: Face,
    /// Origine du glyphe, en espace de page.
    pub x: f64,
    /// Ligne de base, en espace de page.
    pub baseline: f64,
    /// Corps en points de page.
    pub size: f64,
    /// Encre.
    pub color: [f32; 3],
    /// Style d'où vient le glyphe, s'il ne vient pas de la police du bloc.
    ///
    /// Sans lui, on chercherait le contour d'un « q » gras dans la police
    /// romaine : il y manquerait, ou ce serait le mauvais dessin.
    pub run: Option<usize>,
}

/// Provenance du dessin d'un glyphe.
#[derive(Debug, Clone)]
pub enum Face {
    /// Glyphe de la police du document.
    Own(GlyphRef),
    /// Glyphe emprunté à la police système, le temps de la frappe.
    Spare(u16),
}

impl LiveText {
    /// Prépare la saisie d'un bloc : charge sa police et retient son encre.
    ///
    /// Le document n'est pas touché — pas même pour une zone de texte neuve,
    /// dont la police standard n'est ajoutée qu'au moment d'écrire.
    ///
    /// # Errors
    /// Police du bloc introuvable ou illisible.
    pub fn open(doc: &Document, page: &Page, frame: &ParagraphFrame) -> Result<Self> {
        let dict = font_dict(doc, page, frame)?;
        let prepared = encode::measuring(doc, &dict, frame.font.clone())?;
        // La police de secours ne se cherche que si elle sert : une police
        // complète n'a besoin de personne, et lire le dossier des polices
        // système coûte quelques dizaines de millisecondes.
        let spare = if prepared.encode(COMMON).lost.is_empty() {
            None
        } else {
            encode::spare_font(&prepared.font.base_font).map(|font| Spare {
                font,
                paths: RefCell::new(HashMap::new()),
            })
        };
        #[allow(clippy::cast_possible_truncation)]
        let color = [
            frame.color[0] as f32,
            frame.color[1] as f32,
            frame.color[2] as f32,
        ];
        Ok(Self {
            prepared,
            runs: Vec::new(),
            spare,
            color,
        })
    }

    /// Charge en plus les polices des **styles** du bloc.
    ///
    /// Sans elles, l'aperçu montrerait un bloc d'une seule police alors que
    /// l'écriture en rendrait plusieurs : ce qu'on voit ne serait plus ce
    /// qu'on obtient.
    ///
    /// # Errors
    /// Police du bloc introuvable ou illisible.
    pub fn open_styled(
        doc: &Document,
        page: &Page,
        frame: &ParagraphFrame,
        styles: &Styles,
    ) -> Result<Self> {
        let mut live = Self::open(doc, page, frame)?;
        if styles.uniform() {
            return Ok(live);
        }
        for run in &styles.runs {
            let prepared = super::font_dict_of(doc, page, &run.font)
                .or_else(|_| {
                    font_in_forms(doc, page, &run.font)
                        .ok_or_else(|| acrux_core::Error::Corrupt("police de style".into()))
                })
                .and_then(|dict| encode::measuring(doc, &dict, run.font.clone()));
            match prepared {
                Ok(p) => live.runs.push(p),
                // Une police qui se dérobe : cette tranche se dessinera avec
                // celle du bloc, comme elle s'écrira.
                Err(_) => live.runs.push(encode::measuring(
                    doc,
                    &font_dict(doc, page, frame)?,
                    frame.font.clone(),
                )?),
            }
        }
        Ok(live)
    }

    /// Impose l'encre du bloc.
    pub fn set_color(&mut self, color: [f32; 3]) {
        self.color = color;
    }

    /// Encre du bloc.
    #[must_use]
    pub fn color(&self) -> [f32; 3] {
        self.color
    }

    /// Nom de la police du bloc, tel qu'il sera montré à l'utilisateur.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.prepared.font.base_font
    }

    /// Vrai si tous les caractères sont dans la police du document.
    ///
    /// Les autres sont empruntés à la police système le temps de la frappe ;
    /// c'est l'écriture qui les ajoutera vraiment au fichier.
    #[must_use]
    pub fn covers(&self, text: &str) -> bool {
        self.prepared
            .encode(&text.chars().filter(|c| *c != '\n').collect::<String>())
            .lost
            .is_empty()
    }

    /// Vrai si tout ce qui est tapé peut être dessiné, d'une police ou de
    /// l'autre.
    #[must_use]
    pub fn can_draw(&self, text: &str) -> bool {
        text.chars()
            .filter(|c| *c != '\n' && *c != ' ')
            .all(|c| !self.faces(c).is_empty())
    }

    /// Met `text` en page dans `frame`, sans rien écrire.
    #[must_use]
    pub fn lay(&self, frame: &ParagraphFrame, text: &str) -> LaidText {
        self.lay_styled(frame, text, None)
    }

    /// Met `text` en page en donnant à chaque caractère **son** style.
    #[must_use]
    pub fn lay_styled(
        &self,
        frame: &ParagraphFrame,
        text: &str,
        styles: Option<&Styles>,
    ) -> LaidText {
        let chars: Vec<char> = text.chars().collect();
        let geometry = Geometry::from(frame);
        let styles = styles.filter(|s| !s.uniform() && !self.runs.is_empty());
        let laid = lay_out_with(&chars, &geometry, |i, c| {
            let Some(styles) = styles else {
                return self.em(c) * geometry.size;
            };
            let id = styles.per_char.get(i).copied().unwrap_or(0) as usize;
            let font = self.runs.get(id).unwrap_or(&self.prepared);
            let measured = font.encode(&c.to_string());
            let em = if measured.lost.is_empty() {
                measured.widths.iter().map(|(w, _)| w).sum::<f64>()
            } else {
                self.em(c)
            };
            em * styles.size_at(i, geometry.size)
        });
        let lines = laid
            .iter()
            .map(|l| LaidTextLine {
                text: chars[l.start.min(chars.len())..l.draw_end.min(chars.len())]
                    .iter()
                    .collect(),
                x: l.x,
                baseline: l.baseline,
                gap: l.gap,
            })
            .collect();
        LaidText {
            caret: caret_from_layout(&laid, geometry.size),
            lines,
        }
    }

    /// Place les glyphes d'un texte mis en page, dans l'ordre du dessin.
    #[must_use]
    pub fn glyphs(&self, laid: &LaidText, size: f64) -> Vec<PlacedGlyph> {
        self.glyphs_styled(laid, size, None)
    }

    /// Place les glyphes en donnant à chacun sa police, son corps et sa
    /// couleur.
    #[must_use]
    pub fn glyphs_styled(
        &self,
        laid: &LaidText,
        size: f64,
        styles: Option<&Styles>,
    ) -> Vec<PlacedGlyph> {
        let styles = styles.filter(|s| !s.uniform() && !self.runs.is_empty());
        let mut out = Vec::new();
        // Le rang du caractère dans le texte entier : c'est lui qui porte le
        // style, et les lignes se suivent.
        let mut index = 0_usize;
        for line in &laid.lines {
            let mut x = line.x;
            for c in line.text.chars() {
                let (drawn, body, color, run) = self.placed_at(c, index, size, styles);
                let mut width = 0.0;
                for (face, advance) in drawn {
                    if c != ' ' {
                        out.push(PlacedGlyph {
                            face,
                            x: x + width * body,
                            baseline: line.baseline,
                            size: body,
                            color,
                            run,
                        });
                    }
                    width += advance;
                }
                if width <= 0.0 {
                    width = 0.5;
                }
                x += width * body + if c == ' ' { line.gap } else { 0.0 };
                index += 1;
            }
            // Le blanc qui joint deux lignes compte, lui aussi, un caractère.
            index += 1;
        }
        out
    }

    /// Contour d'un glyphe placé, dans l'espace texte unitaire (1 = corps 1).
    #[must_use]
    pub fn outline(&self, placed: &PlacedGlyph) -> Option<Rc<Path>> {
        match &placed.face {
            Face::Own(glyph) => placed
                .run
                .and_then(|i| self.runs.get(i))
                .unwrap_or(&self.prepared)
                .font
                .glyph_path_cached(glyph),
            Face::Spare(gid) => {
                if let Some(cached) = self.spare.as_ref()?.paths.borrow().get(gid) {
                    return cached.clone();
                }
                let spare = self.spare.as_ref()?;
                let unit = 1.0 / f64::from(spare.font.units_per_em().max(1));
                let path = spare
                    .font
                    .glyph_path(*gid)
                    .map(|p| Rc::new(p.transform(&Matrix::scale(unit, unit))));
                spare.paths.borrow_mut().insert(*gid, path.clone());
                path
            }
        }
    }

    /// Tout ce qu'il faut pour poser un caractère : ses dessins, son corps,
    /// son encre, et le style d'où il vient.
    fn placed_at(&self, c: char, index: usize, size: f64, styles: Option<&Styles>) -> Placement {
        let Some(styles) = styles else {
            return (self.faces(c), size, self.color, None);
        };
        let id = styles.per_char.get(index).copied().unwrap_or(0) as usize;
        let font = self.runs.get(id).unwrap_or(&self.prepared);
        let body = styles.size_at(index, size);
        let color = styles.runs.get(id).map_or(self.color, |r| r.color);
        let drawn = faces_in(font, c);
        if !drawn.is_empty() {
            return (drawn, body, color, Some(id));
        }
        // Ce caractère manque à la police du style : il se dessine avec celle
        // du bloc, ou celle de secours.
        (self.faces(c), body, color, None)
    }

    /// Où prendre les dessins d'un caractère, et de quelles avances.
    fn faces(&self, c: char) -> Vec<(Face, f64)> {
        let own = faces_in(&self.prepared, c);
        if !own.is_empty() {
            return own;
        }
        let Some(spare) = self.spare.as_ref() else {
            return Vec::new();
        };
        let Some(gid) = spare.font.unicode_to_gid(c) else {
            return Vec::new();
        };
        let unit = f64::from(spare.font.units_per_em().max(1));
        let width = f64::from(spare.font.advance(gid).unwrap_or(0)) / unit;
        vec![(Face::Spare(gid), width)]
    }

    /// Avance d'un caractère, en cadratins.
    fn em(&self, c: char) -> f64 {
        let drawn = self.faces(c);
        if drawn.is_empty() {
            return 0.5;
        }
        drawn.iter().map(|(_, w)| w).sum()
    }
}

/// Dictionnaire de la police d'un bloc, cherchée là où elle vit.
///
/// Dans la page d'abord ; dans les XObjects de formulaire ensuite, car un
/// texte écrit par un traitement de texte y cite **ses** polices ; et pour
/// une zone de texte neuve, la police standard qu'on lui donnera.
fn font_dict(doc: &Document, page: &Page, frame: &ParagraphFrame) -> Result<Dict> {
    // Une police choisie dans la barre : l'aperçu emploie la même que
    // l'écriture, sans quoi ce qu'on voit ne serait pas ce qu'on obtient.
    if let Some(face) = &frame.face {
        let named = face.named(frame.font.as_str().as_str());
        return Ok(encode::standard_dict(&named, face.bold, face.italic));
    }
    if let Some(family) = &frame.standard {
        return Ok(encode::standard_dict(family, false, false));
    }
    if let Ok(dict) = super::font_dict_of(doc, page, &frame.font) {
        return Ok(dict);
    }
    if let Some(dict) = font_in_forms(doc, page, &frame.font) {
        return Ok(dict);
    }
    super::font_dict_of(doc, page, &frame.font)
}

/// Cherche une police dans les XObjects de formulaire de la page.
fn font_in_forms(doc: &Document, page: &Page, name: &Name) -> Option<Dict> {
    let page_dict = super::current_page_dict(doc, page).ok()?;
    let resources = doc
        .dict_get(&page_dict, "Resources")
        .ok()?
        .and_then(|r| r.as_dict().cloned())
        .unwrap_or_default();
    let xobjects = doc
        .dict_get(&resources, "XObject")
        .ok()?
        .and_then(|x| x.as_dict().cloned())
        .unwrap_or_default();
    for value in xobjects.values() {
        let Ok(object) = doc.resolve(value) else {
            continue;
        };
        let Object::Stream { dict, .. } = &*object else {
            continue;
        };
        if dict.get(&Name::new("Subtype")).and_then(Object::as_name) != Some(&Name::new("Form")) {
            continue;
        }
        let Ok(Some(form_resources)) = doc.dict_get(dict, "Resources") else {
            continue;
        };
        let Some(form_resources) = form_resources.as_dict().cloned() else {
            continue;
        };
        if let Some(dict) = super::scan::font_dict(doc, &form_resources, name) {
            return Some(dict);
        }
    }
    None
}
