//! Composition typographique vérifiée sur les **vraies polices** de la
//! machine (`C:\Windows\Fonts`).
//!
//! Chaque test qui a besoin d'une police absente se termine par un `return`
//! silencieux : le manque d'une police n'est pas un échec, c'est une
//! machine différente. Les polices utilisées sont celles qui accompagnent
//! Windows depuis vingt ans : Calibri, Georgia, Arial, Times New Roman,
//! Tahoma, MS Gothic.
//!
//! Les valeurs attendues ne sont **jamais** recopiées d'une exécution : ce
//! sont des propriétés vérifiables (« la ligature produit moins de glyphes
//! que de caractères », « la paire crénée est plus étroite que la somme des
//! avances », « les quatre formes arabes diffèrent »).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::similar_names
)]

use acrux_fonts::opentype::KernTable;
use acrux_fonts::shape::{shaped_width, Direction, ShapeOptions, ShapedGlyph, Shaper};
use acrux_fonts::TrueTypeFont;

/// Charge une police système ; `None` si elle n'est pas installée.
fn system_font(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!(r"C:\Windows\Fonts\{name}")).ok()
}

/// Identifiants des glyphes produits.
fn gids(glyphs: &[ShapedGlyph]) -> Vec<u16> {
    glyphs.iter().map(|g| g.gid).collect()
}

