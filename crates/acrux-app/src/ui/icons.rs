//! Icônes vectorielles de l'interface, décrites comme des chemins dans une
//! boîte de 24 × 24 unités (y vers le bas) et rasterisées par `acrux-graphics`
//! à la taille demandée : nettes à toutes les échelles DPI, sans fichier
//! d'image, et colorées par le thème.
//!
//! Les icônes « au trait » sont des lignes médianes converties en contours
//! par [`stroke_path`] ; les autres sont des surfaces pleines.

// Coordonnées d'écran entières et couvertures 8 bits.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_arguments
)]

use acrux_core::{Matrix, Path, Point};
use acrux_graphics::{stroke_path, FillRule, LineCap, LineJoin, Rasterizer, StrokeStyle};

use crate::platform::Frame;

/// Icônes disponibles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Engrenage (paramètres).
    Settings,
    /// Maison (accueil).
    Home,
    /// Dossier (ouvrir).
    Open,
    /// Chevron gauche (page précédente).
    Prev,
    /// Chevron droit (page suivante).
    Next,
    /// Loupe avec un moins.
    ZoomOut,
    /// Loupe avec un plus.
    ZoomIn,
    /// Flèche double entre deux bornes (ajuster à la largeur).
    FitWidth,
    /// Loupe (rechercher).
    Search,
    /// Soleil (passer au thème clair). Le bouton montre où l'on va : en thème
    /// sombre, le soleil ; en thème clair, [`Icon::Moon`].
    Theme,
    /// Croissant de lune (passer au thème sombre).
    Moon,
    /// Panneau latéral (rectangle avec une colonne à gauche).
    Sidebar,
    /// Flèche en arc (pivoter).
    Rotate,
    /// Flèche qui revient sur ses pas (annuler la modification).
    Undo,
    /// La même, en miroir (rétablir la modification).
    Redo,
    /// Disquette (enregistrer). La flèche sur un bac qu'elle remplace se
    /// lisait « télécharger », partout ailleurs sur l'écran de la personne.
    Save,
    /// Imprimante.
    Print,
    /// Deux rectangles côte à côte (disposition des pages).
    ViewMode,
    /// Quatre carrés (ouvrir la barre des outils).
    Tools,
    /// Crayon (modifier le texte).
    EditText,
    /// Un T dans un cadre (ajouter une zone de texte).
    AddText,
    /// Rectangle à poignées (modifier les objets).
    Objects,
    /// Paraphe sur une ligne (remplir et signer).
    Sign,
    /// Pointe de surligneur sur une bande (surligner).
    Highlight,
    /// Bulle (poser une note).
    Note,
    /// Page avec un plus (insérer des pages).
    PageInsert,
    /// Deux pages superposées (dupliquer).
    PageDuplicate,
    /// Page avec une flèche sortante (extraire).
    PageExtract,
    /// Page avec une croix (supprimer).
    PageDelete,
    /// Bande pleine sur une page (biffure).
    Redact,
    /// Bande pleine et coche (appliquer les biffures).
    RedactApply,
    /// Page et flèche vers la droite (exporter).
    Export,
    /// Trombone (joindre un fichier).
    Attach,
    /// Cadenas fermé (protéger par mot de passe).
    Lock,
    /// Coche : l'élément en vigueur d'une liste (le zoom en cours).
    Check,
    /// Chevron vers le bas : un bouton qui déroule une liste, ou
    /// l'occurrence suivante d'une recherche.
    ChevronDown,
    /// Chevron vers le haut : l'occurrence précédente d'une recherche.
    ChevronUp,
    /// Croix : fermer (la carte de recherche).
    Close,
    /// Page à deux champs (un document à remplir, la barre de formulaire).
    Form,
    /// Un « U » et un trait plein dessous (souligner).
    Underline,
    /// Un « S » barré en son milieu (barrer le texte).
    StrikeOut,
    /// Deux lignes de texte et une onde dessous (souligner d'un trait ondulé).
    Squiggly,
    /// Un signe « ^ » dans une ligne de texte ouverte (insérer du texte).
    Insert,
    /// Un « T » barré et un signe « ^ » (remplacer le texte).
    Replace,
    /// Bulle ronde à trois points (commenter) : la famille des outils de
    /// relecture, distincte de la note posée sur la page.
    Comment,
    /// Rectangle au trait (dessiner un rectangle).
    Rectangle,
    /// Ellipse au trait (dessiner une ellipse).
    Ellipse,
    /// Trait en diagonale (tracer une ligne).
    Line,
    /// Trait en diagonale et sa pointe (tracer une flèche).
    Arrow,
    /// Gribouillis et petit crayon (dessiner à main levée) : distinct du
    /// grand crayon de « modifier le texte ».
    Pencil,
    /// Un T dans un cadre plein (zone de texte, un commentaire) : le cadre
    /// en pointillé reste à « ajouter du texte » dans le contenu.
    TextBox,
    /// Cadre à deux lignes et son trait d'ancrage fléché (légende).
    Callout,
    /// Corbeille (supprimer un commentaire).
    Trash,
    /// Flèche qui revient en arrière (répondre).
    Reply,
    /// Tampon encreur : poignée, socle et son empreinte (tamponner).
    Stamp,
    /// Cadre, montagnes et soleil (ajouter une image).
    Image,
    /// Une page au coin replié, et ses lignes de texte (un document).
    Document,
}

