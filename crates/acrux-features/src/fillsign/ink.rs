//! Encre manuscrite : du relevé du pointeur au contour rempli.
//!
//! Un trait tracé à la souris ou au stylet arrive sous la forme d'une suite de
//! points bruts, irrégulière et tremblée. Le dessiner tel quel avec une
//! épaisseur constante donne le trait raide et anguleux qui trahit les
//! mauvaises implémentations. Ce module en fait un **contour fermé** que l'on
//! remplit, ce qui permet trois choses qu'un trait d'épaisseur fixe ne permet
//! pas : la largeur varie, les extrémités s'effilent, et les virages restent
//! ronds.
//!
//! # La chaîne
//!
//! 1. [`clean`] : les points trop proches sont jetés — un pointeur immobile en
//!    produit des dizaines au même endroit, et ils rendent les tangentes
//!    folles.
//! 2. [`smooth`] : moyenne exponentielle passée dans les deux sens. Deux
//!    passages en sens inverse s'annulent en phase, si bien que le trait est
//!    lissé **sans retard** : il ne « coupe » pas les virages.
//! 3. [`widths`] : la largeur vient de la pression si le dispositif en donne,
//!    sinon de la vitesse — l'écart entre deux relevés consécutifs, puisque les
//!    dispositifs échantillonnent à cadence fixe. Vite = fin, lent = gras,
//!    comme une plume.
//! 4. [`resample`] : ré-échantillonnage à pas constant le long de l'abscisse
//!    curviligne, largeur interpolée au passage. Sans cela, les décalages
//!    latéraux se croisent là où les points d'origine se serrent.
//! 5. Effilage des deux bouts, décalage des deux côtés, éventail dans les
//!    virages serrés, calottes rondes, puis lissage du contour en cubiques de
//!    Bézier (Catmull-Rom).
//!
//! Le contour se remplit avec la règle **non nulle** : les boucles que le
//! contour intérieur forme dans les virages très serrés disparaissent d'elles
//! mêmes, au lieu de percer un trou.

use acrux_core::Rect;

/// Un point relevé par le dispositif de saisie.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkPoint {
    /// Abscisse, dans l'espace du dessin (Y vers le haut).
    pub x: f64,
    /// Ordonnée.
    pub y: f64,
    /// Pression dans `[0, 1]`, si le dispositif en fournit une.
    pub pressure: Option<f64>,
}

impl InkPoint {
    /// Point sans pression, celui que rend une souris.
    #[must_use]
    pub fn new(x: f64, y: f64) -> Self {
        InkPoint {
            x,
            y,
            pressure: None,
        }
    }
}

/// Un trait continu : du poser au lever du stylo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stroke {
    /// Points relevés, dans l'ordre du tracé.
    pub points: Vec<InkPoint>,
}

impl Stroke {
    /// Construit un trait depuis une suite de coordonnées.
    #[must_use]
    pub fn from_points(points: &[(f64, f64)]) -> Self {
        Stroke {
            points: points.iter().map(|&(x, y)| InkPoint::new(x, y)).collect(),
        }
    }
}

/// Réglages de la plume.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pen {
    /// Largeur nominale du trait, dans l'unité du dessin.
    pub width: f64,
    /// Part de largeur que la vitesse retire, dans `[0, 1]`. `0` donne un
    /// trait d'épaisseur constante, `0.6` une plume nerveuse.
    pub thinning: f64,
    /// Vitesse — distance entre deux relevés — à laquelle l'amincissement est
    /// complet.
    pub speed_ref: f64,
    /// Lissage des positions, dans `[0, 1]`.
    pub smoothing: f64,
    /// Longueur sur laquelle les deux bouts s'effilent. `0` les laisse francs.
    pub taper: f64,
}

