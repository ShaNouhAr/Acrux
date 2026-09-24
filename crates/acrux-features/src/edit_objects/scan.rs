//! Inventaire des objets dessinés par une page.
//!
//! Le flux de contenu est relu opération par opération, en suivant l'état
//! graphique — matrice courante, couleurs, trait, état étendu, découpe — et
//! chaque **chose dessinée** est notée avec :
//!
//! * sa nature (image, formulaire, tracé, texte, dégradé) ;
//! * sa boîte englobante en espace page ;
//! * la matrice en vigueur au moment du tracé ;
//! * la **plage d'octets** exacte qui la dessine dans le flux décodé ;
//! * de quoi la redessiner ailleurs : couleurs, épaisseur de trait, état
//!   graphique étendu.
//!
//! C'est cette plage d'octets qui rend l'édition propre : déplacer un objet
//! n'est pas « régénérer la page », c'est encadrer quelques octets et laisser
//! tout le reste **intact, octet pour octet**.

use acrux_core::{Matrix, Point, Rect};
use acrux_document::{Dict, Document, Name, Object, Page};
use acrux_render::ContentLexer;

/// Nature d'un objet dessiné.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Image externe (`/Nom Do` sur un XObject image).
    Image,
    /// Image en ligne (`BI … ID … EI`).
    InlineImage,
    /// XObject de formulaire (`/Nom Do`) : un dessin groupé.
    Form,
    /// Tracé vectoriel : construction puis peinture.
    Path,
    /// Bloc de texte `BT … ET`.
    Text,
    /// Dégradé (`sh`).
    Shading,
}

impl Kind {
    /// Nom court, en français, pour les listes et les messages.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::InlineImage => "image en ligne",
            Kind::Form => "formulaire",
            Kind::Path => "tracé",
            Kind::Text => "texte",
            Kind::Shading => "dégradé",
        }
    }
}

/// Un objet dessiné par la page.
#[derive(Debug, Clone)]
pub struct PageObject {
    /// Position dans l'inventaire, dans l'ordre de tracé.
    pub index: usize,
    /// Nature.
    pub kind: Kind,
    /// Nom de la ressource (`Im0`, `Fm1`…) pour une image ou un formulaire.
    pub name: Option<String>,
    /// Boîte englobante en espace page, en points.
    pub bbox: Rect,
    /// Matrice en vigueur au moment du tracé.
    pub matrix: Matrix,
    /// Matrice du **niveau racine** à cet endroit du flux.
    ///
    /// C'est ce qui reste de la matrice courante une fois refermés tous les
    /// `q` ouverts : la transformation que subira, à cet endroit, tout ce
    /// qu'on ajouterait au flux. Indispensable pour réémettre un objet
    /// ailleurs — beaucoup de producteurs posent un `cm` au niveau racine
    /// qu'ils ne restaurent jamais, et l'ignorer applique la transformation
    /// deux fois.
    pub outer: Matrix,
    /// Octets qui dessinent l'objet, dans le flux décodé de la page.
    pub range: (usize, usize),
    /// Indices des opérations qui le dessinent, `debut..fin`.
    pub ops: (usize, usize),
    /// Profondeur d'imbrication `q`/`Q` à cet endroit.
    pub depth: usize,
    /// Découpe en vigueur au moment du tracé, en espace page.
    ///
    /// Ce qui compte n'est pas qu'une découpe existe — Chrome et Word en
    /// posent une d'office sur toute la zone imprimable — mais qu'elle
    /// **rogne cet objet-là**. Voir [`PageObject::cut`] et
    /// [`PageObject::movable_in_order`].
    pub clip: Option<Rect>,
    /// Opérations à réémettre pour redessiner l'objet ailleurs (couleurs,
    /// trait, état graphique étendu). Vide pour une image ou un formulaire,
    /// qui ne dépendent que de la matrice.
    pub state: Vec<u8>,
}

impl PageObject {
    /// Vrai si la découpe en vigueur rogne vraiment l'objet.
    #[must_use]
    pub fn cut(&self) -> bool {
        self.clip.is_some_and(|c| !covers(c, self.bbox))
    }

    /// Vrai si l'objet peut changer de place dans l'ordre de tracé.
    ///
    /// Un objet entièrement contenu dans sa découpe peut en sortir sans que
    /// rien ne change à l'écran ; un objet que la découpe rogne, non — le
    /// déplacer ferait apparaître ce qui était coupé.
    #[must_use]
    pub fn movable_in_order(&self) -> bool {
        !self.cut()
    }
}

/// Vrai si `outer` contient `inner`, à un demi-point près.
fn covers(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 - 0.5
        && inner.y0 >= outer.y0 - 0.5
        && inner.x1 <= outer.x1 + 0.5
        && inner.y1 <= outer.y1 + 0.5
}