/// Contour d'une page, motif commun à beaucoup d'icônes.
fn page(path: &mut Path, x: f64, y: f64, w: f64, h: f64) {
    polyline(
        path,
        &[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)],
    );
}

/// Rectangle plein.
fn bar(path: &mut Path, x: f64, y: f64, w: f64, h: f64) {
    polyline(
        path,
        &[(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)],
    );
    path.close();
}

/// Constante de Bézier pour un quart de cercle.
const KAPPA: f64 = 0.552_284_75;

fn circle(path: &mut Path, cx: f64, cy: f64, r: f64) {
    let k = KAPPA * r;
    path.move_to(Point::new(cx + r, cy));
    path.curve_to(
        Point::new(cx + r, cy + k),
        Point::new(cx + k, cy + r),
        Point::new(cx, cy + r),
    );
    path.curve_to(
        Point::new(cx - k, cy + r),
        Point::new(cx - r, cy + k),
        Point::new(cx - r, cy),
    );
    path.curve_to(
        Point::new(cx - r, cy - k),
        Point::new(cx - k, cy - r),
        Point::new(cx, cy - r),
    );
    path.curve_to(
        Point::new(cx + k, cy - r),
        Point::new(cx + r, cy - k),
        Point::new(cx + r, cy),
    );
    path.close();
}

fn polyline(path: &mut Path, pts: &[(f64, f64)]) {
    let mut it = pts.iter();
    if let Some(&(x, y)) = it.next() {
        path.move_to(Point::new(x, y));
    }
    for &(x, y) in it {
        path.line_to(Point::new(x, y));
    }
}