/// Sorte de pointe : ce qui distingue un stylo d'un feutre.
///
/// Deux choses seulement les séparent vraiment, et ce sont celles qu'on voit :
/// **la largeur varie-t-elle** avec la vitesse du geste, et **les bouts
/// s'effilent-ils** ? Une plume amincit beaucoup et effile longuement, un
/// stylo à bille presque pas, un feutre pas du tout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Nib {
    /// Stylo à bille : trait presque régulier, bouts courts.
    #[default]
    Ball,
    /// Plume : trait nerveux, bouts effilés.
    Fountain,
    /// Feutre : trait large et régulier, bouts francs.
    Marker,
}

impl Nib {
    /// Nom affiché.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Nib::Ball => "Stylo",
            Nib::Fountain => "Plume",
            Nib::Marker => "Feutre",
        }
    }

    /// Les trois pointes, dans l'ordre d'affichage.
    #[must_use]
    pub fn all() -> [Nib; 3] {
        [Nib::Ball, Nib::Fountain, Nib::Marker]
    }

    /// Place dans cet ordre, pour les préférences.
    #[must_use]
    pub fn index(self) -> u8 {
        match self {
            Nib::Ball => 0,
            Nib::Fountain => 1,
            Nib::Marker => 2,
        }
    }

    /// Pointe d'un indice ; au-delà, la première.
    #[must_use]
    pub fn from_index(index: u8) -> Nib {
        *Self::all().get(index as usize).unwrap_or(&Nib::Ball)
    }
}

/// Épaisseur choisie, indépendante de la taille du dessin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weight {
    /// Fin.
    Thin,
    /// Moyen.
    #[default]
    Medium,
    /// Épais.
    Thick,
}

impl Weight {
    /// Nom affiché.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Weight::Thin => "Fin",
            Weight::Medium => "Moyen",
            Weight::Thick => "Épais",
        }
    }

    /// Facteur appliqué à la largeur.
    #[must_use]
    pub fn factor(self) -> f64 {
        match self {
            Weight::Thin => 0.6,
            Weight::Medium => 1.0,
            Weight::Thick => 1.7,
        }
    }

    /// Les trois épaisseurs, dans l'ordre d'affichage.
    #[must_use]
    pub fn all() -> [Weight; 3] {
        [Weight::Thin, Weight::Medium, Weight::Thick]
    }

    /// Place dans cet ordre, pour les préférences.
    #[must_use]
    pub fn index(self) -> u8 {
        match self {
            Weight::Thin => 0,
            Weight::Medium => 1,
            Weight::Thick => 2,
        }
    }

    /// Épaisseur d'un indice ; au-delà, moyenne.
    #[must_use]
    pub fn from_index(index: u8) -> Weight {
        *Self::all().get(index as usize).unwrap_or(&Weight::Medium)
    }
}

impl Pen {
    /// Plume proportionnée à un dessin de cette taille.
    ///
    /// La largeur du trait, la vitesse de référence et l'effilage sont tous
    /// des **longueurs** : donnés en absolu, ils ne veulent rien dire tant
    /// qu'on ne sait pas si le dessin fait 60 unités ou 6 000. Une signature
    /// tracée dans une fenêtre de 700 pixels puis réduite à 170 points doit
    /// garder la même allure ; c'est cette fonction qui le garantit, en
    /// rapportant tout à la diagonale de la boîte du tracé.
    #[must_use]
    pub fn for_extent(width: f64, height: f64) -> Pen {
        let diagonal = width.hypot(height).max(1.0);
        Pen {
            width: (diagonal * 0.016).max(0.05),
            speed_ref: diagonal * 0.05,
            taper: diagonal * 0.035,
            ..Pen::default()
        }
    }

    /// Comme [`Pen::for_extent`], avec une pointe et une épaisseur choisies.
    #[must_use]
    pub fn styled(width: f64, height: f64, nib: Nib, weight: Weight) -> Pen {
        let diagonal = width.hypot(height).max(1.0);
        let k = weight.factor();
        match nib {
            Nib::Ball => Pen {
                width: (diagonal * 0.012 * k).max(0.05),
                thinning: 0.18,
                speed_ref: diagonal * 0.05,
                smoothing: 0.62,
                taper: diagonal * 0.010,
            },
            Nib::Fountain => Pen {
                width: (diagonal * 0.016 * k).max(0.05),
                thinning: 0.55,
                speed_ref: diagonal * 0.05,
                smoothing: 0.6,
                taper: diagonal * 0.035,
            },
            Nib::Marker => Pen {
                width: (diagonal * 0.026 * k).max(0.05),
                thinning: 0.0,
                speed_ref: diagonal * 0.05,
                smoothing: 0.75,
                taper: 0.0,
            },
        }
    }