/// État graphique suivi pendant le balayage.
#[derive(Debug, Clone)]
struct State {
    ctm: Matrix,
    /// Découpe en vigueur, en espace page ; `None` = toute la page.
    clip: Option<Rect>,
    /// Opérations de couleur et de trait en vigueur, par clé.
    ///
    /// La clé est la « famille » (`fill`, `stroke`, `w`, `d`…) et la valeur
    /// les octets exacts de l'opération : les réémettre rend l'état tel quel,
    /// sans avoir à comprendre les espaces de couleur exotiques.
    marks: Vec<(&'static str, Vec<u8>)>,
}

impl State {
    fn new() -> Self {
        State {
            ctm: Matrix::IDENTITY,
            clip: None,
            marks: Vec::new(),
        }
    }

    /// Note une opération d'état, en remplaçant celle de la même famille.
    fn mark(&mut self, key: &'static str, bytes: &[u8]) {
        if let Some(slot) = self.marks.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = bytes.to_vec();
        } else {
            self.marks.push((key, bytes.to_vec()));
        }
    }

    /// Les opérations d'état, dans l'ordre où elles ont été posées.
    fn prelude(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (_, bytes) in &self.marks {
            out.extend_from_slice(bytes);
            out.push(b'\n');
        }
        out
    }
}

/// Famille d'état d'un opérateur, ou `None` s'il n'en règle pas.
fn state_key(operator: &[u8]) -> Option<&'static str> {
    match operator {
        b"g" | b"rg" | b"k" | b"sc" | b"scn" | b"cs" => Some("fill"),
        b"G" | b"RG" | b"K" | b"SC" | b"SCN" | b"CS" => Some("stroke"),
        b"w" => Some("w"),
        b"J" => Some("J"),
        b"j" => Some("j"),
        b"M" => Some("M"),
        b"d" => Some("d"),
        b"gs" => Some("gs"),
        b"ri" => Some("ri"),
        b"i" => Some("i"),
        _ => None,
    }
}

/// Opérateurs qui peignent un tracé (§8.5.3).
fn is_paint(operator: &[u8]) -> bool {
    matches!(
        operator,
        b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n"
    )
}

/// Opérateurs qui construisent un tracé (§8.5.2).
fn is_path_op(operator: &[u8]) -> bool {
    matches!(operator, b"m" | b"l" | b"c" | b"v" | b"y" | b"h" | b"re")
}

/// Résultat du balayage.
pub struct Scan {
    /// Objets dessinés, dans l'ordre de tracé.
    pub objects: Vec<PageObject>,
    /// Matrice du niveau racine **à la fin** du flux : celle que subirait
    /// tout ce qu'on ajouterait après.
    pub tail: Matrix,
    /// Nombre de `q` encore ouverts à la fin du flux.
    ///
    /// Un producteur qui laisse des `q` ouverts laisse aussi l'état qu'ils
    /// protègent : ce qu'on ajoute après doit d'abord les refermer pour
    /// retrouver le niveau racine, où la matrice vaut [`Scan::tail`].
    pub open: usize,
}

