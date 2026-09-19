//! Police de secours dessinée par nous, pour les machines sans polices.
//!
//! Quand un PDF n'incorpore pas sa police, il faut bien en trouver une. Sur un
//! poste de travail il y en a toujours ; dans un conteneur, sur une image de
//! compilation minimale, sur un système taillé au plus juste, il n'y en a
//! aucune — et la page reste blanche alors que le fichier est parfaitement
//! valide. Acrobat embarque pour ce cas Adobe Sans MM et Adobe Serif MM. Voici
//! la nôtre.
//!
//! # Des squelettes, pas des contours
//!
//! Chaque lettre est décrite comme on la tracerait à la plume : quelques
//! traits et quelques arcs, sans épaisseur. [`thicken`] les épaissit ensuite
//! d'une largeur constante. Deux raisons, et la seconde est la vraie.
//!
//! La première est que c'est court : un `H` fait trois traits, un `O` fait un
//! arc. La seconde est qu'un squelette ne peut pas se tromper de sens. Un
//! contour rempli, lui, doit tourner dans le bon sens, et ses contre-formes
//! dans l'autre ; une seule erreur et la lettre se remplit à l'envers. Ici les
//! morceaux engendrés tournent tous pareil, le remplissage non nul les réunit,
//! et il n'y a rien à vérifier. Quelqu'un qui veut corriger un `g` déplace un
//! nombre et regarde.
//!
//! # Ce que cela donne
//!
//! Une linéale géométrique, d'un seul déliè : ni empattements, ni contraste,
//! ni italique véritable. Ce n'est pas Helvetica et cela ne prétend pas l'être
//! — c'est la garantie qu'un texte s'affiche, à la bonne place, partout. La
//! chasse est ensuite ajustée à celle que le document déclare
//! (`acrux_render::font::LoadedFont::substitution_fit`), si bien que les
//! lignes tombent juste même si les lettres ne sont pas les bonnes.
//!
//! # Le langage des tracés
//!
//! | Commande | Effet |
//! |---|---|
//! | `M x y` | commence un trait |
//! | `L x y` | segment droit |
//! | `C x1 y1 x2 y2 x y` | cubique de Bézier |
//! | `A cx cy rx ry a0 a1` | arc d'ellipse, angles en degrés |
//! | `. x y` | point (le point du `i`, celui du `?`) |
//!
//! Repères, en millièmes d'em : ligne de base 0, hauteur d'œil 500, hauteur de
//! capitale 700, hampes 730, jambages −210, plume 80.

use std::collections::HashMap;
use std::sync::OnceLock;

use acrux_core::{Matrix, Path, Point};

/// Largeur de plume, en unités de police.
const PEN: f64 = 80.0;
/// Rayon des points (point du `i`, point final) : un peu plus que la plume,
/// sans quoi ils disparaissent à petite taille.
const DOT: f64 = 56.0;
/// Segments par cubique lors de l'aplatissement.
const CURVE_STEPS: usize = 16;
/// Pas d'échantillonnage d'un arc, en degrés.
const ARC_STEP: f64 = 6.0;

/// Un glyphe de la police de secours.
pub struct Glyph {
    /// Contour rempli, en unités de police (1000 par em).
    pub path: Path,
    /// Avance horizontale, en unités de police.
    pub advance: f64,
}

/// Glyphe d'un nom, `None` si la police ne le connaît pas.
///
/// Les composés accentués (`eacute`, `Ccedilla`…) sont assemblés à la volée :
/// la lettre de base, et l'accent posé au-dessus, centré sur elle.
#[must_use]
pub fn glyph(name: &str) -> Option<&'static Glyph> {
    table().get(name).or_else(|| {
        // Assemblé au premier usage puis gardé : voir `composites()`.
        composites().get(name)
    })
}

/// Avance d'un glyphe, en unités de police.
#[must_use]
pub fn advance(name: &str) -> Option<f64> {
    glyph(name).map(|g| g.advance)
}

/// Vrai si la police de secours sait dessiner ce glyphe.
#[must_use]
pub fn has(name: &str) -> bool {
    glyph(name).is_some()
}

/// Nombre d'unités par em : 1000, comme les polices PostScript.
#[must_use]
pub fn units_per_em() -> f64 {
    1000.0
}

/// Nom de glyphe correspondant à un caractère, s'il en existe un dans cette
/// police.
///
/// Sert aux polices composites, où le code n'est pas un nom de glyphe mais un
/// identifiant propre au document : seul le `/ToUnicode` dit de quelle lettre
/// il s'agit.
#[must_use]
pub fn name_for_char(c: char) -> Option<&'static str> {
    for code in 32u8..=255 {
        let Some(name) = crate::encodings::win_ansi(code) else {
            continue;
        };
        if crate::encodings::glyph_name_to_unicode(name) == Some(c) && has(name) {
            return Some(name);
        }
    }
    None
}

/// Table des glyphes simples, construite une fois.
fn table() -> &'static HashMap<&'static str, Glyph> {
    static TABLE: OnceLock<HashMap<&'static str, Glyph>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for (name, advance, spec) in GLYPHS {
            map.insert(
                *name,
                Glyph {
                    path: thicken(&parse(spec), PEN),
                    advance: *advance,
                },
            );
        }
        map
    })
}