    /// Plume qui écrit **sur la page**, en points PDF.
    ///
    /// Ici la largeur ne se déduit pas du dessin : on écrit sur une feuille,
    /// et un trait de stylo fait la même épaisseur qu'on trace un trait de
    /// deux centimètres ou qu'on raye toute la page. `unit` vaut 1 pour des
    /// points PDF, l'échelle d'affichage pour un aperçu à l'écran.
    #[must_use]
    pub fn on_page(nib: Nib, weight: Weight, unit: f64) -> Pen {
        let unit = unit.max(0.01);
        let width = match nib {
            Nib::Ball => 1.5,
            Nib::Fountain => 1.9,
            Nib::Marker => 3.4,
        } * weight.factor()
            * unit;
        Pen {
            width,
            thinning: match nib {
                Nib::Ball => 0.18,
                Nib::Fountain => 0.55,
                Nib::Marker => 0.0,
            },
            speed_ref: 7.0 * unit,
            smoothing: match nib {
                Nib::Marker => 0.75,
                _ => 0.62,
            },
            taper: match nib {
                Nib::Ball => width * 0.8,
                Nib::Fountain => width * 2.5,
                Nib::Marker => 0.0,
            },
        }
    }

    /// Plume proportionnée à l'étendue d'une suite de traits, pointe et
    /// épaisseur choisies.
    #[must_use]
    pub fn styled_for_strokes(strokes: &[Stroke], nib: Nib, weight: Weight) -> Pen {
        match Self::extent_of(strokes) {
            Some((w, h)) => Pen::styled(w, h, nib, weight),
            None => Pen::default(),
        }
    }

    /// Étendue d'une suite de traits (largeur, hauteur).
    fn extent_of(strokes: &[Stroke]) -> Option<(f64, f64)> {
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for point in strokes.iter().flat_map(|s| s.points.iter()) {
            match &mut bounds {
                Some(b) => {
                    b.0 = b.0.min(point.x);
                    b.1 = b.1.min(point.y);
                    b.2 = b.2.max(point.x);
                    b.3 = b.3.max(point.y);
                }
                None => bounds = Some((point.x, point.y, point.x, point.y)),
            }
        }
        bounds.map(|(x0, y0, x1, y1)| (x1 - x0, y1 - y0))
    }

    /// Plume proportionnée à l'étendue d'une suite de traits.
    #[must_use]
    pub fn for_strokes(strokes: &[Stroke]) -> Pen {
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for point in strokes.iter().flat_map(|s| s.points.iter()) {
            match &mut bounds {
                Some(b) => {
                    b.0 = b.0.min(point.x);
                    b.1 = b.1.min(point.y);
                    b.2 = b.2.max(point.x);
                    b.3 = b.3.max(point.y);
                }
                None => bounds = Some((point.x, point.y, point.x, point.y)),
            }
        }
        match bounds {
            Some((x0, y0, x1, y1)) => Pen::for_extent(x1 - x0, y1 - y0),
            None => Pen::default(),
        }
    }
}

impl Default for Pen {
    fn default() -> Self {
        Pen {
            width: 2.2,
            thinning: 0.55,
            speed_ref: 9.0,
            smoothing: 0.6,
            taper: 6.0,
        }
    }
}

/// Segment d'un contour, dans l'espace du dessin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    /// Début d'un sous-chemin.
    Move(f64, f64),
    /// Segment droit.
    Line(f64, f64),
    /// Cubique de Bézier : deux points de contrôle puis le point d'arrivée.
    Curve(f64, f64, f64, f64, f64, f64),
    /// Fermeture du sous-chemin courant.
    Close,
}