/// Balaye une page et rend ses objets, dans l'ordre de tracé.
///
/// # Errors
/// Flux de contenu illisible.
#[allow(clippy::too_many_lines)] // un bloc par famille d'opérateur, à la suite
pub fn scan(doc: &Document, page: &Page, content: &[u8]) -> acrux_core::Result<Scan> {
    let resources = doc
        .dict_get(&page.dict, "Resources")
        .ok()
        .flatten()
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();

    let mut objects: Vec<PageObject> = Vec::new();
    let mut stack: Vec<State> = Vec::new();
    let mut state = State::new();
    let mut lexer = ContentLexer::new(content);
    let mut last = 0usize;
    let mut op_index = 0usize;
    // Matrice du niveau racine : la matrice courante chaque fois que la pile
    // des `q` est vide.
    let mut outer = Matrix::IDENTITY;

    // Tracé en cours de construction : début, points, et si c'est une découpe.
    let mut path_start: Option<(usize, usize)> = None;
    let mut path_points: Vec<Point> = Vec::new();
    let mut path_current = Point::new(0.0, 0.0);
    let mut path_is_clip = false;
    // Bloc de texte en cours.
    let mut text_start: Option<(usize, usize)> = None;
    let mut text_state: Option<State> = None;

    while let Some(op) = lexer.next_operation()? {
        let end = lexer.pos();
        // Début de l'opération, espacement de tête exclu : c'est cette borne
        // qu'il faut pour encadrer proprement l'objet.
        let start = last + leading_space(&content[last..end]);
        let operator = op.operator.as_slice();

        match operator {
            b"q" => {
                stack.push(state.clone());
            }
            b"Q" => {
                if let Some(previous) = stack.pop() {
                    state = previous;
                }
            }
            b"cm" => {
                if let Some(m) = matrix_of(&op.operands) {
                    state.ctm = m.then(&state.ctm);
                }
            }
            b"BT" => {
                text_start = Some((start, op_index));
                text_state = Some(state.clone());
            }
            b"ET" => {
                if let Some((from, from_op)) = text_start.take() {
                    let at = text_state.take().unwrap_or_else(|| state.clone());
                    objects.push(PageObject {
                        index: objects.len(),
                        kind: Kind::Text,
                        name: None,
                        // La boîte d'un bloc de texte est remplie plus tard,
                        // par l'extraction de texte : elle seule connaît les
                        // métriques de chaque police.
                        bbox: Rect::default(),
                        matrix: at.ctm,
                        outer,
                        range: (from, end),
                        ops: (from_op, op_index + 1),
                        depth: stack.len(),
                        clip: at.clip,
                        state: at.prelude(),
                    });
                }
            }
            b"Do" => {
                let Some(Object::Name(n)) = op.operands.first() else {
                    last = end;
                    op_index += 1;
                    continue;
                };
                let Some((kind, bbox)) = xobject(doc, &resources, n) else {
                    last = end;
                    op_index += 1;
                    continue;
                };
                objects.push(PageObject {
                    index: objects.len(),
                    kind,
                    name: Some(n.as_str()),
                    bbox: state.ctm.transform_rect(&bbox),
                    matrix: state.ctm,
                    outer,
                    range: (start, end),
                    ops: (op_index, op_index + 1),
                    depth: stack.len(),
                    clip: state.clip,
                    // Un `Do` sauve et restaure l'état (§8.10.1) et ne dépend
                    // que de la matrice : rien à réémettre.
                    state: Vec::new(),
                });
            }
            b"BI" => {
                objects.push(PageObject {
                    index: objects.len(),
                    kind: Kind::InlineImage,
                    name: None,
                    bbox: state.ctm.transform_rect(&unit_square()),
                    matrix: state.ctm,
                    outer,
                    range: (start, end),
                    ops: (op_index, op_index + 1),
                    depth: stack.len(),
                    clip: state.clip,
                    state: Vec::new(),
                });
            }
            b"sh" => {
                objects.push(PageObject {
                    index: objects.len(),
                    kind: Kind::Shading,
                    name: op.operands.first().and_then(|o| match o {
                        Object::Name(n) => Some(n.as_str()),
                        _ => None,
                    }),
                    // Un dégradé remplit la découpe courante ; sans découpe
                    // connue, on prend le carré unité transformé, ce qui
                    // suffit à le désigner.
                    bbox: state
                        .clip
                        .unwrap_or_else(|| state.ctm.transform_rect(&unit_square())),
                    matrix: state.ctm,
                    outer,
                    range: (start, end),
                    ops: (op_index, op_index + 1),
                    depth: stack.len(),
                    clip: state.clip,
                    state: state.prelude(),
                });
            }
            b"W" | b"W*" => path_is_clip = true,
            _ if is_path_op(operator) => {
                if path_start.is_none() {
                    path_start = Some((start, op_index));
                    path_points.clear();
                    path_is_clip = false;
                }
                collect_points(operator, &op.operands, &mut path_current, &mut path_points);
            }
            _ if is_paint(operator) => {
                if let Some((from, from_op)) = path_start.take() {
                    if path_is_clip {
                        // Le tracé servait de découpe : il ne se déplace pas,
                        // et il restreint ce qui suit dans ce niveau. On garde
                        // son rectangle pour savoir s'il restreint vraiment.
                        let box_of = points_box(&path_points, &state.ctm);
                        state.clip = Some(match state.clip {
                            Some(previous) => previous.intersect(&box_of),
                            None => box_of,
                        });
                    } else if !path_points.is_empty() {
                        let mut bbox: Option<Rect> = None;
                        for p in &path_points {
                            let t = state.ctm.apply(*p);
                            bbox = Some(match bbox {
                                Some(b) => b.union(&Rect::new(t.x, t.y, t.x, t.y)),
                                None => Rect::new(t.x, t.y, t.x, t.y),
                            });
                        }
                        objects.push(PageObject {
                            index: objects.len(),
                            kind: Kind::Path,
                            name: None,
                            bbox: bbox.unwrap_or_default(),
                            matrix: state.ctm,
                            outer,
                            range: (from, end),
                            ops: (from_op, op_index + 1),
                            depth: stack.len(),
                            clip: state.clip,
                            state: state.prelude(),
                        });
                    }
                    path_points.clear();
                    path_is_clip = false;
                }
            }
            _ => {
                if let Some(key) = state_key(operator) {
                    state.mark(key, content[start..end].trim_ascii());
                }
            }
        }
        last = end;
        op_index += 1;
        if stack.is_empty() {
            outer = state.ctm;
        }
    }
    Ok(Scan {
        objects,
        tail: outer,
        open: stack.len(),
    })
}