/// Points d'un arc d'ellipse de centre `(cx, cy)`, d'angle `from` à `to`
/// en degrés (y vers le bas : les angles croissent dans le sens horaire).
fn arc(cx: f64, cy: f64, rx: f64, ry: f64, from: f64, to: f64) -> Vec<(f64, f64)> {
    let steps = 16;
    (0..=steps)
        .map(|i| {
            let a = (from + (to - from) * f64::from(i) / f64::from(steps)).to_radians();
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

fn magnifier(path: &mut Path) {
    circle(path, 10.5, 10.5, 6.0);
    polyline(path, &[(15.0, 15.0), (20.5, 20.5)]);
}

/// Tracé de la flèche « annuler » : la pointe tournée vers la gauche, la
/// tige, puis la boucle qui revient en bas. « Rétablir » est le même tracé
/// en miroir ([`mirror`]) : les deux boutons, voisins dans la barre, sont
/// symétriques par construction et non à l'œil.
fn undo_points() -> [Vec<(f64, f64)>; 3] {
    let head = vec![(8.5, 5.5), (4.0, 10.0), (8.5, 14.5)];
    let stem = vec![(4.0, 10.0), (14.5, 10.0)];
    // Demi-cercle de centre (14,5 ; 14,5) et de rayon 4,5, du haut vers le
    // bas en passant par la droite, puis retour vers la gauche.
    let mut hook: Vec<(f64, f64)> = (0..=12)
        .map(|i| {
            let a = (-90.0 + 180.0 * f64::from(i) / 12.0_f64).to_radians();
            (14.5 + 4.5 * a.cos(), 14.5 + 4.5 * a.sin())
        })
        .collect();
    hook.push((10.0, 19.0));
    [head, stem, hook]
}

/// Retourne un tracé de gauche à droite dans la boîte 24 × 24.
fn mirror(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    points.iter().map(|&(x, y)| (24.0 - x, y)).collect()
}

/// Croissant de lune plein : le disque `C1` privé du disque `C2` qui le
/// mord en haut à droite.
///
/// Les deux arcs se rejoignent aux intersections des cercles, calculées
/// exactement : des points posés à l'œil laisseraient une pointe émoussée
/// ou un petit bec, visibles à vingt pixels.
fn crescent(path: &mut Path) {
    let (c1, r1) = ((12.0_f64, 12.0_f64), 8.0_f64);
    let (c2, r2) = ((16.5_f64, 8.0_f64), 6.5_f64);
    let (dx, dy) = (c2.0 - c1.0, c2.1 - c1.1);
    let d = dx.hypot(dy);
    // Distance de C1 à la corde commune, puis demi-longueur de la corde.
    let a = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let (ux, uy) = (dx / d, dy / d);
    let base = (c1.0 + a * ux, c1.1 + a * uy);
    let p1 = (base.0 - h * uy, base.1 + h * ux);
    let p2 = (base.0 + h * uy, base.1 - h * ux);
    let angle = |c: (f64, f64), p: (f64, f64)| (p.1 - c.1).atan2(p.0 - c.0);
    let tau = std::f64::consts::TAU;
    // Grand arc de C1, de P1 à P2, du côté opposé à C2 : les angles
    // croissent (sens horaire, y vers le bas).
    let (a1, mut a2) = (angle(c1, p1), angle(c1, p2));
    while a2 <= a1 {
        a2 += tau;
    }
    // Petit arc de C2, de P2 à P1, à l'intérieur de C1 : les angles
    // décroissent.
    let (b1, mut b2) = (angle(c2, p2), angle(c2, p1));
    while b2 >= b1 {
        b2 -= tau;
    }
    let steps = 16_u32;
    let mut pts = Vec::new();
    for i in 0..=steps {
        let t = a1 + (a2 - a1) * f64::from(i) / f64::from(steps);
        pts.push((c1.0 + r1 * t.cos(), c1.1 + r1 * t.sin()));
    }
    for i in 1..steps {
        let t = b1 + (b2 - b1) * f64::from(i) / f64::from(steps);
        pts.push((c2.0 + r2 * t.cos(), c2.1 + r2 * t.sin()));
    }
    polyline(path, &pts);
    path.close();
}

/// Chemin de l'icône dans la boîte 24 × 24 : lignes médianes (à épaissir)
/// et surfaces pleines, retournés séparément.
#[must_use]
#[allow(clippy::too_many_lines)] // une branche par icône
pub fn geometry(icon: Icon) -> (Path, Path) {
    let mut lines = Path::new();
    let mut fills = Path::new();
    match icon {
        Icon::Open => {
            polyline(
                &mut lines,
                &[
                    (3.0, 7.0),
                    (9.0, 7.0),
                    (11.0, 9.5),
                    (21.0, 9.5),
                    (21.0, 19.0),
                    (3.0, 19.0),
                    (3.0, 7.0),
                ],
            );
        }
        Icon::Prev => polyline(&mut lines, &[(14.5, 6.0), (8.5, 12.0), (14.5, 18.0)]),
        Icon::Next => polyline(&mut lines, &[(9.5, 6.0), (15.5, 12.0), (9.5, 18.0)]),
        Icon::ZoomOut => {
            magnifier(&mut lines);
            polyline(&mut lines, &[(7.5, 10.5), (13.5, 10.5)]);
        }
        Icon::ZoomIn => {
            magnifier(&mut lines);
            polyline(&mut lines, &[(7.5, 10.5), (13.5, 10.5)]);
            polyline(&mut lines, &[(10.5, 7.5), (10.5, 13.5)]);
        }
        Icon::FitWidth => {
            polyline(&mut lines, &[(4.0, 5.0), (4.0, 19.0)]);
            polyline(&mut lines, &[(20.0, 5.0), (20.0, 19.0)]);
            polyline(&mut lines, &[(7.0, 12.0), (17.0, 12.0)]);
            polyline(&mut lines, &[(10.0, 9.0), (7.0, 12.0), (10.0, 15.0)]);
            polyline(&mut lines, &[(14.0, 9.0), (17.0, 12.0), (14.0, 15.0)]);
        }
        Icon::Search => magnifier(&mut lines),
        Icon::Rotate => {
            // Arc horaire de 135° à 405° (rayon 7), pointe de flèche à l'arrivée.
            let pts: Vec<(f64, f64)> = (0..=12)
                .map(|i| {
                    let a = (135.0 + 270.0 * f64::from(i) / 12.0).to_radians();
                    (12.0 + 7.0 * a.cos(), 12.5 + 7.0 * a.sin())
                })
                .collect();
            polyline(&mut lines, &pts);
            polyline(&mut lines, &[(13.5, 3.5), (18.0, 7.5), (13.0, 10.5)]);
        }
        Icon::Undo => {
            for part in undo_points() {
                polyline(&mut lines, &part);
            }
        }
        Icon::Redo => {
            for part in undo_points() {
                polyline(&mut lines, &mirror(&part));
            }
        }
        Icon::Save => {
            // Le boîtier au coin coupé, le volet de métal en haut, l'étiquette
            // en bas : les trois traits qui font une disquette.
            polyline(
                &mut lines,
                &[
                    (4.0, 4.0),
                    (16.5, 4.0),
                    (20.0, 7.5),
                    (20.0, 20.0),
                    (4.0, 20.0),
                    (4.0, 4.0),
                ],
            );
            polyline(
                &mut lines,
                &[(8.0, 4.0), (8.0, 9.0), (15.0, 9.0), (15.0, 4.0)],
            );
            polyline(
                &mut lines,
                &[(7.5, 20.0), (7.5, 14.0), (16.5, 14.0), (16.5, 20.0)],
            );
        }
        Icon::Print => {
            polyline(
                &mut lines,
                &[(7.0, 9.0), (7.0, 4.0), (17.0, 4.0), (17.0, 9.0)],
            );
            polyline(
                &mut lines,
                &[
                    (7.0, 15.0),
                    (4.0, 15.0),
                    (4.0, 9.0),
                    (20.0, 9.0),
                    (20.0, 15.0),
                    (17.0, 15.0),
                ],
            );
            polyline(
                &mut lines,
                &[
                    (7.0, 12.5),
                    (7.0, 20.0),
                    (17.0, 20.0),
                    (17.0, 12.5),
                    (7.0, 12.5),
                ],
            );
        }
        Icon::Tools => {
            for (x, y) in [(4.5, 4.5), (13.5, 4.5), (4.5, 13.5), (13.5, 13.5)] {
                bar(&mut fills, x, y, 6.0, 6.0);
            }
        }
        Icon::EditText => {
            // Crayon en diagonale, pointe en bas à gauche.
            polyline(
                &mut lines,
                &[
                    (4.5, 19.5),
                    (4.5, 16.0),
                    (16.0, 4.5),
                    (19.5, 8.0),
                    (8.0, 19.5),
                    (4.5, 19.5),
                ],
            );
            polyline(&mut lines, &[(13.5, 7.0), (17.0, 10.5)]);
        }
        Icon::AddText => {
            // Cadre en pointillé : quatre coins seulement, comme une zone
            // qu'on vient de tracer.
            for (a, b, c) in [
                ((3.5, 7.5), (3.5, 3.5), (7.5, 3.5)),
                ((16.5, 3.5), (20.5, 3.5), (20.5, 7.5)),
                ((20.5, 16.5), (20.5, 20.5), (16.5, 20.5)),
                ((7.5, 20.5), (3.5, 20.5), (3.5, 16.5)),
            ] {
                polyline(&mut lines, &[a, b, c]);
            }
            polyline(&mut lines, &[(8.0, 8.0), (16.0, 8.0)]);
            polyline(&mut lines, &[(12.0, 8.0), (12.0, 17.0)]);
        }
        Icon::Objects => {
            page(&mut lines, 6.0, 6.5, 12.0, 11.0);
            for (x, y) in [(4.0, 4.5), (16.0, 4.5), (4.0, 15.5), (16.0, 15.5)] {
                bar(&mut fills, x, y, 4.0, 4.0);
            }
        }
        Icon::Sign => {
            // Un paraphe : trois boucles enlevées, puis la ligne de signature.
            polyline(
                &mut lines,
                &[
                    (4.0, 14.5),
                    (7.0, 8.0),
                    (8.5, 14.0),
                    (11.0, 6.5),
                    (12.5, 14.0),
                    (15.0, 9.5),
                    (17.0, 13.5),
                    (20.0, 11.0),
                ],
            );
            polyline(&mut lines, &[(4.0, 19.0), (20.0, 19.0)]);
        }
        Icon::Highlight => {
            polyline(
                &mut lines,
                &[
                    (8.0, 13.0),
                    (15.5, 5.5),
                    (19.0, 9.0),
                    (11.5, 16.5),
                    (8.0, 16.5),
                    (8.0, 13.0),
                ],
            );
            bar(&mut fills, 4.0, 18.5, 16.0, 2.5);
        }
        Icon::Note => {
            polyline(
                &mut lines,
                &[
                    (4.0, 5.0),
                    (20.0, 5.0),
                    (20.0, 15.5),
                    (11.0, 15.5),
                    (7.0, 19.5),
                    (7.0, 15.5),
                    (4.0, 15.5),
                    (4.0, 5.0),
                ],
            );
            polyline(&mut lines, &[(8.0, 9.0), (16.0, 9.0)]);
            polyline(&mut lines, &[(8.0, 12.0), (13.0, 12.0)]);
        }
        Icon::PageInsert => {
            page(&mut lines, 5.0, 3.5, 14.0, 17.0);
            polyline(&mut lines, &[(12.0, 8.0), (12.0, 16.0)]);
            polyline(&mut lines, &[(8.0, 12.0), (16.0, 12.0)]);
        }
        Icon::PageDuplicate => {
            page(&mut lines, 4.0, 3.5, 12.0, 14.0);
            page(&mut lines, 8.0, 6.5, 12.0, 14.0);
        }
        Icon::PageExtract => {
            polyline(
                &mut lines,
                &[
                    (13.0, 3.5),
                    (5.0, 3.5),
                    (5.0, 20.5),
                    (17.0, 20.5),
                    (17.0, 14.0),
                ],
            );
            polyline(&mut lines, &[(11.0, 10.0), (20.5, 10.0)]);
            polyline(&mut lines, &[(17.0, 6.5), (20.5, 10.0), (17.0, 13.5)]);
        }
        Icon::PageDelete => {
            page(&mut lines, 5.0, 3.5, 14.0, 17.0);
            polyline(&mut lines, &[(9.0, 9.0), (15.0, 15.0)]);
            polyline(&mut lines, &[(15.0, 9.0), (9.0, 15.0)]);
        }
        Icon::Lock => {
            // Un cadenas : le corps plein, l'anse au-dessus.
            bar(&mut fills, 5.5, 11.0, 13.0, 9.5);
            // L'anse : deux montants et un demi-cercle approché par une
            // polyligne — assez fine pour se lire à seize pixels.
            let mut anse = vec![(8.5, 11.0), (8.5, 7.5)];
            for step in 0..=8 {
                let angle = std::f64::consts::PI * (1.0 - f64::from(step) / 8.0);
                anse.push((12.0 + 3.5 * angle.cos(), 7.5 - 3.5 * angle.sin()));
            }
            anse.push((15.5, 11.0));
            polyline(&mut lines, &anse);
        }
        Icon::Check => polyline(&mut lines, &[(5.0, 12.5), (10.0, 17.5), (19.0, 7.5)]),
        Icon::ChevronDown => polyline(&mut lines, &[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)]),
        Icon::ChevronUp => polyline(&mut lines, &[(7.0, 14.0), (12.0, 9.0), (17.0, 14.0)]),
        Icon::Close => {
            polyline(&mut lines, &[(7.0, 7.0), (17.0, 17.0)]);
            polyline(&mut lines, &[(17.0, 7.0), (7.0, 17.0)]);
        }
        Icon::Form => {
            // Une page, et deux champs : chacun son étiquette (un trait
            // court) et sa case à remplir.
            page(&mut lines, 4.0, 3.5, 16.0, 17.0);
            polyline(&mut lines, &[(7.0, 9.0), (9.5, 9.0)]);
            page(&mut lines, 11.5, 7.0, 5.5, 4.0);
            polyline(&mut lines, &[(7.0, 15.5), (9.5, 15.5)]);
            page(&mut lines, 11.5, 13.5, 5.5, 4.0);
        }
        Icon::Underline => {
            // Deux montants reliés par un demi-cercle qui passe par le bas.
            let mut u = vec![(7.5, 4.5)];
            u.extend(arc(12.0, 11.0, 4.5, 4.5, 180.0, 0.0));
            u.push((16.5, 4.5));
            polyline(&mut lines, &u);
            bar(&mut fills, 5.0, 18.5, 14.0, 2.2);
        }
        Icon::StrikeOut => {
            // Deux arcs d'un seul tenant : le haut tourne vers la gauche,
            // le bas vers la droite, ils se rejoignent au milieu.
            let mut s = arc(12.0, 8.25, 4.5, 3.75, -30.0, -270.0);
            s.extend(
                arc(12.0, 15.75, 4.5, 3.75, -90.0, 150.0)
                    .into_iter()
                    .skip(1),
            );
            polyline(&mut lines, &s);
            bar(&mut fills, 3.5, 11.1, 17.0, 1.8);
        }
        Icon::Squiggly => {
            polyline(&mut lines, &[(4.5, 6.0), (19.5, 6.0)]);
            polyline(&mut lines, &[(4.5, 11.0), (15.0, 11.0)]);
            let wave: Vec<(f64, f64)> = (0..=8)
                .map(|i| {
                    let y = if i % 2 == 0 { 18.5 } else { 15.5 };
                    (4.0 + 2.0 * f64::from(i), y)
                })
                .collect();
            polyline(&mut lines, &wave);
        }
        Icon::Insert => {
            // La ligne s'ouvre là où le signe pointe.
            polyline(&mut lines, &[(3.5, 7.0), (9.5, 7.0)]);
            polyline(&mut lines, &[(14.5, 7.0), (20.5, 7.0)]);
            polyline(&mut lines, &[(7.5, 19.0), (12.0, 11.0), (16.5, 19.0)]);
        }
        Icon::Replace => {
            polyline(&mut lines, &[(4.0, 5.0), (14.0, 5.0)]);
            polyline(&mut lines, &[(9.0, 5.0), (9.0, 15.0)]);
            bar(&mut fills, 2.5, 9.2, 13.0, 1.8);
            polyline(&mut lines, &[(14.0, 20.0), (17.5, 14.0), (21.0, 20.0)]);
        }
        Icon::Comment => {
            // Bulle ovale, la queue en bas à gauche, trois points dedans.
            let mut bubble = arc(12.0, 10.5, 8.5, 6.5, 120.0, 420.0);
            bubble.push((6.5, 20.0));
            bubble.push((7.75, 16.13));
            polyline(&mut lines, &bubble);
            for x in [8.0, 12.0, 16.0] {
                circle(&mut fills, x, 10.5, 1.3);
            }
        }
        Icon::Rectangle => page(&mut lines, 3.5, 6.0, 17.0, 12.0),
        Icon::Ellipse => polyline(&mut lines, &arc(12.0, 12.0, 8.5, 6.5, 0.0, 360.0)),
        Icon::Line => polyline(&mut lines, &[(4.5, 19.5), (19.5, 4.5)]),
        Icon::Arrow => {
            polyline(&mut lines, &[(4.5, 19.5), (19.0, 5.0)]);
            polyline(&mut lines, &[(11.5, 4.5), (19.5, 4.5), (19.5, 12.5)]);
        }
        Icon::Pencil => {
            // Le trait laissé, qui finit sous la pointe du crayon.
            polyline(
                &mut lines,
                &[(3.5, 20.0), (5.5, 15.5), (8.0, 19.0), (11.0, 13.0)],
            );
            polyline(
                &mut lines,
                &[
                    (11.0, 13.0),
                    (11.0, 10.5),
                    (17.5, 4.0),
                    (20.0, 6.5),
                    (13.5, 13.0),
                    (11.0, 13.0),
                ],
            );
        }
        Icon::TextBox => {
            page(&mut lines, 3.5, 4.5, 17.0, 15.0);
            polyline(&mut lines, &[(8.0, 8.5), (16.0, 8.5)]);
            polyline(&mut lines, &[(12.0, 8.5), (12.0, 15.5)]);
        }
        Icon::Callout => {
            page(&mut lines, 9.0, 3.5, 11.5, 9.5);
            polyline(&mut lines, &[(12.0, 7.0), (17.5, 7.0)]);
            polyline(&mut lines, &[(12.0, 10.0), (15.5, 10.0)]);
            polyline(&mut lines, &[(9.0, 13.0), (4.0, 19.5)]);
            polyline(&mut lines, &[(4.0, 15.0), (4.0, 19.5), (8.5, 19.5)]);
        }
        Icon::Stamp => {
            // Le pommeau, le col, le socle, puis l'empreinte qu'il laisse :
            // sans elle, on lirait un champignon.
            circle(&mut lines, 12.0, 5.8, 2.6);
            polyline(&mut lines, &[(10.4, 8.2), (10.4, 12.0)]);
            polyline(&mut lines, &[(13.6, 8.2), (13.6, 12.0)]);
            polyline(
                &mut lines,
                &[
                    (5.0, 12.0),
                    (19.0, 12.0),
                    (19.0, 16.0),
                    (5.0, 16.0),
                    (5.0, 12.0),
                ],
            );
            polyline(&mut lines, &[(4.0, 20.0), (20.0, 20.0)]);
        }
        Icon::Document => {
            // La page, son coin replié en haut à droite, trois lignes.
            polyline(
                &mut lines,
                &[
                    (14.0, 3.5),
                    (5.5, 3.5),
                    (5.5, 20.5),
                    (18.5, 20.5),
                    (18.5, 8.0),
                    (14.0, 3.5),
                    (14.0, 8.0),
                    (18.5, 8.0),
                ],
            );
            polyline(&mut lines, &[(8.5, 12.0), (15.5, 12.0)]);
            polyline(&mut lines, &[(8.5, 15.0), (15.5, 15.0)]);
            polyline(&mut lines, &[(8.5, 18.0), (13.0, 18.0)]);
        }
        Icon::Image => {
            page(&mut lines, 3.5, 5.0, 17.0, 14.0);
            polyline(
                &mut lines,
                &[
                    (4.0, 17.5),
                    (9.0, 12.0),
                    (12.5, 15.5),
                    (15.0, 13.0),
                    (20.0, 18.0),
                ],
            );
            circle(&mut fills, 15.5, 9.0, 1.7);
        }
        Icon::Trash => {
            // Le couvercle et sa poignée, puis la cuve et ses trois stries.
            polyline(&mut lines, &[(4.0, 6.5), (20.0, 6.5)]);
            polyline(
                &mut lines,
                &[(9.5, 6.5), (9.5, 4.0), (14.5, 4.0), (14.5, 6.5)],
            );
            polyline(
                &mut lines,
                &[(6.0, 6.5), (7.0, 20.0), (17.0, 20.0), (18.0, 6.5)],
            );
            polyline(&mut lines, &[(10.0, 10.0), (10.0, 16.5)]);
            polyline(&mut lines, &[(14.0, 10.0), (14.0, 16.5)]);
        }
        Icon::Reply => {
            // Une flèche qui repart vers la gauche, puis descend : la
            // réponse revient à ce qu'on a dit.
            polyline(&mut lines, &[(9.0, 5.0), (4.0, 10.0), (9.0, 15.0)]);
            let mut tail = vec![(4.0, 10.0), (14.0, 10.0)];
            tail.extend(arc(14.0, 15.0, 6.0, 5.0, -90.0, 0.0).into_iter().skip(1));
            tail.push((20.0, 19.5));
            polyline(&mut lines, &tail);
        }
        Icon::Redact => {
            page(&mut lines, 5.0, 3.5, 14.0, 17.0);
            bar(&mut fills, 7.5, 9.5, 9.0, 5.0);
        }
        Icon::RedactApply => {
            bar(&mut fills, 3.5, 5.0, 11.0, 5.0);
            polyline(&mut lines, &[(3.5, 14.5), (11.5, 14.5)]);
            polyline(&mut lines, &[(12.5, 17.0), (15.5, 20.0), (21.0, 12.0)]);
        }
        Icon::Export => {
            polyline(
                &mut lines,
                &[
                    (13.0, 3.5),
                    (5.0, 3.5),
                    (5.0, 20.5),
                    (17.0, 20.5),
                    (17.0, 12.0),
                ],
            );
            polyline(&mut lines, &[(12.0, 12.5), (20.5, 4.5)]);
            polyline(&mut lines, &[(14.5, 4.5), (20.5, 4.5), (20.5, 10.5)]);
        }
        Icon::Attach => {
            // Trombone d'un seul trait. Le dessiner à deux traits parallèles,
            // comme un vrai trombone, les ferait se toucher à dix-huit pixels
            // et l'icône deviendrait une tache.
            polyline(
                &mut lines,
                &[
                    (17.5, 7.0),
                    (17.5, 15.5),
                    (16.8, 18.2),
                    (14.0, 19.5),
                    (11.2, 18.2),
                    (10.5, 15.5),
                    (10.5, 7.5),
                    (11.0, 5.5),
                    (12.8, 4.5),
                    (14.6, 5.5),
                    (15.1, 7.5),
                    (15.1, 16.0),
                ],
            );
        }
        Icon::ViewMode => {
            polyline(
                &mut lines,
                &[
                    (3.5, 5.0),
                    (11.0, 5.0),
                    (11.0, 19.0),
                    (3.5, 19.0),
                    (3.5, 5.0),
                ],
            );
            polyline(
                &mut lines,
                &[
                    (13.0, 5.0),
                    (20.5, 5.0),
                    (20.5, 19.0),
                    (13.0, 19.0),
                    (13.0, 5.0),
                ],
            );
        }
        Icon::Home => {
            // Un toit et deux murs : la maison la plus simple qui se lise à
            // seize pixels.
            polyline(&mut lines, &[(3.0, 11.5), (12.0, 4.0), (21.0, 11.5)]);
            polyline(
                &mut lines,
                &[(5.5, 10.0), (5.5, 20.0), (18.5, 20.0), (18.5, 10.0)],
            );
            polyline(
                &mut lines,
                &[(9.8, 20.0), (9.8, 14.5), (14.2, 14.5), (14.2, 20.0)],
            );
        }
        Icon::Sidebar => {
            polyline(
                &mut lines,
                &[
                    (4.0, 5.0),
                    (20.0, 5.0),
                    (20.0, 19.0),
                    (4.0, 19.0),
                    (4.0, 5.0),
                ],
            );
            polyline(&mut lines, &[(9.5, 5.0), (9.5, 19.0)]);
        }
        Icon::Settings => {
            // Deux curseurs de réglage : rien à voir avec le soleil du
            // thème, qui rayonne, alors que celui-ci glisse.
            polyline(&mut lines, &[(4.0, 8.5), (20.0, 8.5)]);
            polyline(&mut lines, &[(4.0, 15.5), (20.0, 15.5)]);
            circle(&mut fills, 9.0, 8.5, 2.6);
            circle(&mut fills, 15.0, 15.5, 2.6);
        }
        Icon::Theme => {
            circle(&mut fills, 12.0, 12.0, 4.0);
            for i in 0..8 {
                let a = f64::from(i) * std::f64::consts::FRAC_PI_4;
                let (s, c) = a.sin_cos();
                polyline(
                    &mut lines,
                    &[
                        (12.0 + 6.5 * c, 12.0 + 6.5 * s),
                        (12.0 + 9.0 * c, 12.0 + 9.0 * s),
                    ],
                );
            }
        }
        // Plein, comme le disque du soleil qu'il remplace : les deux états
        // du même bouton ont le même poids.
        Icon::Moon => crescent(&mut fills),
    }
    (lines, fills)
}

/// Contour plein de l'icône (traits épaissis + surfaces), dans la boîte 24 × 24.
#[must_use]
pub fn outline(icon: Icon) -> Path {
    let (lines, fills) = geometry(icon);
    let style = StrokeStyle {
        width: 1.9,
        cap: LineCap::Round,
        join: LineJoin::Round,
        miter_limit: 10.0,
        dash: None,
    };
    let mut out = stroke_path(&lines, &style, &Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0));
    out.append(&fills);
    out
}

/// Dessine l'icône avec son coin supérieur gauche en `(x, y)` et une boîte
/// de `size` pixels de côté.
pub fn draw(
    frame: &mut Frame<'_>,
    raster: &mut Rasterizer,
    icon: Icon,
    x: i32,
    y: i32,
    size: f32,
    color: (u8, u8, u8),
) {
    let px = size.round().max(1.0) as u32;
    let s = f64::from(px) / 24.0;
    let m = Matrix::new(s, 0.0, 0.0, s, 0.0, 0.0);
    let mask = raster.path_coverage(&outline(icon), &m, FillRule::NonZero, px, px);
    let cov = mask.data();
    for row in 0..px {
        let dy = y + row as i32;
        if dy < 0 || dy >= frame.height as i32 {
            continue;
        }
        for col in 0..px {
            let dx = x + col as i32;
            if dx < 0 || dx >= frame.width as i32 {
                continue;
            }
            let a = u32::from(cov[(row * px + col) as usize]);
            if a == 0 {
                continue;
            }
            let i = frame.index(dx as usize, dy as usize);
            let inv = 255 - a;
            let d = &mut frame.pixels[i..i + 4];
            d[0] = ((u32::from(color.2) * a + u32::from(d[0]) * inv) / 255) as u8;
            d[1] = ((u32::from(color.1) * a + u32::from(d[1]) * inv) / 255) as u8;
            d[2] = ((u32::from(color.0) * a + u32::from(d[2]) * inv) / 255) as u8;
            d[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_covers_some_pixels_inside_the_box() {
        let icons = [
            Icon::Open,
            Icon::Prev,
            Icon::Next,
            Icon::ZoomOut,
            Icon::ZoomIn,
            Icon::FitWidth,
            Icon::Search,
            Icon::Theme,
            Icon::Sidebar,
            Icon::Rotate,
            Icon::Undo,
            Icon::Redo,
            Icon::Moon,
            Icon::Save,
            Icon::Print,
            Icon::ViewMode,
            Icon::Check,
            Icon::ChevronDown,
            Icon::ChevronUp,
            Icon::Close,
            Icon::Form,
            Icon::Highlight,
            Icon::Note,
            Icon::Underline,
            Icon::StrikeOut,
            Icon::Squiggly,
            Icon::Insert,
            Icon::Replace,
            Icon::Comment,
            Icon::Rectangle,
            Icon::Ellipse,
            Icon::Line,
            Icon::Arrow,
            Icon::Pencil,
            Icon::TextBox,
            Icon::Callout,
            Icon::Trash,
            Icon::Reply,
            Icon::Stamp,
            Icon::Image,
            Icon::Document,
        ];
        let mut raster = Rasterizer::new();
        for icon in icons {
            let m = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
            let mask = raster.path_coverage(&outline(icon), &m, FillRule::NonZero, 24, 24);
            let covered = mask.data().iter().filter(|&&c| c > 128).count();
            assert!(covered > 20 && covered < 400, "{icon:?} : {covered} pixels");
            let b = outline(icon).bounds().unwrap_or_default();
            assert!(
                b.x0 >= 0.0 && b.y0 >= 0.0 && b.x1 <= 24.0 && b.y1 <= 24.0,
                "{icon:?} déborde"
            );
        }
    }

    /// « Rétablir » est « annuler » retourné : mêmes bornes, en miroir.
    #[test]
    fn redo_mirrors_undo() {
        let undo = outline(Icon::Undo).bounds().unwrap_or_default();
        let redo = outline(Icon::Redo).bounds().unwrap_or_default();
        assert!(
            (redo.x0 - (24.0 - undo.x1)).abs() < 1e-6,
            "{undo:?} / {redo:?}"
        );
        assert!(
            (redo.x1 - (24.0 - undo.x0)).abs() < 1e-6,
            "{undo:?} / {redo:?}"
        );
        assert!((redo.y0 - undo.y0).abs() < 1e-6 && (redo.y1 - undo.y1).abs() < 1e-6);
    }

    /// La lune est un croissant : le bord gauche du disque est plein, le
    /// creux mordu en haut à droite est vide.
    #[test]
    fn moon_is_a_crescent() {
        let mut raster = Rasterizer::new();
        let m = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        let mask = raster.path_coverage(&outline(Icon::Moon), &m, FillRule::NonZero, 24, 24);
        let at = |x: usize, y: usize| mask.data()[y * 24 + x];
        assert!(at(5, 12) > 200, "bord gauche : {}", at(5, 12));
        assert!(at(12, 18) > 200, "bas du croissant : {}", at(12, 18));
        assert_eq!(at(17, 8), 0, "le creux doit rester vide");
        assert_eq!(at(13, 10), 0, "le cœur du disque est mordu");
    }

    #[test]
    fn draw_blends_into_a_frame() {
        let mut pixels = vec![0u8; 32 * 32 * 4];
        let mut frame = Frame::new(32, 32, &mut pixels);
        let mut raster = Rasterizer::new();
        draw(
            &mut frame,
            &mut raster,
            Icon::Search,
            4,
            4,
            24.0,
            (255, 255, 255),
        );
        assert!(pixels.chunks_exact(4).any(|p| p[0] > 200));
    }
}