/// Contour fermé, à remplir avec la règle non nulle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outline {
    /// Segments, un sous-chemin fermé par trait d'encre.
    pub segs: Vec<Seg>,
    /// Boîte englobante du contour.
    pub bbox: Rect,
}

impl Outline {
    /// Vrai si le contour ne contient rien.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }
}

/// Un échantillon du trait après ré-échantillonnage : abscisse, ordonnée,
/// largeur, et distance parcourue depuis le début du trait.
type Sample = (f64, f64, f64, f64);

/// Les deux bords d'un trait, gauche puis droit.
type Sides = (Vec<(f64, f64)>, Vec<(f64, f64)>);

/// Point de travail : position et pression.
#[derive(Debug, Clone, Copy)]
struct Pt {
    x: f64,
    y: f64,
    pressure: Option<f64>,
}

/// Longueur d'un vecteur.
fn norm(dx: f64, dy: f64) -> f64 {
    dx.hypot(dy)
}

/// Transforme des traits d'encre en un contour rempli.
///
/// Les traits vides, ou réduits à un point, donnent un rond de la largeur de
/// la plume : c'est ce que fait un stylo qu'on pose et qu'on relève.
#[must_use]
pub fn outline(strokes: &[Stroke], pen: &Pen) -> Outline {
    let mut out = Outline::default();
    let mut bbox: Option<Rect> = None;
    for stroke in strokes {
        let points = clean(&stroke.points, (pen.width * 0.25).max(0.05));
        if points.is_empty() {
            continue;
        }
        let segs = if points.len() == 1 {
            circle(points[0].x, points[0].y, pen.width.max(0.1) / 2.0)
        } else {
            stroke_outline(&points, pen)
        };
        for seg in &segs {
            grow(&mut bbox, *seg);
        }
        out.segs.extend(segs);
    }
    out.bbox = bbox.unwrap_or(Rect {
        x0: 0.0,
        y0: 0.0,
        x1: 0.0,
        y1: 0.0,
    });
    out
}

/// Élargit la boîte englobante pour contenir un segment.
///
/// Les points de contrôle d'une cubique sont pris en compte : la boîte est
/// donc un peu large, jamais trop courte. C'est le bon sens pour une `/BBox`,
/// qui **découpe** ce qui dépasse.
fn grow(bbox: &mut Option<Rect>, seg: Seg) {
    let mut add = |x: f64, y: f64| match bbox {
        Some(b) => {
            b.x0 = b.x0.min(x);
            b.y0 = b.y0.min(y);
            b.x1 = b.x1.max(x);
            b.y1 = b.y1.max(y);
        }
        None => {
            *bbox = Some(Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            });
        }
    };
    match seg {
        Seg::Move(x, y) | Seg::Line(x, y) => add(x, y),
        Seg::Curve(x1, y1, x2, y2, x3, y3) => {
            add(x1, y1);
            add(x2, y2);
            add(x3, y3);
        }
        Seg::Close => {}
    }
}

/// Jette les points trop proches de leur prédécesseur.
fn clean(points: &[InkPoint], min_step: f64) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(points.len());
    for p in points {
        if !p.x.is_finite() || !p.y.is_finite() {
            continue;
        }
        if let Some(last) = out.last() {
            if norm(p.x - last.x, p.y - last.y) < min_step {
                continue;
            }
        }
        out.push(Pt {
            x: p.x,
            y: p.y,
            pressure: p.pressure.map(|v| v.clamp(0.0, 1.0)),
        });
    }
    out
}

/// Lissage des positions, sans retard : une passe avant, une passe arrière.
fn smooth(points: &mut [Pt], amount: f64) {
    let alpha = 1.0 - amount.clamp(0.0, 1.0) * 0.75;
    if alpha >= 1.0 || points.len() < 3 {
        return;
    }
    for index in 1..points.len() {
        let previous = points[index - 1];
        points[index].x = alpha.mul_add(points[index].x, (1.0 - alpha) * previous.x);
        points[index].y = alpha.mul_add(points[index].y, (1.0 - alpha) * previous.y);
    }
    for index in (0..points.len() - 1).rev() {
        let next = points[index + 1];
        points[index].x = alpha.mul_add(points[index].x, (1.0 - alpha) * next.x);
        points[index].y = alpha.mul_add(points[index].y, (1.0 - alpha) * next.y);
    }
}