/// Table des composés accentués, construite une fois.
fn composites() -> &'static HashMap<String, Glyph> {
    static TABLE: OnceLock<HashMap<String, Glyph>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let simples = table();
        let mut map = HashMap::new();
        for (base, _, _) in GLYPHS {
            if base.len() != 1 || !base.as_bytes()[0].is_ascii_alphabetic() {
                continue;
            }
            for (accent, suffix) in ACCENTS {
                let (Some(lettre), Some(marque)) = (simples.get(base), simples.get(accent)) else {
                    continue;
                };
                let Some(compose) = compose(lettre, marque, accent == "cedilla") else {
                    continue;
                };
                map.insert(format!("{base}{suffix}"), compose);
            }
        }
        map
    })
}

/// Accents composables : nom du glyphe d'accent, et suffixe du nom composé.
const ACCENTS: [(&str, &str); 11] = [
    ("acute", "acute"),
    ("grave", "grave"),
    ("circumflex", "circumflex"),
    ("dieresis", "dieresis"),
    ("tilde", "tilde"),
    ("ring", "ring"),
    ("caron", "caron"),
    ("breve", "breve"),
    ("macron", "macron"),
    ("dotaccent", "dotaccent"),
    ("cedilla", "cedilla"),
];

/// Pose un accent sur une lettre : centré sur elle, au-dessus (ou au-dessous
/// pour la cédille).
fn compose(base: &Glyph, accent: &Glyph, below: bool) -> Option<Glyph> {
    let b = base.path.bounds()?;
    let a = accent.path.bounds()?;
    let dx = f64::midpoint(b.x0, b.x1) - f64::midpoint(a.x0, a.x1);
    // 60 unités d'air entre la lettre et sa marque : assez pour qu'elles ne se
    // touchent pas, assez peu pour que l'accent reste lié à sa lettre.
    let dy = if below {
        b.y0 - a.y1 - 20.0
    } else {
        b.y1 - a.y0 + 60.0
    };
    let mut path = base.path.clone();
    path.append(&accent.path.transform(&Matrix::translate(dx, dy)));
    Some(Glyph {
        path,
        advance: base.advance,
    })
}

/// Un squelette : des traits ouverts, et des points.
#[derive(Default)]
struct Skeleton {
    strokes: Vec<Vec<Point>>,
    dots: Vec<Point>,
}