/// Boîte d'une suite de points, transformée.
fn points_box(points: &[Point], m: &Matrix) -> Rect {
    let mut out: Option<Rect> = None;
    for p in points {
        let t = m.apply(*p);
        out = Some(match out {
            Some(b) => b.union(&Rect::new(t.x, t.y, t.x, t.y)),
            None => Rect::new(t.x, t.y, t.x, t.y),
        });
    }
    out.unwrap_or_default()
}

/// Nombre d'octets d'espacement en tête d'une tranche.
fn leading_space(slice: &[u8]) -> usize {
    slice
        .iter()
        .take_while(|b| acrux_document::lexer::is_whitespace(**b))
        .count()
}

/// Le carré unité, espace naturel d'une image PDF (§8.9.5.2).
fn unit_square() -> Rect {
    Rect::new(0.0, 0.0, 1.0, 1.0)
}

/// Matrice d'un opérateur `cm`.
fn matrix_of(operands: &[Object]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    let v: Vec<f64> = operands.iter().filter_map(Object::as_f64).collect();
    (v.len() >= 6).then(|| Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
}

/// Nature et boîte naturelle d'un XObject nommé.
fn xobject(doc: &Document, resources: &Dict, name: &Name) -> Option<(Kind, Rect)> {
    let xobjects = doc.dict_get(resources, "XObject").ok()??;
    let entry = xobjects.as_dict()?.get(name)?.clone();
    let resolved = doc.resolve(&entry).ok()?;
    let dict = resolved.as_dict()?;
    let entry = doc.dict_get(dict, "Subtype").ok()??;
    let Object::Name(subtype) = &*entry else {
        return None;
    };
    match subtype.as_str().as_str() {
        "Image" => Some((Kind::Image, unit_square())),
        "Form" => {
            let numbers = |key: &str| -> Vec<f64> {
                doc.dict_get(dict, key)
                    .ok()
                    .flatten()
                    .and_then(|o| {
                        o.as_array()
                            .map(|a| a.iter().filter_map(Object::as_f64).collect())
                    })
                    .unwrap_or_default()
            };
            let b = numbers("BBox");
            let mut bbox = if b.len() == 4 {
                Rect::new(b[0], b[1], b[2], b[3])
            } else {
                unit_square()
            };
            let m = numbers("Matrix");
            if m.len() == 6 {
                bbox = Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5]).transform_rect(&bbox);
            }
            Some((Kind::Form, bbox))
        }
        _ => None,
    }
}

/// Ajoute à `points` les points de contrôle d'une opération de tracé.
///
/// Les points de contrôle des cubiques sont inclus : la boîte est donc un peu
/// large, jamais trop courte — ce qu'il faut pour désigner un objet.
#[allow(clippy::many_single_char_names)] // coordonnées d'un rectangle
fn collect_points(
    operator: &[u8],
    operands: &[Object],
    current: &mut Point,
    points: &mut Vec<Point>,
) {
    let v: Vec<f64> = operands.iter().filter_map(Object::as_f64).collect();
    match operator {
        b"m" | b"l" if v.len() >= 2 => {
            *current = Point::new(v[0], v[1]);
            points.push(*current);
        }
        b"c" if v.len() >= 6 => {
            for pair in v.chunks_exact(2).take(3) {
                points.push(Point::new(pair[0], pair[1]));
            }
            *current = Point::new(v[4], v[5]);
        }
        b"v" if v.len() >= 4 => {
            points.push(*current);
            points.push(Point::new(v[0], v[1]));
            points.push(Point::new(v[2], v[3]));
            *current = Point::new(v[2], v[3]);
        }
        b"y" if v.len() >= 4 => {
            points.push(Point::new(v[0], v[1]));
            points.push(Point::new(v[2], v[3]));
            *current = Point::new(v[2], v[3]);
        }
        b"re" if v.len() >= 4 => {
            let (x, y, w, h) = (v[0], v[1], v[2], v[3]);
            points.push(Point::new(x, y));
            points.push(Point::new(x + w, y));
            points.push(Point::new(x + w, y + h));
            points.push(Point::new(x, y + h));
            *current = Point::new(x, y);
        }
        _ => {}
    }
}