/// Largeur en chaque point : pression si elle existe, vitesse sinon.
fn widths(points: &[Pt], pen: &Pen) -> Vec<f64> {
    let base = pen.width.max(0.05);
    let thinning = pen.thinning.clamp(0.0, 0.95);
    let reference = pen.speed_ref.max(0.001);
    let mut out = Vec::with_capacity(points.len());
    for index in 0..points.len() {
        let before = points[index.saturating_sub(1)];
        let after = points[(index + 1).min(points.len() - 1)];
        let span = if index == 0 || index + 1 == points.len() {
            norm(after.x - before.x, after.y - before.y)
        } else {
            norm(after.x - before.x, after.y - before.y) / 2.0
        };
        let factor = match points[index].pressure {
            // La pression commande directement, mais jamais jusqu'au trait nul.
            Some(p) => 1.0 - thinning * (1.0 - p),
            None => 1.0 - thinning * (span / reference).clamp(0.0, 1.0),
        };
        out.push(base * factor);
    }
    // Les largeurs tremblent autant que les positions : même traitement.
    for index in 1..out.len() {
        out[index] = 0.35f64.mul_add(out[index], 0.65 * out[index - 1]);
    }
    for index in (0..out.len().saturating_sub(1)).rev() {
        out[index] = 0.35f64.mul_add(out[index], 0.65 * out[index + 1]);
    }
    out
}

/// Ré-échantillonne à pas constant le long de l'abscisse curviligne.
///
/// Rend les points, leur largeur, et la distance parcourue jusqu'à chacun.
fn resample(points: &[Pt], widths: &[f64], step: f64) -> Vec<Sample> {
    let step = step.max(0.01);
    let mut out = vec![(points[0].x, points[0].y, widths[0], 0.0)];
    let mut travelled = 0.0;
    let mut next = step;
    for index in 1..points.len() {
        let (a, b) = (points[index - 1], points[index]);
        let length = norm(b.x - a.x, b.y - a.y);
        if length <= 0.0 {
            continue;
        }
        while next <= travelled + length {
            let t = (next - travelled) / length;
            out.push((
                (b.x - a.x).mul_add(t, a.x),
                (b.y - a.y).mul_add(t, a.y),
                (widths[index] - widths[index - 1]).mul_add(t, widths[index - 1]),
                next,
            ));
            next += step;
        }
        travelled += length;
    }
    let last = points[points.len() - 1];
    let (lx, ly) = (last.x, last.y);
    if let Some(&(x, y, _, _)) = out.last() {
        if norm(lx - x, ly - y) > step * 0.2 {
            out.push((lx, ly, widths[widths.len() - 1], travelled));
        }
    }
    out
}

/// Effilage des deux extrémités : la largeur s'éteint sur `taper`.
fn taper(samples: &mut [Sample], taper: f64, total: f64) {
    if taper <= 0.0 || total <= 0.0 {
        return;
    }
    // Un trait court ne peut pas s'effiler sur toute sa longueur, sinon il
    // n'en reste rien : la moitié de chaque côté au plus.
    let span = taper.min(total / 2.0);
    if span <= 0.0 {
        return;
    }
    for sample in samples.iter_mut() {
        let from_start = sample.3;
        let from_end = total - sample.3;
        let edge = from_start.min(from_end);
        if edge < span {
            let t = (edge / span).clamp(0.0, 1.0);
            // Lissage de Hermite : départ et arrivée tangents, pas de cassure.
            sample.2 *= t * t * (3.0 - 2.0 * t);
        }
    }
}