/// Lit un tracé du langage décrit en tête de module.
///
/// Un tracé illisible ne panique pas : la commande fautive est ignorée, et le
/// glyphe sort incomplet plutôt que de faire tomber le rendu d'une page.
fn parse(spec: &str) -> Skeleton {
    let mut out = Skeleton::default();
    let mut tokens = spec.split_ascii_whitespace();
    let mut current: Vec<Point> = Vec::new();
    let number = |t: &mut std::str::SplitAsciiWhitespace| -> Option<f64> {
        t.next().and_then(|s| s.parse::<f64>().ok())
    };
    while let Some(cmd) = tokens.next() {
        match cmd {
            "M" => {
                if current.len() > 1 {
                    out.strokes.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                if let (Some(x), Some(y)) = (number(&mut tokens), number(&mut tokens)) {
                    current.push(Point::new(x, y));
                }
            }
            "L" => {
                if let (Some(x), Some(y)) = (number(&mut tokens), number(&mut tokens)) {
                    current.push(Point::new(x, y));
                }
            }
            "C" => {
                let values: Vec<f64> = (0..6).filter_map(|_| number(&mut tokens)).collect();
                if values.len() == 6 {
                    if let Some(&start) = current.last() {
                        let c1 = Point::new(values[0], values[1]);
                        let c2 = Point::new(values[2], values[3]);
                        let end = Point::new(values[4], values[5]);
                        for i in 1..=CURVE_STEPS {
                            #[allow(clippy::cast_precision_loss)]
                            let t = i as f64 / CURVE_STEPS as f64;
                            current.push(cubic(start, c1, c2, end, t));
                        }
                    }
                }
            }
            "A" => {
                let values: Vec<f64> = (0..6).filter_map(|_| number(&mut tokens)).collect();
                if values.len() == 6 {
                    if current.len() > 1 {
                        out.strokes.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                    current = arc(
                        values[0], values[1], values[2], values[3], values[4], values[5],
                    );
                }
            }
            "." => {
                if let (Some(x), Some(y)) = (number(&mut tokens), number(&mut tokens)) {
                    out.dots.push(Point::new(x, y));
                }
            }
            _ => {}
        }
    }
    if current.len() > 1 {
        out.strokes.push(current);
    }
    out
}

/// Point d'une cubique de Bézier.
fn cubic(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let reste = 1.0 - t;
    let poids = [
        reste * reste * reste,
        3.0 * reste * reste * t,
        3.0 * reste * t * t,
        t * t * t,
    ];
    let points = [p0, p1, p2, p3];
    let mut out = Point::new(0.0, 0.0);
    for (poids, point) in poids.iter().zip(points) {
        out.x += poids * point.x;
        out.y += poids * point.y;
    }
    out
}

/// Échantillonne un arc d'ellipse, angles en degrés.
fn arc(cx: f64, cy: f64, rx: f64, ry: f64, a0: f64, a1: f64) -> Vec<Point> {
    let span = a1 - a0;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let steps = ((span.abs() / ARC_STEP).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f64 / steps as f64;
            let a = (a0 + span * t).to_radians();
            Point::new(cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// Épaissit un squelette d'une largeur de plume constante.
///
/// Chaque segment devient un quadrilatère, chaque extrémité et chaque brisure
/// un disque. Tous tournent dans le même sens : le remplissage **non nul** les
/// réunit sans qu'aucun ne creuse l'autre. C'est ce qui rend ce fichier sûr à
/// modifier — il n'y a pas de sens de parcours à respecter.
fn thicken(skeleton: &Skeleton, pen: f64) -> Path {
    let r = pen / 2.0;
    let mut path = Path::new();
    for stroke in &skeleton.strokes {
        for pair in stroke.windows(2) {
            let (p, q) = (pair[0], pair[1]);
            let (dx, dy) = (q.x - p.x, q.y - p.y);
            let len = dx.hypot(dy);
            if len < 1e-9 {
                continue;
            }
            let (nx, ny) = (-dy / len * r, dx / len * r);
            path.move_to(Point::new(p.x + nx, p.y + ny));
            path.line_to(Point::new(q.x + nx, q.y + ny));
            path.line_to(Point::new(q.x - nx, q.y - ny));
            path.line_to(Point::new(p.x - nx, p.y - ny));
            path.close();
        }
        // Un disque à chaque sommet, extrémités comprises. Deux
        // quadrilatères consécutifs laissent à leur jointure une encoche
        // minuscule du côté extérieur : invisible sur le papier, mais
        // l'antialiassage la révèle et le contour devient chevelu. Le disque
        // la comble, et sert de bout rond aux extrémités.
        for point in stroke {
            disc(&mut path, *point, r);
        }
    }
    for dot in &skeleton.dots {
        disc(&mut path, *dot, DOT);
    }
    path
}

/// Disque, parcouru dans le même sens que les quadrilatères.
fn disc(path: &mut Path, centre: Point, r: f64) {
    // Constante de l'approximation d'un cercle par quatre cubiques.
    const K: f64 = 0.552_284_749_8;
    let (cx, cy, k) = (centre.x, centre.y, r * K);
    path.move_to(Point::new(cx + r, cy));
    path.curve_to(
        Point::new(cx + r, cy - k),
        Point::new(cx + k, cy - r),
        Point::new(cx, cy - r),
    );
    path.curve_to(
        Point::new(cx - k, cy - r),
        Point::new(cx - r, cy - k),
        Point::new(cx - r, cy),
    );
    path.curve_to(
        Point::new(cx - r, cy + k),
        Point::new(cx - k, cy + r),
        Point::new(cx, cy + r),
    );
    path.curve_to(
        Point::new(cx + k, cy + r),
        Point::new(cx + r, cy + k),
        Point::new(cx + r, cy),
    );
    path.close();
}

/// Les tracés : nom de glyphe, avance, squelette.
///
/// Les avances suivent Helvetica, ce qui donne des lignes d'allure juste quand
/// le document n'en déclare aucune.
const GLYPHS: &[(&str, f64, &str)] = &[
    ("space", 278.0, ""),
    ("exclam", 278.0, "M 139 190 L 139 700 . 139 60"),
    ("quotedbl", 355.0, "M 110 480 L 110 700 M 245 480 L 245 700"),
    (
        "numbersign",
        556.0,
        "M 170 0 L 240 700 M 330 0 L 400 700 M 60 210 L 500 210 M 60 470 L 500 470",
    ),
    (
        "dollar",
        556.0,
        "M 278 -60 L 278 760 M 450 600 C 450 680 360 710 280 700 C 190 690 140 630 155 570 \
         C 175 490 300 470 380 420 C 460 370 480 280 440 210 C 400 130 280 100 200 130 \
         C 140 152 110 190 105 240",
    ),
    (
        "percent",
        889.0,
        "A 190 540 130 130 0 360 A 700 160 130 130 0 360 M 760 700 L 130 0",
    ),
    (
        "ampersand",
        667.0,
        "M 620 0 C 430 -30 120 40 120 230 C 120 370 300 410 380 490 C 440 550 420 670 320 685 \
         C 230 698 180 620 215 555 C 275 440 520 260 610 190",
    ),
    ("quotesingle", 191.0, "M 95 480 L 95 700"),
    ("parenleft", 333.0, "M 250 -150 C 90 0 90 550 250 700"),
    ("parenright", 333.0, "M 83 -150 C 243 0 243 550 83 700"),
    (
        "asterisk",
        389.0,
        "M 194 400 L 194 700 M 65 470 L 325 630 M 65 630 L 325 470",
    ),
    ("plus", 584.0, "M 292 120 L 292 500 M 102 310 L 482 310"),
    ("comma", 278.0, "M 155 30 C 155 -50 120 -110 65 -145"),
    ("hyphen", 333.0, "M 60 300 L 273 300"),
    ("period", 278.0, ". 139 60"),
    ("slash", 278.0, "M 20 -50 L 258 730"),
    ("zero", 556.0, "A 278 350 208 350 0 360"),
    ("one", 556.0, "M 300 0 L 300 700 M 140 560 L 300 700"),
    (
        "two",
        556.0,
        "A 278 500 200 200 180 10 M 476 465 L 90 0 M 90 0 L 495 0",
    ),
    (
        "three",
        556.0,
        "A 285 520 180 180 160 -60 A 278 190 205 195 60 -170",
    ),
    (
        "four",
        556.0,
        "M 400 0 L 400 700 M 400 700 L 60 195 M 60 195 L 520 195",
    ),
    (
        "five",
        556.0,
        "M 140 700 L 480 700 M 140 700 L 128 405 M 128 405 L 270 425 A 288 225 200 225 75 -145",
    ),
    (
        "six",
        556.0,
        "A 278 215 208 215 0 360 M 70 215 C 70 470 175 670 420 700",
    ),
    ("seven", 556.0, "M 60 700 L 500 700 M 500 700 L 205 0"),
    (
        "eight",
        556.0,
        "A 278 515 175 185 0 360 A 278 165 205 165 0 360",
    ),
    (
        "nine",
        556.0,
        "A 278 485 208 215 0 360 M 486 485 C 486 230 380 30 136 0",
    ),
    ("colon", 278.0, ". 139 60 . 139 400"),
    (
        "semicolon",
        278.0,
        ". 139 400 M 155 30 C 155 -50 120 -110 65 -145",
    ),
    ("less", 584.0, "M 480 550 L 110 310 M 110 310 L 480 70"),
    ("equal", 584.0, "M 102 210 L 482 210 M 102 410 L 482 410"),
    ("greater", 584.0, "M 104 550 L 474 310 M 474 310 L 104 70"),
    (
        "question",
        556.0,
        "A 278 540 175 160 180 -30 M 405 430 L 278 300 M 278 300 L 278 230 . 278 60",
    ),
    (
        "at",
        1015.0,
        "A 508 350 190 190 0 360 A 508 350 420 330 -30 320 M 698 350 L 698 160",
    ),
    (
        "A",
        667.0,
        "M 40 0 L 333 700 M 333 700 L 626 0 M 135 215 L 531 215",
    ),
    (
        "B",
        667.0,
        "M 130 0 L 130 700 M 130 700 L 370 700 A 370 525 175 175 90 -90 \
         M 130 350 L 380 350 A 380 175 185 175 90 -90 M 130 0 L 380 0",
    ),
    ("C", 722.0, "A 380 350 290 350 45 315"),
    (
        "D",
        722.0,
        "M 140 0 L 140 700 M 140 700 L 330 700 A 330 350 280 350 90 -90 M 140 0 L 330 0",
    ),
    (
        "E",
        667.0,
        "M 150 0 L 150 700 M 150 700 L 580 700 M 150 355 L 520 355 M 150 0 L 580 0",
    ),
    (
        "F",
        611.0,
        "M 150 0 L 150 700 M 150 700 L 560 700 M 150 370 L 500 370",
    ),
    (
        "G",
        778.0,
        "A 400 350 290 350 45 315 M 450 330 L 690 330 M 690 330 L 690 110",
    ),
    (
        "H",
        722.0,
        "M 140 0 L 140 700 M 582 0 L 582 700 M 140 350 L 582 350",
    ),
    ("I", 278.0, "M 139 0 L 139 700"),
    ("J", 500.0, "M 370 700 L 370 190 A 220 190 150 190 0 -180"),
    (
        "K",
        667.0,
        "M 140 0 L 140 700 M 600 700 L 175 320 M 300 430 L 620 0",
    ),
    ("L", 556.0, "M 150 0 L 150 700 M 150 0 L 520 0"),
    (
        "M",
        833.0,
        "M 110 0 L 110 700 M 110 700 L 416 250 M 416 250 L 722 700 M 722 700 L 722 0",
    ),
    (
        "N",
        722.0,
        "M 140 0 L 140 700 M 140 700 L 582 0 M 582 0 L 582 700",
    ),
    ("O", 778.0, "A 389 350 320 350 0 360"),
    (
        "P",
        667.0,
        "M 140 0 L 140 700 M 140 700 L 360 700 A 360 520 180 180 90 -90 M 140 340 L 360 340",
    ),
    ("Q", 778.0, "A 389 350 320 350 0 360 M 470 175 L 680 -40"),
    (
        "R",
        722.0,
        "M 140 0 L 140 700 M 140 700 L 360 700 A 360 520 180 180 90 -90 M 140 340 L 360 340 \
         M 350 340 L 600 0",
    ),
    (
        "S",
        667.0,
        "M 570 560 C 570 670 450 720 330 705 C 210 690 130 610 155 515 C 180 420 340 390 450 345 \
         C 560 300 590 200 545 125 C 500 40 350 -15 240 20 C 155 46 110 95 100 155",
    ),
    ("T", 611.0, "M 305 0 L 305 700 M 60 700 L 550 700"),
    (
        "U",
        722.0,
        "M 140 700 L 140 240 A 361 240 221 240 180 360 M 582 700 L 582 240",
    ),
    ("V", 667.0, "M 40 700 L 333 0 M 333 0 L 626 700"),
    (
        "W",
        944.0,
        "M 40 700 L 250 0 M 250 0 L 472 560 M 472 560 L 694 0 M 694 0 L 904 700",
    ),
    ("X", 667.0, "M 60 0 L 607 700 M 60 700 L 607 0"),
    (
        "Y",
        667.0,
        "M 60 700 L 333 350 M 607 700 L 333 350 M 333 350 L 333 0",
    ),
    (
        "Z",
        611.0,
        "M 70 700 L 541 700 M 541 700 L 70 0 M 70 0 L 541 0",
    ),
    (
        "bracketleft",
        278.0,
        "M 250 -150 L 90 -150 M 90 -150 L 90 700 M 90 700 L 250 700",
    ),
    ("backslash", 278.0, "M 20 730 L 258 -50"),
    (
        "bracketright",
        278.0,
        "M 28 -150 L 188 -150 M 188 -150 L 188 700 M 188 700 L 28 700",
    ),
    (
        "asciicircum",
        469.0,
        "M 60 400 L 234 660 M 234 660 L 409 400",
    ),
    ("underscore", 556.0, "M 0 -120 L 556 -120"),
    ("grave", 333.0, "M 100 700 L 233 580"),
    ("a", 556.0, "A 250 250 190 250 0 360 M 440 500 L 440 0"),
    ("b", 556.0, "M 110 730 L 110 0 A 320 250 210 250 0 360"),
    ("c", 500.0, "A 265 250 195 250 45 315"),
    ("d", 556.0, "M 446 730 L 446 0 A 236 250 210 250 0 360"),
    ("e", 556.0, "A 278 250 208 250 -35 255 M 128 250 L 440 250"),
    (
        "f",
        278.0,
        "M 200 0 L 200 600 C 200 700 265 740 340 715 M 70 500 L 330 500",
    ),
    (
        "g",
        556.0,
        "A 270 250 200 250 0 360 M 470 500 L 470 -55 A 290 -55 180 155 0 -180",
    ),
    (
        "h",
        556.0,
        "M 110 730 L 110 0 A 300 310 190 190 0 180 M 490 310 L 490 0",
    ),
    ("i", 222.0, "M 111 0 L 111 500 . 111 650"),
    (
        "j",
        222.0,
        "M 111 500 L 111 -50 A -20 -50 131 150 0 -180 . 111 650",
    ),
    (
        "k",
        500.0,
        "M 110 730 L 110 0 M 460 500 L 145 225 M 250 315 L 480 0",
    ),
    ("l", 222.0, "M 111 0 L 111 730"),
    (
        "m",
        833.0,
        "M 100 0 L 100 500 A 257 343 157 157 0 180 M 414 343 L 414 0 \
         A 571 343 157 157 0 180 M 728 343 L 728 0",
    ),
    (
        "n",
        556.0,
        "M 110 0 L 110 500 A 300 310 190 190 0 180 M 490 310 L 490 0",
    ),
    ("o", 556.0, "A 278 250 208 250 0 360"),
    ("p", 556.0, "M 110 -210 L 110 500 A 320 250 210 250 0 360"),
    ("q", 556.0, "M 446 -210 L 446 500 A 236 250 210 250 0 360"),
    ("r", 333.0, "M 110 0 L 110 500 A 290 320 180 180 180 25"),
    (
        "s",
        500.0,
        "M 425 405 C 425 480 330 515 250 502 C 170 489 115 435 135 375 \
         C 155 315 255 293 335 262 C 415 231 438 170 410 120 C 380 62 275 22 190 48 \
         C 130 66 95 100 85 145",
    ),
    (
        "t",
        278.0,
        "M 180 600 L 180 105 A 248 105 68 105 180 355 M 55 500 L 310 500",
    ),
    (
        "u",
        556.0,
        "M 110 500 L 110 195 A 300 195 190 195 180 360 M 490 500 L 490 0",
    ),
    ("v", 500.0, "M 40 500 L 250 0 M 250 0 L 460 500"),
    (
        "w",
        722.0,
        "M 40 500 L 200 0 M 200 0 L 361 390 M 361 390 L 522 0 M 522 0 L 682 500",
    ),
    ("x", 500.0, "M 60 0 L 440 500 M 60 500 L 440 0"),
    ("y", 500.0, "M 40 500 L 250 0 M 460 500 L 155 -210"),
    (
        "z",
        500.0,
        "M 60 500 L 440 500 M 440 500 L 60 0 M 60 0 L 440 0",
    ),
    (
        "braceleft",
        334.0,
        "M 285 -150 C 185 -150 205 215 110 275 C 205 335 185 700 285 700",
    ),
    ("bar", 260.0, "M 130 -150 L 130 730"),
    (
        "braceright",
        334.0,
        "M 49 -150 C 149 -150 129 215 224 275 C 129 335 149 700 49 700",
    ),
    (
        "asciitilde",
        584.0,
        "M 90 350 C 150 440 240 440 292 390 C 344 340 434 340 494 430",
    ),
    // Accents, dessinés centrés sur x = 0 et posés au-dessus de y = 0 ;
    // `compose` les déplace sur la lettre.
    ("acute", 333.0, "M -55 0 L 65 140"),
    ("circumflex", 333.0, "M -85 0 L 0 145 M 0 145 L 85 0"),
    (
        "tilde",
        333.0,
        "M -100 40 C -60 120 -20 120 0 70 C 20 20 60 20 100 100",
    ),
    ("macron", 333.0, "M -95 45 L 95 45"),
    ("breve", 333.0, "A 0 130 95 95 200 -20"),
    ("dotaccent", 333.0, ". 0 70"),
    ("dieresis", 333.0, ". -75 70 . 75 70"),
    ("ring", 333.0, "A 0 95 80 80 0 360"),
    ("cedilla", 333.0, "M 0 0 C 0 -60 -45 -90 -95 -95"),
    ("caron", 333.0, "M -85 145 L 0 0 M 0 0 L 85 145"),
    ("hungarumlaut", 333.0, "M -95 0 L -25 140 M 35 0 L 105 140"),
    ("ogonek", 333.0, "M 0 0 C 0 -55 -85 -75 -60 -140"),
    // Quelques glyphes que WinAnsi réclame et qui ne sont pas des composés.
    ("dotlessi", 278.0, "M 139 0 L 139 500"),
    (
        "germandbls",
        611.0,
        "M 110 0 L 110 590 A 270 590 160 110 180 0 M 430 590 L 430 430 \
         C 430 370 300 350 300 300 C 300 250 470 250 470 140 C 470 50 380 10 300 30",
    ),
    (
        "ae",
        889.0,
        "A 220 250 170 250 0 360 M 390 500 L 390 0 A 650 250 185 250 -35 255 M 520 250 L 790 250",
    ),
    (
        "oe",
        944.0,
        "A 230 250 195 250 0 360 M 425 0 L 425 500 A 690 250 195 250 -35 255 M 560 250 L 840 250",
    ),
    (
        "oslash",
        611.0,
        "A 305 250 230 250 0 360 M 80 -20 L 530 520",
    ),
    (
        "Oslash",
        778.0,
        "A 389 350 320 350 0 360 M 80 -40 L 700 740",
    ),
    (
        "AE",
        1000.0,
        "M 40 0 L 333 700 M 333 700 L 950 700 M 333 700 L 333 0 M 333 0 L 950 0 \
         M 333 355 L 880 355 M 135 215 L 333 215",
    ),
    (
        "OE",
        1000.0,
        "A 400 350 300 350 45 315 M 460 0 L 460 700 M 460 700 L 950 700 \
         M 460 355 L 890 355 M 460 0 L 950 0",
    ),
    (
        "Lslash",
        556.0,
        "M 150 0 L 150 700 M 150 0 L 520 0 M 50 260 L 290 400",
    ),
    ("lslash", 222.0, "M 111 0 L 111 730 M 10 290 L 215 420"),
    (
        "Eth",
        722.0,
        "M 140 0 L 140 700 M 140 700 L 330 700 A 330 350 280 350 90 -90 M 140 0 L 330 0 \
         M 60 350 L 300 350",
    ),
    (
        "Thorn",
        667.0,
        "M 140 0 L 140 700 M 140 560 L 360 560 A 360 380 180 180 90 -90 M 140 200 L 360 200",
    ),
    (
        "eth",
        556.0,
        "A 278 250 208 250 0 360 M 200 620 L 470 470 M 300 700 C 380 620 470 520 470 380",
    ),
    (
        "thorn",
        556.0,
        "M 110 -210 L 110 610 A 320 250 210 250 0 360",
    ),
    (
        "Euro",
        556.0,
        "A 320 350 250 300 50 310 M 40 270 L 400 270 M 40 420 L 400 420",
    ),
    ("bullet", 350.0, "A 175 300 110 110 0 360"),
    ("endash", 556.0, "M 40 300 L 516 300"),
    ("emdash", 1000.0, "M 20 300 L 980 300"),
    ("quoteleft", 222.0, "M 130 700 C 60 660 55 570 105 530"),
    ("quoteright", 222.0, "M 92 700 C 162 660 167 570 117 530"),
    (
        "quotedblleft",
        333.0,
        "M 130 700 C 60 660 55 570 105 530 M 275 700 C 205 660 200 570 250 530",
    ),
    (
        "quotedblright",
        333.0,
        "M 92 700 C 162 660 167 570 117 530 M 237 700 C 307 660 312 570 262 530",
    ),
    ("quotesinglbase", 222.0, "M 92 160 C 162 120 167 30 117 -10"),
    (
        "quotedblbase",
        333.0,
        "M 92 160 C 162 120 167 30 117 -10 M 237 160 C 307 120 312 30 262 -10",
    ),
    (
        "guilsinglleft",
        333.0,
        "M 250 400 L 90 240 M 90 240 L 250 80",
    ),
    (
        "guilsinglright",
        333.0,
        "M 83 400 L 243 240 M 243 240 L 83 80",
    ),
    (
        "guillemotleft",
        556.0,
        "M 250 400 L 90 240 M 90 240 L 250 80 M 480 400 L 320 240 M 320 240 L 480 80",
    ),
    (
        "guillemotright",
        556.0,
        "M 76 400 L 236 240 M 236 240 L 76 80 M 306 400 L 466 240 M 466 240 L 306 80",
    ),
    ("ellipsis", 1000.0, ". 160 60 . 500 60 . 840 60"),
    ("dagger", 556.0, "M 278 0 L 278 700 M 110 520 L 446 520"),
    (
        "daggerdbl",
        556.0,
        "M 278 0 L 278 700 M 110 520 L 446 520 M 110 160 L 446 160",
    ),
    ("periodcentered", 278.0, ". 139 290"),
    (
        "paragraph",
        537.0,
        "M 300 -100 L 300 700 M 430 -100 L 430 700 M 300 700 L 190 700 \
         A 190 560 140 140 90 270 M 190 420 L 430 420",
    ),
    ("exclamdown", 333.0, "M 166 -190 L 166 320 . 166 450"),
    (
        "questiondown",
        611.0,
        "A 305 -30 175 160 0 210 M 178 100 L 305 230 M 305 230 L 305 300 . 305 450",
    ),
    (
        "cent",
        556.0,
        "A 278 250 190 250 45 315 M 278 -60 L 278 560",
    ),
    (
        "sterling",
        556.0,
        "M 60 0 L 500 0 M 110 0 C 260 60 340 190 320 330 C 305 440 240 520 150 520 \
         M 90 290 L 400 290",
    ),
    (
        "yen",
        556.0,
        "M 60 700 L 278 380 M 496 700 L 278 380 M 278 380 L 278 0 M 110 300 L 446 300 \
         M 110 160 L 446 160",
    ),
    (
        "florin",
        556.0,
        "M 120 -180 C 220 -140 300 -20 320 180 C 345 420 400 620 520 700 M 90 300 L 430 300",
    ),
    (
        "section",
        556.0,
        "M 440 560 C 440 640 330 680 240 660 C 150 640 110 560 180 500 \
         C 260 430 420 400 420 300 C 420 210 300 170 200 190 M 120 130 C 120 50 230 10 320 30 \
         C 410 50 450 130 380 190",
    ),
    (
        "currency",
        556.0,
        "A 278 350 150 150 0 360 M 100 530 L 175 455 M 456 530 L 381 455 \
         M 100 170 L 175 245 M 456 170 L 381 245",
    ),
    (
        "brokenbar",
        260.0,
        "M 130 -150 L 130 230 M 130 400 L 130 730",
    ),
    (
        "copyright",
        737.0,
        "A 368 350 330 330 0 360 A 368 350 165 165 50 310",
    ),
    (
        "registered",
        737.0,
        "A 368 350 330 330 0 360 M 290 180 L 290 520 M 290 520 L 400 520 \
         A 400 440 80 80 90 -90 M 290 360 L 400 360 M 390 360 L 480 180",
    ),
    (
        "trademark",
        1000.0,
        "M 60 700 L 340 700 M 200 700 L 200 420 M 420 420 L 420 700 M 420 700 L 570 470 \
         M 570 470 L 720 700 M 720 700 L 720 420",
    ),
    (
        "logicalnot",
        584.0,
        "M 60 400 L 524 400 M 524 400 L 524 180",
    ),
    (
        "plusminus",
        584.0,
        "M 292 180 L 292 560 M 102 370 L 482 370 M 102 60 L 482 60",
    ),
    ("multiply", 584.0, "M 130 100 L 454 470 M 130 470 L 454 100"),
    ("divide", 584.0, "M 102 290 L 482 290 . 292 450 . 292 130"),
    ("minus", 584.0, "M 102 310 L 482 310"),
    ("fraction", 167.0, "M -60 -30 L 227 730"),
    ("degree", 400.0, "A 200 590 120 120 0 360"),
    (
        "onesuperior",
        333.0,
        "M 180 380 L 180 700 M 80 620 L 180 700",
    ),
    (
        "twosuperior",
        333.0,
        "A 166 610 100 90 180 10 M 264 585 L 60 380 M 60 380 L 280 380",
    ),
    (
        "threesuperior",
        333.0,
        "A 170 625 90 75 160 -60 A 166 470 100 90 60 -170",
    ),
    (
        "mu",
        556.0,
        "M 110 -210 L 110 500 M 110 195 A 300 195 190 195 180 360 M 490 500 L 490 0",
    ),
    (
        "onequarter",
        834.0,
        "M 160 380 L 160 700 M 80 630 L 160 700 M 600 700 L 200 0 \
         M 680 0 L 680 320 M 680 320 L 500 110 M 500 110 L 790 110",
    ),
    (
        "onehalf",
        834.0,
        "M 160 380 L 160 700 M 80 630 L 160 700 M 600 700 L 200 0 \
         A 660 250 95 85 180 10 M 753 230 L 560 30 M 560 30 L 770 30",
    ),
    (
        "threequarters",
        834.0,
        "A 170 625 90 75 160 -60 A 166 470 100 90 60 -170 M 640 700 L 240 0 \
         M 700 0 L 700 320 M 700 320 L 520 110 M 520 110 L 800 110",
    ),
    (
        "ordfeminine",
        370.0,
        "A 160 560 110 110 0 360 M 270 670 L 270 450 M 60 380 L 310 380",
    ),
    (
        "ordmasculine",
        365.0,
        "A 182 570 120 120 0 360 M 60 380 L 310 380",
    ),
    (
        "perthousand",
        1000.0,
        "A 150 540 110 110 0 360 A 500 160 110 110 0 360 \
         A 830 160 110 110 0 360 M 560 700 L 90 0",
    ),
    (
        "fi",
        500.0,
        "M 200 0 L 200 600 C 200 700 265 740 340 715 M 70 500 L 330 500 \
         M 390 0 L 390 500 . 390 650",
    ),
    (
        "fl",
        500.0,
        "M 200 0 L 200 600 C 200 700 265 740 340 715 M 70 500 L 330 500 \
         M 390 0 L 390 730",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_lettres_latines_sont_toutes_dessinees() {
        // Tout l'ASCII imprimable doit avoir un glyphe.
        for code in 33u8..=126 {
            let name = crate::encodings::win_ansi(code).unwrap_or("");
            assert!(has(name), "code {code} ({name}) manque");
            let g = glyph(name).unwrap_or_else(|| unreachable!());
            assert!(g.path.bounds().is_some(), "{name} ne dessine rien");
        }
        // L'espace existe et ne dessine rien.
        assert!(has("space"));
        assert!(glyph("space").and_then(|g| g.path.bounds()).is_none());
    }

    #[test]
    fn les_composes_portent_leur_accent() {
        let e = glyph("e").and_then(|g| g.path.bounds());
        let ea = glyph("eacute").and_then(|g| g.path.bounds());
        let (Some(e), Some(ea)) = (e, ea) else {
            unreachable!("e et eacute doivent exister")
        };
        // Même lettre dessous, quelque chose de plus haut dessus.
        assert!((ea.y0 - e.y0).abs() < 1.0);
        assert!(ea.y1 > e.y1 + 50.0, "l'accent doit dépasser la lettre");
        // La cédille descend sous la ligne de base.
        let c = glyph("c").and_then(|g| g.path.bounds());
        let cc = glyph("ccedilla").and_then(|g| g.path.bounds());
        let (Some(c), Some(cc)) = (c, cc) else {
            unreachable!("c et ccedilla doivent exister")
        };
        assert!(cc.y0 < c.y0 - 20.0);
        assert!(has("Adieresis") && has("ntilde") && has("Aring") && has("scaron"));
    }

    #[test]
    fn les_glyphes_tiennent_dans_leur_em() {
        // Rien ne doit partir à l'infini : une coordonnée aberrante dans les
        // tracés se verrait ici avant de se voir sur une page.
        for (name, _, _) in GLYPHS {
            let Some(b) = glyph(name).and_then(|g| g.path.bounds()) else {
                continue;
            };
            assert!(
                b.x0 > -300.0 && b.x1 < 1300.0 && b.y0 > -400.0 && b.y1 < 900.0,
                "{name} déborde : {b:?}"
            );
        }
    }

    #[test]
    fn un_trace_fautif_ne_fait_pas_tomber_le_rendu() {
        // Commande inconnue, nombre manquant, nombre illisible.
        let s = parse("M 0 0 L 100 X 100 Q 5 5 A 1 2");
        assert!(s.strokes.len() <= 1);
        let s = parse("");
        assert!(s.strokes.is_empty() && s.dots.is_empty());
    }

    #[test]
    fn lepaississement_reunit_sans_creuser() {
        // Deux segments qui se croisent : le remplissage non nul doit donner
        // une seule forme pleine, pas un trou à l'intersection. On le vérifie
        // sur les sens de parcours : tous les morceaux tournent pareil.
        let path = thicken(&parse("M 0 0 L 100 0 M 50 -50 L 50 50"), 20.0);
        let mut aires = Vec::new();
        let mut depart = Point::new(0.0, 0.0);
        let mut courant = Point::new(0.0, 0.0);
        let mut aire = 0.0;
        for cmd in path.commands() {
            match cmd {
                acrux_core::PathCommand::MoveTo(p) => {
                    depart = *p;
                    courant = *p;
                    aire = 0.0;
                }
                acrux_core::PathCommand::LineTo(p) | acrux_core::PathCommand::CurveTo(_, _, p) => {
                    aire += courant.x * p.y - p.x * courant.y;
                    courant = *p;
                }
                acrux_core::PathCommand::Close => {
                    aire += courant.x * depart.y - depart.x * courant.y;
                    aires.push(aire);
                    courant = depart;
                }
            }
        }
        assert!(aires.len() >= 6, "{} morceaux", aires.len());
        assert!(
            aires.iter().all(|a| *a < 0.0) || aires.iter().all(|a| *a > 0.0),
            "les morceaux doivent tourner tous dans le même sens : {aires:?}"
        );
    }
}
