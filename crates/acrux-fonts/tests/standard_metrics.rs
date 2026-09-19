//! Les métriques des quatorze polices standard, recoupées avec les polices du
//! système qui leur sont **métriquement compatibles**.
//!
//! Arial, Times New Roman et Courier New ont été dessinées pour occuper
//! exactement la même place qu'Helvetica, Times et Courier : c'était leur
//! raison d'être. Symbol de Monotype suit l'original d'Adobe de la même façon.
//! Cela donne un juge extérieur : si une largeur de nos tables était mal
//! recopiée, elle se détacherait de la police du système.
//!
//! La compatibilité est parfaite sur les codes 32 à 126, qui sont le cœur de
//! l'affaire. Au-delà, Arial s'écarte délibérément d'Helvetica sur quelques
//! signes (`÷` et `±` 549 contre 584, `¯` 552 contre 333, `·` 333 contre 278,
//! `µ` 576 contre 556) : ces glyphes-là sont exclus, avec leur valeur, plutôt
//! que d'aligner nos tables sur Arial et de trahir l'AFM.
//!
//! Une police absente n'est pas un échec : le test se termine par un `return`
//! silencieux, sauf si `ACRUX_REQUIRE_FONTS=1` réclame la vérification.

#![allow(clippy::unwrap_used, clippy::panic)]

use acrux_fonts::standard::Standard;
use acrux_fonts::{encodings, TrueTypeFont};

/// Charge une police système ; `None` si elle n'est pas installée.
fn system_font(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(r"C:\Windows\Fonts\{name}")).ok()
}

/// Vrai si l'environnement exige que les polices soient là.
fn required() -> bool {
    std::env::var("ACRUX_REQUIRE_FONTS").is_ok_and(|v| v == "1")
}

/// Avance d'un caractère dans une police, en millièmes d'em.
fn advance(font: &TrueTypeFont, c: char) -> Option<f64> {
    let gid = font.unicode_to_gid(c)?;
    let adv = font.advance(gid)?;
    let upem = f64::from(font.units_per_em().max(1));
    Some((f64::from(adv) * 1000.0 / upem).round())
}

/// Les 95 codes 32 à 126, sur les six tables latines.
#[test]
fn les_codes_ascii_concordent_avec_les_polices_compatibles() {
    let paires = [
        ("arial.ttf", Standard::Helvetica),
        ("arialbd.ttf", Standard::HelveticaBold),
        ("times.ttf", Standard::TimesRoman),
        ("timesbd.ttf", Standard::TimesBold),
        ("timesi.ttf", Standard::TimesItalic),
        ("timesbi.ttf", Standard::TimesBoldItalic),
        ("cour.ttf", Standard::Courier),
        ("courbd.ttf", Standard::CourierBold),
    ];
    let mut verifiees = 0;
    for (fichier, police) in paires {
        let Some(data) = system_font(fichier) else {
            assert!(!required(), "{fichier} absente et ACRUX_REQUIRE_FONTS=1");
            continue;
        };
        let font = TrueTypeFont::parse(&data).unwrap();
        for code in 32u8..=126 {
            let c = char::from(code);
            let Some(systeme) = advance(&font, c) else {
                continue;
            };
            let notre = police
                .width_by_char(c)
                .unwrap_or_else(|| panic!("{fichier} : largeur inconnue pour {c:?}"));
            assert!(
                (notre - systeme).abs() < 0.5,
                "{fichier} {c:?} : table {notre}, police {systeme}"
            );
        }
        verifiees += 1;
    }
    assert!(!required() || verifiees == paires.len());
}