/// Construit le contour d'un trait.
fn stroke_outline(points: &[Pt], pen: &Pen) -> Vec<Seg> {
    let mut points = points.to_vec();
    smooth(&mut points, pen.smoothing);
    let ws = widths(&points, pen);
    let step = (pen.width * 0.5).clamp(0.25, 4.0);
    let mut samples = resample(&points, &ws, step);
    if samples.len() < 2 {
        return circle(samples[0].0, samples[0].1, pen.width.max(0.1) / 2.0);
    }
    let total = samples[samples.len() - 1].3;
    taper(&mut samples, pen.taper, total);
    // Une largeur strictement nulle ferait disparaître la calotte : un
    // minuscule reste de matière garde la pointe visible.
    let floor = pen.width * 0.06;
    for sample in &mut samples {
        sample.2 = sample.2.max(floor);
    }

    let (left, right) = sides(&samples);
    let mut segs = Vec::with_capacity(left.len() + right.len() + 8);
    let (first, last) = (samples[0], samples[samples.len() - 1]);
    segs.push(Seg::Move(left[0].0, left[0].1));
    catmull(&left, &mut segs);
    // Calotte de fin : demi-tour autour du dernier point, du côté gauche vers
    // le côté droit, en passant par l'avant.
    cap(
        &mut segs,
        (last.0, last.1),
        last.2 / 2.0,
        *left.last().unwrap_or(&(last.0, last.1)),
        *right.last().unwrap_or(&(last.0, last.1)),
    );
    let mut back = right.clone();
    back.reverse();
    catmull(&back, &mut segs);
    // Calotte de départ, dans l'autre sens.
    cap(
        &mut segs,
        (first.0, first.1),
        first.2 / 2.0,
        right[0],
        left[0],
    );
    segs.push(Seg::Close);
    segs
}

/// Décale les échantillons de part et d'autre de la ligne moyenne.
///
/// Dans un virage serré, le côté **extérieur** reçoit un éventail de points
/// au lieu d'un seul : la normale moyennée y laisserait un coin coupé, et le
/// trait se casserait. Le côté intérieur garde son point unique ; les boucles
/// qu'il forme quand le rayon du virage est plus petit que la demi-largeur
/// sont avalées par le remplissage non nul.
fn sides(samples: &[Sample]) -> Sides {
    let mut left = Vec::with_capacity(samples.len() + 16);
    let mut right = Vec::with_capacity(samples.len() + 16);
    let count = samples.len();
    for index in 0..count {
        let previous = samples[index.saturating_sub(1)];
        let next = samples[(index + 1).min(count - 1)];
        let (mut tx, mut ty) = (next.0 - previous.0, next.1 - previous.1);
        let length = norm(tx, ty);
        if length < 1e-9 {
            continue;
        }
        tx /= length;
        ty /= length;
        // Normale gauche : la tangente tournée d'un quart de tour direct.
        let (nx, ny) = (-ty, tx);
        let (x, y, w) = (samples[index].0, samples[index].1, samples[index].2);
        let radius = w / 2.0;
        let turn = if index == 0 || index + 1 == count {
            0.0
        } else {
            corner(samples, index)
        };
        if turn.abs() < SHARP {
            left.push((nx.mul_add(radius, x), ny.mul_add(radius, y)));
            right.push((nx.mul_add(-radius, x), ny.mul_add(-radius, y)));
            continue;
        }
        // Le virage tourne à gauche (`turn > 0`) : l'extérieur est à droite.
        let outer_left = turn < 0.0;
        let incoming = (samples[index].1 - samples[index - 1].1)
            .atan2(samples[index].0 - samples[index - 1].0);
        let base = if outer_left {
            incoming + std::f64::consts::FRAC_PI_2
        } else {
            incoming - std::f64::consts::FRAC_PI_2
        };
        let steps = fan_steps(turn);
        let fan: Vec<(f64, f64)> = (0..=steps)
            .map(|k| {
                let t = f64::from(k) / f64::from(steps);
                let angle = turn.mul_add(t, base);
                (
                    angle.cos().mul_add(radius, x),
                    angle.sin().mul_add(radius, y),
                )
            })
            .collect();
        if outer_left {
            left.extend(fan);
            right.push((nx.mul_add(-radius, x), ny.mul_add(-radius, y)));
        } else {
            right.extend(fan);
            left.push((nx.mul_add(radius, x), ny.mul_add(radius, y)));
        }
    }
    if left.is_empty() || right.is_empty() {
        let (x, y, w, _) = samples[0];
        let r = w / 2.0;
        return (vec![(x, y + r)], vec![(x, y - r)]);
    }
    (left, right)
}