/// Ligature `fi`, `fl`, `ff` et `ffi` de Calibri : un seul glyphe là où il y
/// avait deux ou trois caractères, et la grappe du premier caractère.
#[test]
fn calibri_forms_the_f_ligatures() {
    let Some(data) = system_font("calibri.ttf") else {
        return; // Calibri absente de cette machine.
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let shaper = Shaper::new(&font);
    for (text, characters) in [("fi", 2), ("fl", 2), ("ff", 2), ("ffi", 3)] {
        let plain = shaper.shape(text, &ShapeOptions::plain());
        let shaped = shaper.shape(text, &ShapeOptions::default());
        assert_eq!(plain.len(), characters, "{text} sans fonctionnalité");
        assert_eq!(shaped.len(), 1, "{text} doit donner une ligature");
        assert_eq!(shaped[0].cluster, 0);
        assert_ne!(shaped[0].gid, plain[0].gid, "{text} : glyphe de ligature");
        // La ligature n'est pas plus large que les glyphes séparés.
        assert!(shaped_width(&shaped) <= shaped_width(&plain) + 1.0);
    }
}

/// Les grappes restent des indices d'octets : dans « affiche », la ligature
/// `ffi` porte la grappe du premier `f`, et les lettres suivantes gardent
/// leur position réelle dans la chaîne.
#[test]
fn calibri_ligature_keeps_byte_clusters() {
    let Some(data) = system_font("calibri.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let glyphs = Shaper::new(&font).shape("affiche", &ShapeOptions::default());
    let clusters: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(clusters, vec![0, 1, 4, 5, 6], "a | ffi | c | h | e");
    // Chaque grappe désigne bien un début de caractère de la chaîne.
    for c in clusters {
        assert!("affiche".is_char_boundary(c as usize));
    }
}

/// Georgia range sa ligature `fi` dans `dlig` (discrétionnaire) et non dans
/// `liga` : elle ne doit apparaître que si on la demande.
#[test]
fn georgia_discretionary_ligature() {
    let Some(data) = system_font("georgia.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let shaper = Shaper::new(&font);
    let standard = shaper.shape("fi", &ShapeOptions::default());
    let discretionary = shaper.shape("fi", &ShapeOptions::default().with_feature(*b"dlig"));
    assert_eq!(standard.len(), 2, "sans dlig, deux glyphes");
    assert_eq!(discretionary.len(), 1, "avec dlig, la ligature fi");
    assert_eq!(
        shaper
            .shape("ffi", &ShapeOptions::default().with_feature(*b"dlig"))
            .len(),
        1
    );
}

/// Crénage : « AV » et « To » doivent être **strictement** plus étroits
/// composés que la somme des avances brutes.
#[test]
fn kerning_tightens_av_and_to() {
    let mut tested = 0;
    for name in [
        "arial.ttf",
        "times.ttf",
        "calibri.ttf",
        "cambria.ttc",
        "segoeui.ttf",
        "tahoma.ttf",
    ] {
        let Some(data) = system_font(name) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        let shaper = Shaper::new(&font);
        for pair in ["AV", "To"] {
            let plain = shaped_width(&shaper.shape(pair, &ShapeOptions::plain()));
            let kerned = shaped_width(&shaper.shape(pair, &ShapeOptions::default()));
            assert!(
                kerned < plain,
                "{name} : « {pair} » devrait être créné ({kerned} vs {plain})"
            );
        }
        tested += 1;
    }
    assert!(tested > 0, "aucune police de crénage installée");
}

/// Désactiver `kern` rend exactement la largeur brute.
#[test]
fn kerning_can_be_switched_off() {
    let Some(data) = system_font("arial.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let shaper = Shaper::new(&font);
    let plain = shaped_width(&shaper.shape("AVATAR", &ShapeOptions::plain()));
    let without =
        shaped_width(&shaper.shape("AVATAR", &ShapeOptions::default().without_feature(*b"kern")));
    let with = shaped_width(&shaper.shape("AVATAR", &ShapeOptions::default()));
    assert!((without - plain).abs() < 0.5);
    assert!(with < plain);
}

/// La table `kern` ancienne d'Arial donne les mêmes paires que son `GPOS` :
/// c'est ce qui rend le repli acceptable.
#[test]
fn legacy_kern_table_agrees_with_gpos() {
    let Some(data) = system_font("arial.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let Some(kern) = font.table_bytes(*b"kern").and_then(KernTable::parse) else {
        return; // Pas de table kern : rien à comparer.
    };
    let shaper = Shaper::new(&font);
    for (left, right) in [('A', 'V'), ('T', 'o')] {
        let (a, b) = (
            font.unicode_to_gid(left).unwrap(),
            font.unicode_to_gid(right).unwrap(),
        );
        let legacy = f64::from(kern.kerning(a, b));
        assert!(legacy < 0.0, "la paire {left}{right} doit être resserrée");
        let text = format!("{left}{right}");
        let plain = shaped_width(&shaper.shape(&text, &ShapeOptions::plain()));
        let shaped = shaped_width(&shaper.shape(&text, &ShapeOptions::default()));
        assert!(
            (shaped - (plain + legacy)).abs() < 1.0,
            "GPOS et kern doivent s'accorder sur {left}{right}"
        );
    }
}

/// Formes contextuelles arabes : la même lettre isolée, initiale, médiane et
/// finale donne quatre glyphes différents.
///
/// Arial est volontairement écartée : ses formes initiale et médiane de beh
/// ont le **même contour**, et sa table `GSUB` les fait donc pointer sur le
/// même glyphe. Le test le vérifie plus bas.
#[test]
fn arabic_contextual_forms_differ() {
    let mut tested = 0;
    for name in ["times.ttf", "tahoma.ttf", "calibri.ttf", "segoeui.ttf"] {
        let Some(data) = system_font(name) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        if font.unicode_to_gid('\u{0628}').is_none() {
            continue; // Police sans arabe.
        }
        let shaper = Shaper::new(&font);
        let options = ShapeOptions::default();
        // « ب » seul, puis « بببب » : isolée, puis initiale, médiane, finale.
        let isolated = gids(&shaper.shape("\u{0628}", &options));
        let four = gids(&shaper.shape("\u{0628}\u{0628}\u{0628}\u{0628}", &options));
        assert_eq!(isolated.len(), 1, "{name}");
        assert_eq!(four.len(), 4, "{name}");
        // L'ordre rendu est visuel : finale d'abord, initiale en dernier.
        let (final_form, medial, initial) = (four[0], four[1], four[3]);
        let mut forms = vec![isolated[0], final_form, medial, initial];
        forms.sort_unstable();
        forms.dedup();
        assert_eq!(
            forms.len(),
            4,
            "{name} : les quatre formes doivent différer, obtenu {four:?} et isolée {isolated:?}"
        );
        tested += 1;
    }
    assert!(tested > 0, "aucune police arabe installée");
}

/// Arial : les formes initiale et médiane de beh ont le même contour, donc
/// la police les confond légitimement. Le test documente ce cas pour qu'on
/// ne le prenne pas pour un défaut du compositeur.
#[test]
fn arial_shares_initial_and_medial_beh() {
    let Some(data) = system_font("arial.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let (Some(initial), Some(medial)) = (
        font.unicode_to_gid('\u{FE91}'),
        font.unicode_to_gid('\u{FE92}'),
    ) else {
        return;
    };
    let (Some(a), Some(b)) = (font.glyph_path(initial), font.glyph_path(medial)) else {
        return;
    };
    assert_eq!(
        format!("{:?}", a.commands()),
        format!("{:?}", b.commands()),
        "Arial dessine la même chose pour FE91 et FE92"
    );
    let four = gids(
        &Shaper::new(&font).shape("\u{0628}\u{0628}\u{0628}\u{0628}", &ShapeOptions::default()),
    );
    assert_eq!(four.len(), 4);
    assert_eq!(four[1], four[3], "médiane et initiale partagent le glyphe");
    assert_ne!(four[0], four[1], "la finale, elle, est distincte");
}

/// Un alef coupe la liaison : « باب » donne initiale, finale, isolée.
#[test]
fn arabic_alef_breaks_the_join() {
    let Some(data) = system_font("times.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    if font.unicode_to_gid('\u{0628}').is_none() {
        return;
    }
    let shaper = Shaper::new(&font);
    let options = ShapeOptions::default();
    let word = gids(&shaper.shape("\u{0628}\u{0627}\u{0628}", &options));
    let isolated = gids(&shaper.shape("\u{0628}", &options))[0];
    assert_eq!(word.len(), 3);
    // Ordre visuel : le dernier beh (isolé) en premier, le premier beh
    // (initial) en dernier.
    assert_eq!(word[0], isolated, "le beh après l'alef reste isolé");
    assert_ne!(word[2], isolated, "le beh initial change de forme");
}

/// Marques arabes : une voyelle est posée sur sa lettre, sans avance, avec
/// un décalage non nul.
#[test]
fn arabic_marks_are_anchored() {
    let mut tested = 0;
    for name in ["arial.ttf", "times.ttf", "tahoma.ttf"] {
        let Some(data) = system_font(name) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        if font.unicode_to_gid('\u{064E}').is_none() {
            continue;
        }
        let shaper = Shaper::new(&font);
        // « بَ » : beh + fatha.
        let glyphs = shaper.shape("\u{0628}\u{064E}", &ShapeOptions::default());
        assert_eq!(glyphs.len(), 2, "{name} : la marque garde son glyphe");
        // Les deux glyphes appartiennent à la même grappe.
        assert_eq!(glyphs[0].cluster, glyphs[1].cluster, "{name}");
        let mark = glyphs
            .iter()
            .find(|g| g.advance == 0.0)
            .unwrap_or_else(|| panic!("{name} : aucune marque sans avance"));
        assert!(
            mark.offset_x != 0.0 || mark.offset_y != 0.0,
            "{name} : la marque doit être déplacée par GPOS"
        );
        tested += 1;
    }
    assert!(tested > 0, "aucune police arabe installée");
}

/// Accent combinant latin : « e » + accent aigu se compose en deux glyphes
/// d'une seule grappe, l'accent sans avance et déplacé.
#[test]
fn latin_combining_accent_is_anchored() {
    let Some(data) = system_font("calibri.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let glyphs = Shaper::new(&font).shape("e\u{0301}", &ShapeOptions::default());
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].cluster, glyphs[1].cluster);
    assert_eq!(glyphs[1].advance, 0.0, "un accent n'avance pas");
    assert!(
        glyphs[1].offset_y != 0.0,
        "l'accent est monté sur la lettre"
    );
}

/// Bidirectionnel : un mot latin suivi d'un mot hébreu donne d'abord les
/// glyphes latins dans l'ordre, puis les glyphes hébreux dans l'ordre
/// **inverse** des octets.
#[test]
fn bidi_latin_then_hebrew() {
    let Some(data) = system_font("arial.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    if font.unicode_to_gid('\u{05D0}').is_none() {
        return;
    }
    let text = "abc \u{05D0}\u{05D1}\u{05D2}";
    let glyphs = Shaper::new(&font).shape(text, &ShapeOptions::default());
    assert_eq!(glyphs.len(), 7);
    let clusters: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
    assert_eq!(&clusters[..4], &[0, 1, 2, 3], "le latin reste dans l'ordre");
    assert_eq!(
        &clusters[4..],
        &[8, 6, 4],
        "l'hébreu se lit de droite à gauche"
    );
    // Les glyphes hébreux sont bien ceux des trois lettres.
    for (i, cluster) in [(0usize, 8u32), (1, 6), (2, 4)] {
        let c = text[cluster as usize..].chars().next().unwrap();
        assert_eq!(glyphs[4 + i].gid, font.unicode_to_gid(c).unwrap());
    }
}

/// Même texte, direction forcée de gauche à droite : l'hébreu reste inversé
/// (c'est une propriété du texte, pas du paragraphe) mais l'ordre des
/// passages change.
#[test]
fn bidi_direction_can_be_forced() {
    let Some(data) = system_font("arial.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    if font.unicode_to_gid('\u{05D0}').is_none() {
        return;
    }
    let shaper = Shaper::new(&font);
    let text = "\u{05D0}\u{05D1} abc";
    let auto = shaper.shape(text, &ShapeOptions::default());
    let forced = shaper.shape(
        text,
        &ShapeOptions::default().with_direction(Direction::LeftToRight),
    );
    let first_auto = auto.first().unwrap().cluster;
    let first_forced = forced.first().unwrap().cluster;
    assert_ne!(
        first_auto, first_forced,
        "le paragraphe hébreu place le latin à gauche, pas le paragraphe latin"
    );
}

/// **Non-régression** : sans aucune fonctionnalité, la composition rend
/// exactement les glyphes de `cmap` et les avances de `hmtx`, c'est-à-dire
/// ce que fait le rendu actuel (`acrux_render::font::LoadedFont`).
#[test]
fn plain_shaping_matches_the_naive_layout() {
    for name in ["arial.ttf", "times.ttf", "calibri.ttf", "georgia.ttf"] {
        let Some(data) = system_font(name) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        let text = "Portez ce vieux whisky au juge blond qui fume ! 0123456789";
        let glyphs = Shaper::new(&font).shape(text, &ShapeOptions::plain());
        let expected: Vec<(u16, f64)> = text
            .chars()
            .map(|c| {
                let gid = font.unicode_to_gid(c).unwrap_or(0);
                (gid, f64::from(font.advance(gid).unwrap_or(0)))
            })
            .collect();
        assert_eq!(
            glyphs.len(),
            expected.len(),
            "{name} : un glyphe par lettre"
        );
        for (glyph, (gid, advance)) in glyphs.iter().zip(expected) {
            assert_eq!(glyph.gid, gid, "{name}");
            assert!(
                (glyph.advance - advance).abs() < f64::EPSILON,
                "{name} : avance inchangée"
            );
            assert_eq!(glyph.offset_x, 0.0);
            assert_eq!(glyph.offset_y, 0.0);
        }
    }
}

/// CJC vertical : la fonctionnalité `vert` change le glyphe des signes de
/// ponctuation, l'avance vient de `vmtx` et le dessin est recentré.
#[test]
fn vertical_cjk_substitutes_and_measures() {
    let mut tested = 0;
    for name in ["msgothic.ttc", "YuGothR.ttc", "meiryo.ttc"] {
        let Some(data) = system_font(name) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        let shaper = Shaper::new(&font);
        let Some(metrics) = shaper.vertical_metrics() else {
            continue;
        };
        let text = "\u{300C}\u{3042}\u{300D}"; // 「あ」
        let horizontal = shaper.shape(text, &ShapeOptions::default());
        let vertical = shaper.shape(text, &ShapeOptions::vertical());
        assert_eq!(horizontal.len(), 3, "{name}");
        assert_eq!(vertical.len(), 3, "{name}");
        assert_ne!(
            horizontal[0].gid, vertical[0].gid,
            "{name} : « 「 » a une forme verticale"
        );
        assert_eq!(
            horizontal[1].gid, vertical[1].gid,
            "{name} : « あ » ne change pas"
        );
        for glyph in &vertical {
            assert_eq!(
                glyph.advance,
                f64::from(metrics.advance(glyph.gid)),
                "{name} : l'avance verticale vient de vmtx"
            );
            assert!(glyph.offset_x < 0.0, "{name} : le glyphe est recentré");
        }
        tested += 1;
    }
    assert!(tested > 0, "aucune police CJC installée");
}

/// Le compositeur ne panique jamais, même sur des textes hostiles.
#[test]
fn shaping_never_panics() {
    let Some(data) = system_font("arial.ttf") else {
        return;
    };
    let font = TrueTypeFont::parse(&data).unwrap();
    let shaper = Shaper::new(&font);
    let samples = [
        "",
        "\u{0000}\u{FFFD}",
        "\u{202E}\u{202E}\u{202E}abc\u{202C}",
        "\u{2066}\u{2067}\u{2068}\u{2069}",
        "\u{0628}\u{200D}\u{200C}\u{0640}\u{064B}\u{0651}",
        "\u{1F1EB}\u{1F1F7}\u{1F468}\u{200D}\u{1F469}",
        "e\u{0301}\u{0327}\u{0300}\u{0651}\u{064E}",
        "\u{05D0}1\u{0660}a\u{202B}\u{05D1}",
    ];
    for text in samples {
        for options in [
            ShapeOptions::default(),
            ShapeOptions::plain(),
            ShapeOptions::vertical(),
            ShapeOptions::default().with_direction(Direction::RightToLeft),
        ] {
            let glyphs = shaper.shape(text, &options);
            for glyph in &glyphs {
                assert!(
                    (glyph.cluster as usize) <= text.len(),
                    "grappe hors de la chaîne pour {text:?}"
                );
            }
        }
    }
    // Une chaîne longue et répétitive ne fait pas exploser la mémoire.
    let long: String = std::iter::repeat_n("fi\u{0628}\u{064E}a", 2000).collect();
    assert!(!shaper.shape(&long, &ShapeOptions::default()).is_empty());
}

/// Une police sans `GSUB` ni `GPOS` compose quand même, sans rien changer.
#[test]
fn font_without_layout_tables_still_shapes() {
    let mut tested = 0;
    for name in ["marlett.ttf", "wingding.ttf", "symbol.ttf"] {
        let Some(data) = system_font(name) else {
            continue;
        };
        let Ok(font) = TrueTypeFont::parse(&data) else {
            continue;
        };
        let shaper = Shaper::new(&font);
        if shaper.has_gsub() || shaper.has_gpos() {
            continue;
        }
        let glyphs = shaper.shape("abc", &ShapeOptions::default());
        assert_eq!(glyphs.len(), 3, "{name}");
        tested += 1;
    }
    let _ = tested; // Aucune de ces polices n'est obligatoire.
}