/// Les signes du haut de WinAnsi, moins ceux où Arial s'écarte d'Helvetica.
#[test]
fn les_signes_hors_ascii_concordent_aussi() {
    // Glyphes qu'Arial et Times New Roman dessinent à une autre largeur que
    // l'AFM d'Adobe : la table garde l'AFM, qui fait foi pour un PDF.
    const ECARTS: [char; 5] = ['÷', '±', '¯', 'µ', '·'];
    let paires = [
        ("arial.ttf", Standard::Helvetica),
        ("arialbd.ttf", Standard::HelveticaBold),
        ("times.ttf", Standard::TimesRoman),
        ("timesbd.ttf", Standard::TimesBold),
        ("timesi.ttf", Standard::TimesItalic),
        ("timesbi.ttf", Standard::TimesBoldItalic),
    ];
    for (fichier, police) in paires {
        let Some(data) = system_font(fichier) else {
            assert!(!required(), "{fichier} absente et ACRUX_REQUIRE_FONTS=1");
            continue;
        };
        let font = TrueTypeFont::parse(&data).unwrap();
        let mut comparees = 0;
        for code in 128u8..=255 {
            let Some(nom) = encodings::win_ansi(code) else {
                continue;
            };
            let Some(c) = encodings::glyph_name_to_unicode(nom) else {
                continue;
            };
            if ECARTS.contains(&c) {
                continue;
            }
            let Some(systeme) = advance(&font, c) else {
                continue;
            };
            let notre = police
                .width_by_name(nom)
                .unwrap_or_else(|| panic!("{fichier} : largeur inconnue pour {nom}"));
            assert!(
                (notre - systeme).abs() < 0.5,
                "{fichier} {nom} : table {notre}, police {systeme}"
            );
            comparees += 1;
        }
        // La plage haute de WinAnsi compte une centaine de glyphes ; en
        // comparer moins voudrait dire que la boucle ne teste plus rien.
        assert!(comparees > 80, "{fichier} : {comparees} glyphes comparés");
    }
}

/// Symbol : la table d'Adobe et celle de Monotype coïncident.
#[test]
fn symbol_concorde_avec_la_police_du_systeme() {
    let Some(data) = system_font("symbol.ttf") else {
        assert!(!required(), "symbol.ttf absente et ACRUX_REQUIRE_FONTS=1");
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let mut comparees = 0;
    for code in 32u32..=255 {
        // Police symbolique : sa cmap (3,0) vit dans la zone privée F000.
        let Some(gid) = font
            .cmap_lookup(3, 0, 0xF000 + code)
            .or_else(|| font.cmap_lookup(3, 0, code))
        else {
            continue;
        };
        let Some(adv) = font.advance(gid) else {
            continue;
        };
        let upem = f64::from(font.units_per_em().max(1));
        let systeme = (f64::from(adv) * 1000.0 / upem).round();
        let Ok(octet) = u8::try_from(code) else {
            continue;
        };
        let Some(nom) = encodings::symbol(octet) else {
            continue;
        };
        let Some(notre) = Standard::Symbol.width_by_name(nom) else {
            continue;
        };
        assert!(
            (notre - systeme).abs() < 0.5,
            "symbol {nom} : table {notre}, police {systeme}"
        );
        comparees += 1;
    }
    assert!(comparees > 150, "symbol : {comparees} glyphes comparés");
}

/// Les accentués prennent l'avance de leur lettre de base — vérifié contre la
/// police du système, pas simplement affirmé.
#[test]
fn les_composes_ont_bien_lavance_de_leur_lettre() {
    let Some(data) = system_font("arial.ttf") else {
        assert!(!required(), "arial.ttf absente et ACRUX_REQUIRE_FONTS=1");
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    for (compose, base) in [
        ('é', 'e'),
        ('À', 'A'),
        ('ç', 'c'),
        ('ñ', 'n'),
        ('ü', 'u'),
        ('Ý', 'Y'),
        ('š', 's'),
        ('ž', 'z'),
    ] {
        let (Some(a), Some(b)) = (advance(&font, compose), advance(&font, base)) else {
            continue;
        };
        assert!(
            (a - b).abs() < 0.5,
            "{compose:?} et {base:?} devraient avoir la même avance"
        );
        assert_eq!(
            Standard::Helvetica.width_by_char(compose),
            Standard::Helvetica.width_by_char(base)
        );
    }
}