/// Au-delà de cet angle, un virage reçoit un éventail (environ 26°).
const SHARP: f64 = 0.45;

/// Angle du virage en un point : positif vers la gauche.
fn corner(samples: &[Sample], index: usize) -> f64 {
    let (ax, ay) = (
        samples[index].0 - samples[index - 1].0,
        samples[index].1 - samples[index - 1].1,
    );
    let (bx, by) = (
        samples[index + 1].0 - samples[index].0,
        samples[index + 1].1 - samples[index].1,
    );
    ax.mul_add(by, -(ay * bx)).atan2(ax.mul_add(bx, ay * by))
}

/// Nombre de facettes de l'éventail : une tous les 0,4 radian environ.
fn fan_steps(turn: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné juste après
    let steps = (turn.abs() / 0.4).ceil() as u32;
    steps.clamp(1, 12)
}

/// Calotte ronde : demi-tour autour d'un point, de `from` vers `to`.
///
/// `from` est toujours le côté gauche et `to` le côté droit (ou l'inverse au
/// départ du trait) : les deux sont diamétralement opposés, et le demi-tour se
/// fait **par l'avant**, du côté de la pointe. D'où un balayage de −π et non
/// de +π, qui passerait derrière et trancherait le trait.
fn cap(segs: &mut Vec<Seg>, center: (f64, f64), radius: f64, from: (f64, f64), to: (f64, f64)) {
    if radius > 1e-9 {
        let start = (from.1 - center.1).atan2(from.0 - center.0);
        arc(segs, center, radius, start, -std::f64::consts::PI);
    }
    // L'arc arrive à l'opposé exact ; ce raccord ne fait que sceller l'arrondi
    // des flottants.
    segs.push(Seg::Line(to.0, to.1));
}

/// Arc de cercle approché par des cubiques de 90° au plus.
fn arc(segs: &mut Vec<Seg>, center: (f64, f64), radius: f64, start: f64, sweep: f64) {
    let pieces = (sweep.abs() / std::f64::consts::FRAC_PI_2)
        .ceil()
        .clamp(1.0, 8.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // borné à 8
    let count = pieces as usize;
    let each = sweep / pieces;
    // Facteur classique : k = 4/3 · tan(Δ/4).
    let k = 4.0 / 3.0 * (each / 4.0).tan();
    let mut angle = start;
    for _ in 0..count {
        let (c0, s0) = (angle.cos(), angle.sin());
        let (c1, s1) = ((angle + each).cos(), (angle + each).sin());
        let p0 = (c0.mul_add(radius, center.0), s0.mul_add(radius, center.1));
        let p1 = (c1.mul_add(radius, center.0), s1.mul_add(radius, center.1));
        segs.push(Seg::Curve(
            (-s0 * k).mul_add(radius, p0.0),
            (c0 * k).mul_add(radius, p0.1),
            (s1 * k).mul_add(radius, p1.0),
            (-c1 * k).mul_add(radius, p1.1),
            p1.0,
            p1.1,
        ));
        angle += each;
    }
}

/// Cercle complet, pour un trait réduit à un point.
fn circle(x: f64, y: f64, radius: f64) -> Vec<Seg> {
    ellipse(x, y, radius, radius, false)
}

/// Facteur d'un quart de cercle en cubique : 4/3 · (√2 − 1).
const QUARTER: f64 = 0.552_284_749_830_793_4;

/// Ellipse fermée, en quatre cubiques.
///
/// `clockwise` retourne le sens de parcours. Deux ellipses concentriques de
/// sens opposés forment un anneau sous la règle non nulle — c'est ainsi qu'est
/// dessiné le rond de la barre « remplir et signer ».
#[must_use]
pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64, clockwise: bool) -> Vec<Seg> {
    let sign = if clockwise { -1.0 } else { 1.0 };
    let (kx, ky) = (QUARTER * rx, QUARTER * ry * sign);
    let ry = ry * sign;
    let mut segs = vec![Seg::Move(cx + rx, cy)];
    segs.push(Seg::Curve(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry));
    segs.push(Seg::Curve(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy));
    segs.push(Seg::Curve(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry));
    segs.push(Seg::Curve(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy));
    segs.push(Seg::Close);
    segs
}

/// Fait passer une cubique par chaque point, façon Catmull-Rom.
///
/// Le point courant doit déjà être `points[0]`. La tangente en un point est
/// la corde entre ses deux voisins, divisée par six : c'est la forme uniforme,
/// la plus simple et la plus stable quand les points sont régulièrement
/// espacés — ce que garantit [`resample`].
fn catmull(points: &[(f64, f64)], segs: &mut Vec<Seg>) {
    let count = points.len();
    if count < 2 {
        return;
    }
    for index in 0..count - 1 {
        let p0 = points[index.saturating_sub(1)];
        let p1 = points[index];
        let p2 = points[index + 1];
        let p3 = points[(index + 2).min(count - 1)];
        segs.push(Seg::Curve(
            (p2.0 - p0.0).mul_add(1.0 / 6.0, p1.0),
            (p2.1 - p0.1).mul_add(1.0 / 6.0, p1.1),
            (p3.0 - p1.0).mul_add(-1.0 / 6.0, p2.0),
            (p3.1 - p1.1).mul_add(-1.0 / 6.0, p2.1),
            p2.0,
            p2.1,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{Nib, Pen, Stroke, Weight};

    #[test]
    fn lepaisseur_choisie_change_la_largeur_du_trait() {
        let widths: Vec<f64> = Weight::all()
            .iter()
            .map(|w| Pen::on_page(Nib::Ball, *w, 1.0).width)
            .collect();
        assert!(widths[0] < widths[1] && widths[1] < widths[2], "{widths:?}");
    }

    #[test]
    fn chaque_pointe_a_son_caractere() {
        let ball = Pen::on_page(Nib::Ball, Weight::Medium, 1.0);
        let fountain = Pen::on_page(Nib::Fountain, Weight::Medium, 1.0);
        let marker = Pen::on_page(Nib::Marker, Weight::Medium, 1.0);
        // La plume amincit et effile ; le feutre ne fait ni l'un ni l'autre.
        assert!(fountain.thinning > ball.thinning);
        assert!(fountain.taper > ball.taper);
        assert!(marker.thinning.abs() < 1e-9);
        assert!(marker.taper.abs() < 1e-9);
        assert!(marker.width > fountain.width);
    }

    #[test]
    fn un_trait_sur_la_page_ne_depend_pas_de_sa_longueur() {
        // Deux gestes, l'un court l'autre long : même stylo, même épaisseur.
        let court = Pen::on_page(Nib::Ball, Weight::Medium, 1.0);
        let long = Pen::on_page(Nib::Ball, Weight::Medium, 1.0);
        assert!((court.width - long.width).abs() < 1e-9);
        // Alors qu'une signature, elle, se met à l'échelle de son dessin.
        let petite = Pen::styled_for_strokes(
            &[Stroke::from_points(&[(0.0, 0.0), (10.0, 10.0)])],
            Nib::Ball,
            Weight::Medium,
        );
        let grande = Pen::styled_for_strokes(
            &[Stroke::from_points(&[(0.0, 0.0), (100.0, 100.0)])],
            Nib::Ball,
            Weight::Medium,
        );
        assert!(grande.width > petite.width * 5.0);
    }

    #[test]
    fn les_reglages_font_laller_retour_par_leur_indice() {
        for nib in Nib::all() {
            assert_eq!(Nib::from_index(nib.index()), nib);
        }
        for weight in Weight::all() {
            assert_eq!(Weight::from_index(weight.index()), weight);
        }
        assert_eq!(Nib::from_index(9), Nib::Ball);
        assert_eq!(Weight::from_index(9), Weight::Medium);
    }
}
