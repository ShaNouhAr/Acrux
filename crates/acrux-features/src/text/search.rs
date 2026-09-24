//! Recherche dans le texte d'une page : casse, mot entier, et occurrences qui
//! passent d'une ligne à l'autre au sein d'un paragraphe.
//!
//! C'est l'unique moteur de recherche d'Acrux : les surlignages de la carte de
//! recherche, les plages que « Remplacer » réécrit (`edit_text::find_ranges`)
//! et la commande `acr find` sortent du même parcours, dans le même ordre.
//! Avant lui, deux moteurs comptaient chacun à sa façon ; l'interface devait
//! vérifier qu'ils tombaient d'accord avant d'oser remplacer quoi que ce soit.
//!
//! Le texte d'un paragraphe est parcouru d'un seul tenant, comme on le lit :
//!
//! - une fin de ligne vaut une espace — « de première / ligne » se trouve en
//!   tapant « de première ligne » ;
//! - une césure se referme — « docu- / mentation » se trouve en tapant
//!   « documentation », avec la même règle que le texte des paragraphes
//!   ([`super::layout::is_hyphen_break`]) ; un tiret gardé (« Jean- /
//!   Pierre ») se lit collé à la suite, « Jean-Pierre » ;
//! - d'un paragraphe à l'autre, d'une cellule de tableau à l'autre, rien ne
//!   se joint : chaque groupe de lignes est cherché à part.
//!
//! Les caractères sont comparés après un **repli un pour un** : tous les
//! blancs (espace insécable, espace fine…) valent une espace, l'apostrophe
//! typographique vaut l'apostrophe droite (« l’article » se trouve en tapant
//! « l'article »), et, sauf si l'on respecte la casse, une lettre vaut sa
//! minuscule. Un pour un, jamais plus : « İ » devient « i », pas « i̇ » en deux
//! caractères, sans quoi les boîtes des caractères suivants seraient
//! décalées. Les ligatures (« ﬁ ») sont en revanche dépliées **avant** la
//! comparaison, chaque lettre gardant le glyphe qui la porte : « fin » se
//! trouve même quand le PDF a écrit « ﬁn » en un glyphe.
//!
//! Les occurrences ne se chevauchent pas : « aa » se trouve deux fois dans
//! « aaaa », pas trois. Ce que l'on remplace ensuite ne peut donc pas se
//! recouvrir.

use acrux_core::Rect;

use super::layout::is_hyphen_break;
use super::PageText;

/// Options d'une recherche.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    /// Respecter la casse : « Ici » ne trouve plus « ici ».
    pub match_case: bool,
    /// Mot entier : « art » ne trouve plus « partie » ni « artiste », mais
    /// toujours « art, » et « l'art ».
    pub whole_word: bool,
}

/// Morceau d'une occurrence, sur une ligne.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchPiece {
    /// Indice de la ligne dans `PageText::lines`.
    pub line: usize,
    /// Premier glyphe couvert. Les glyphes sont comptés comme dans
    /// `edit_text::TextRange` : de mot en mot, sans les espaces qui les
    /// séparent.
    pub start: usize,
    /// Glyphe qui suit le dernier couvert.
    pub end: usize,
    /// Boîte du morceau, en espace utilisateur PDF.
    pub rect: Rect,
}

/// Une occurrence : un morceau par ligne traversée, dans l'ordre de lecture.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMatch {
    /// Les morceaux ; jamais vide.
    pub pieces: Vec<MatchPiece>,
    /// L'occurrence commence au début d'un glyphe et finit à la fin d'un
    /// glyphe. Faux quand elle coupe un glyphe qui porte plusieurs lettres
    /// — « in » dans une ligature « ﬁn », « icher » dans « aﬃcher » : la
    /// surligner est juste, mais réécrire ses glyphes emporterait les
    /// lettres voisines (« aICHER »).
    pub whole_glyphs: bool,
}

impl TextMatch {
    /// Le morceau unique d'une occurrence qui tient sur une ligne ; `None`
    /// pour une occurrence à cheval sur deux lignes, que l'édition de texte
    /// (une ligne à la fois) ne sait pas réécrire.
    #[must_use]
    pub fn single_line(&self) -> Option<&MatchPiece> {
        match self.pieces.as_slice() {
            [piece] => Some(piece),
            _ => None,
        }
    }

    /// Le morceau que l'édition de texte peut réécrire tel quel : une
    /// occurrence sur une seule ligne, qui ne coupe aucun glyphe. C'est
    /// l'occurrence que « Remplacer » accepte.
    #[must_use]
    pub fn editable(&self) -> Option<&MatchPiece> {
        self.single_line().filter(|_| self.whole_glyphs)
    }

    /// Boîte englobant tous les morceaux.
    #[must_use]
    pub fn bbox(&self) -> Rect {
        self.pieces.iter().skip(1).fold(
            self.pieces.first().map_or_else(Rect::default, |p| p.rect),
            |r, p| r.union(&p.rect),
        )
    }
}

/// Un caractère du texte parcouru.
struct Unit {
    /// Caractère replié, celui que l'on compare.
    folded: char,
    /// Caractère d'origine (ligature dépliée), pour les bornes de mot.
    orig: char,
    /// Ligne qui le porte.
    line: usize,
    /// Glyphe qui le porte ; `None` pour une espace entre deux mots ou entre
    /// deux lignes, qui n'a pas de glyphe.
    glyph: Option<usize>,
    /// Boîte du glyphe (sans objet sans glyphe).
    rect: Rect,
}

/// Occurrences de `needle` dans la page, dans l'ordre de lecture.
///
/// Les blancs de la requête sont réduits à une espace et ceux du bout
/// ignorés ; une requête vide ne trouve rien.
#[must_use]
pub fn find_matches(text: &PageText, needle: &str, options: SearchOptions) -> Vec<TextMatch> {
    let pattern = fold_needle(needle, options.match_case);
    if pattern.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut units = Vec::new();
    for group in line_groups(text) {
        units.clear();
        push_group(text, &group, options.match_case, &mut units);
        find_in(&units, &pattern, options.whole_word, &mut out);
    }
    out
}

/// Les groupes de lignes cherchés d'un seul tenant : un par paragraphe, dans
/// l'ordre de lecture ; chaque ligne qu'aucun paragraphe ne couvre (cellule
/// de tableau, page dont la mise en page n'a pas été analysée) fait groupe à
/// elle seule.
fn line_groups(text: &PageText) -> Vec<Vec<usize>> {
    let n = text.lines.len();
    let mut covered = vec![false; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for paragraph in text.blocks.iter().flat_map(|b| b.paragraphs.iter()) {
        let mut lines = Vec::with_capacity(paragraph.lines.len());
        for &i in &paragraph.lines {
            if i < n && !covered[i] {
                covered[i] = true;
                lines.push(i);
            }
        }
        if !lines.is_empty() {
            groups.push(lines);
        }
    }
    groups.extend((0..n).filter(|&i| !covered[i]).map(|i| vec![i]));
    // `lines` suit l'ordre de lecture : le premier indice d'un groupe le
    // place parmi les autres. Les groupes sont disjoints, l'ordre est total.
    groups.sort_by_key(|g| g.first().copied().unwrap_or(usize::MAX));
    groups
}

/// Pousse les caractères d'un groupe de lignes, joints comme on les lit.
fn push_group(text: &PageText, group: &[usize], match_case: bool, units: &mut Vec<Unit>) {
    for (k, &index) in group.iter().enumerate() {
        let line = &text.lines[index];
        if k > 0 {
            join(units, line, index);
        }
        let mut glyph = 0;
        for (w, word) in line.words.iter().enumerate() {
            if w > 0 {
                units.push(Unit {
                    folded: ' ',
                    orig: ' ',
                    line: index,
                    glyph: None,
                    rect: Rect::default(),
                });
            }
            for g in &word.glyphs {
                for c in g.text.chars() {
                    for_each_folded(c, match_case, |folded, orig| {
                        units.push(Unit {
                            folded,
                            orig,
                            line: index,
                            glyph: Some(glyph),
                            rect: g.bbox,
                        });
                    });
                }
                glyph += 1;
            }
        }
    }
}

/// Jonction avec la ligne suivante d'un même paragraphe : une césure se
/// referme (le tiret quitte le texte cherché), un tiret collé à un mot
/// (« Jean- / Pierre ») se lit collé à la suite, tout le reste vaut une
/// espace.
fn join(units: &mut Vec<Unit>, next: &super::Line, index: usize) {
    let first = next.words.first().and_then(|w| w.text.chars().next());
    let Some(last) = units.last().filter(|u| u.glyph.is_some()) else {
        return;
    };
    if is_hyphen_break(last.orig, first) {
        units.pop();
        return;
    }
    let len = units.len();
    let attached = matches!(last.orig, '-' | '\u{2010}' | '\u{2011}')
        && len >= 2
        && units[len - 2].glyph.is_some()
        && units[len - 2].line == last.line;
    if !attached {
        units.push(Unit {
            folded: ' ',
            orig: ' ',
            line: index,
            glyph: None,
            rect: Rect::default(),
        });
    }
}

/// Cherche le motif dans un groupe, sans chevauchement.
fn find_in(units: &[Unit], pattern: &[char], whole_word: bool, out: &mut Vec<TextMatch>) {
    let m = pattern.len();
    if m == 0 || units.len() < m {
        return;
    }
    // La borne de mot ne se vérifie que du côté où la requête commence ou
    // finit par une lettre : « -12 » en mot entier trouve « x-12 ».
    let check_start = whole_word && pattern[0].is_alphanumeric();
    let check_end = whole_word && pattern[m - 1].is_alphanumeric();
    let mut i = 0;
    while i + m <= units.len() {
        let same = units[i..i + m]
            .iter()
            .zip(pattern)
            .all(|(u, p)| u.folded == *p);
        let bounded = (!check_start || i == 0 || !units[i - 1].orig.is_alphanumeric())
            && (!check_end || i + m == units.len() || !units[i + m].orig.is_alphanumeric());
        if same && bounded {
            // Le glyphe d'avant et celui d'après doivent être d'autres
            // glyphes que ceux du bord de l'occurrence : sinon elle en coupe
            // un en deux.
            let same_glyph =
                |a: &Unit, b: &Unit| a.glyph.is_some() && (a.line, a.glyph) == (b.line, b.glyph);
            let whole = (i == 0 || !same_glyph(&units[i - 1], &units[i]))
                && (i + m == units.len() || !same_glyph(&units[i + m - 1], &units[i + m]));
            if let Some(found) = to_match(&units[i..i + m], whole) {
                out.push(found);
            }
            i += m;
        } else {
            i += 1;
        }
    }
}

/// Morceaux d'une occurrence : ses glyphes regroupés ligne par ligne.
fn to_match(units: &[Unit], whole_glyphs: bool) -> Option<TextMatch> {
    let mut pieces: Vec<MatchPiece> = Vec::new();
    for u in units {
        let Some(g) = u.glyph else { continue };
        match pieces.last_mut() {
            Some(p) if p.line == u.line => {
                p.start = p.start.min(g);
                p.end = p.end.max(g + 1);
                p.rect = p.rect.union(&u.rect);
            }
            _ => pieces.push(MatchPiece {
                line: u.line,
                start: g,
                end: g + 1,
                rect: u.rect,
            }),
        }
    }
    (!pieces.is_empty()).then_some(TextMatch {
        pieces,
        whole_glyphs,
    })
}

/// La requête repliée : blancs réduits à une espace, ceux du bout ôtés.
fn fold_needle(needle: &str, match_case: bool) -> Vec<char> {
    let mut out = Vec::new();
    for word in needle.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        for c in word.chars() {
            for_each_folded(c, match_case, |folded, _| out.push(folded));
        }
    }
    out
}

/// Déplie une ligature, puis rend chaque lettre repliée avec sa forme
/// d'origine.
fn for_each_folded(c: char, match_case: bool, mut f: impl FnMut(char, char)) {
    let expanded = match c {
        '\u{fb00}' => "ff",
        '\u{fb01}' => "fi",
        '\u{fb02}' => "fl",
        '\u{fb03}' => "ffi",
        '\u{fb04}' => "ffl",
        '\u{fb05}' | '\u{fb06}' => "st",
        _ => {
            f(fold(c, match_case), c);
            return;
        }
    };
    for e in expanded.chars() {
        f(fold(e, match_case), e);
    }
}

/// Repli d'un caractère : toujours un caractère pour un.
fn fold(c: char, match_case: bool) -> char {
    let c = match c {
        c if c.is_whitespace() => ' ',
        '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
        '\u{201c}' | '\u{201d}' => '"',
        '\u{2010}' | '\u{2011}' => '-',
        c => c,
    };
    if match_case {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::super::test_support::page;
    use super::*;

    fn count(text: &PageText, needle: &str, options: SearchOptions) -> usize {
        find_matches(text, needle, options).len()
    }

    const CASE: SearchOptions = SearchOptions {
        match_case: true,
        whole_word: false,
    };
    const WORD: SearchOptions = SearchOptions {
        match_case: false,
        whole_word: true,
    };

    /// La page du test des paragraphes : une césure « docu- / mentation »,
    /// puis un second paragraphe dont une expression passe à la ligne.
    fn hyphen_page() -> PageText {
        let mut p = page(&[
            (
                50.0,
                700.0,
                12.0,
                "Premiere ligne du premier paragraphe docu-",
            ),
            (50.0, 686.0, 12.0, "mentation suite et fin du paragraphe."),
            (
                70.0,
                660.0,
                12.0,
                "Second paragraphe avec retrait de premiere",
            ),
            (
                50.0,
                646.0,
                12.0,
                "ligne qui continue ici sans s'arreter du tout.",
            ),
            (50.0, 632.0, 12.0, "Et encore une ligne."),
        ]);
        p.analyze();
        p
    }

    #[test]
    fn casse_ignoree_par_defaut_et_respectee_sur_demande() {
        let p = page(&[(50.0, 700.0, 12.0, "Ici ICI ici")]);
        let default = SearchOptions::default();
        assert_eq!(count(&p, "ici", default), 3);
        assert_eq!(count(&p, "ICI", default), 3);
        assert_eq!(count(&p, "Ici", CASE), 1);
        assert_eq!(count(&p, "ici", CASE), 1);
        assert_eq!(count(&p, "iCi", CASE), 0);
    }

    #[test]
    fn mot_entier() {
        let p = page(&[(50.0, 700.0, 12.0, "art, (art) partie artiste l'art")]);
        assert_eq!(count(&p, "art", SearchOptions::default()), 5);
        assert_eq!(count(&p, "art", WORD), 3);
        assert_eq!(count(&p, "ART", WORD), 3);
        assert_eq!(count(&p, "partie", WORD), 1);
        assert_eq!(count(&p, "arti", WORD), 0);
        // La borne ne compte que du côté d'une lettre : « (art » commence par
        // une parenthèse, elle se trouve même collée à ce qui précède.
        assert_eq!(count(&p, "(art", WORD), 1);
    }

    #[test]
    fn cesure_dans_un_paragraphe() {
        let p = hyphen_page();
        let found = find_matches(&p, "documentation", SearchOptions::default());
        assert_eq!(found.len(), 1, "{found:?}");
        let m = &found[0];
        assert_eq!(m.pieces.len(), 2);
        assert!(m.single_line().is_none());
        // « Premiere ligne du premier paragraphe docu- » : 32 glyphes avant
        // « docu », le tiret (36) n'en fait pas partie.
        assert_eq!((m.pieces[0].start, m.pieces[0].end), (32, 36));
        assert_eq!((m.pieces[1].start, m.pieces[1].end), (0, 9));
        assert!(p.lines[m.pieces[0].line].text().ends_with("docu-"));
        assert!(p.lines[m.pieces[1].line].text().starts_with("mentation"));
        // Les deux moitiés, chacune sur sa ligne, restent trouvables.
        assert_eq!(count(&p, "docu", SearchOptions::default()), 1);
        assert_eq!(count(&p, "docu-", SearchOptions::default()), 0);
        assert_eq!(count(&p, "documentation", WORD), 1);
        assert_eq!(count(&p, "mentation", WORD), 0);
    }

    #[test]
    fn fin_de_ligne_sans_cesure() {
        let p = hyphen_page();
        let found = find_matches(&p, "de premiere ligne", SearchOptions::default());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].pieces.len(), 2);
        let bbox = found[0].bbox();
        assert!(bbox.height() > 20.0, "deux lignes : {bbox:?}");
        // « premiere ligne » se trouve aussi dans le premier paragraphe, sur
        // une seule ligne : une occurrence éditable et une à cheval.
        let both = find_matches(&p, "premiere ligne", SearchOptions::default());
        assert_eq!(both.len(), 2);
        assert!(both[0].single_line().is_some());
        assert!(both[1].single_line().is_none());
    }

    #[test]
    fn pas_d_un_paragraphe_a_l_autre() {
        let p = hyphen_page();
        assert_eq!(count(&p, "paragraphe. Second", SearchOptions::default()), 0);
        assert_eq!(count(&p, "paragraphe.", SearchOptions::default()), 1);
        assert_eq!(count(&p, "paragraphe", SearchOptions::default()), 3);
    }

    #[test]
    fn tiret_suivi_d_une_majuscule_reste_un_tiret() {
        let mut p = page(&[
            (50.0, 700.0, 12.0, "Nous avons salue longuement Jean-"),
            (50.0, 686.0, 12.0, "Pierre et tous les autres invites."),
        ]);
        p.analyze();
        assert_eq!(count(&p, "JeanPierre", SearchOptions::default()), 0);
        let found = find_matches(&p, "Jean-Pierre", SearchOptions::default());
        assert_eq!(found.len(), 1);
        // Le tiret reste dans le premier morceau.
        let first = found[0].pieces[0];
        assert_eq!(first.end - first.start, 5);
    }

    #[test]
    fn sans_chevauchement() {
        let p = page(&[(50.0, 700.0, 12.0, "aaaa")]);
        assert_eq!(count(&p, "aa", SearchOptions::default()), 2);
        assert_eq!(count(&p, "a", SearchOptions::default()), 4);
    }

    #[test]
    fn blancs_et_apostrophes() {
        let p = page(&[(
            50.0,
            700.0,
            12.0,
            "voir l\u{2019}article\u{a0}premier du code",
        )]);
        let found = find_matches(&p, "  l'article   premier ", SearchOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(count(&p, "L'ARTICLE PREMIER", SearchOptions::default()), 1);
        assert_eq!(count(&p, "\t", SearchOptions::default()), 0, "requête vide");
    }

    #[test]
    fn ligatures_depliees() {
        let p = page(&[(50.0, 700.0, 12.0, "la \u{fb01}n du \u{fb02}euve")]);
        let found = find_matches(&p, "fin", WORD);
        assert_eq!(found.len(), 1);
        // « ﬁ » est un seul glyphe : l'occurrence couvre deux glyphes.
        assert_eq!(found[0].pieces[0].end - found[0].pieces[0].start, 2);
        assert_eq!(count(&p, "fleuve", SearchOptions::default()), 1);
        assert_eq!(count(&p, "in", WORD), 0);
        assert!(found[0].editable().is_some(), "la ligature entière");
        // « in » coupe la ligature : on le trouve, on ne le réécrit pas —
        // remplacer ses glyphes emporterait le « f ».
        let cut = find_matches(&p, "in", SearchOptions::default());
        assert_eq!(cut.len(), 1);
        assert!(!cut[0].whole_glyphs);
        assert!(cut[0].single_line().is_some());
        assert!(cut[0].editable().is_none());
        let cut = find_matches(&p, "f", SearchOptions::default());
        assert_eq!(cut.len(), 2);
        assert!(cut.iter().all(|m| !m.whole_glyphs));
    }

    #[test]
    fn repli_un_pour_un() {
        let p = page(&[(50.0, 700.0, 12.0, "\u{130}stanbul ici \u{130}")]);
        let found = find_matches(&p, "ici", SearchOptions::default());
        assert_eq!(found.len(), 1);
        let piece = found[0].pieces[0];
        // « İstanbul » : 8 glyphes, puis « ici ».
        assert_eq!((piece.start, piece.end), (8, 11));
        let glyphs: Vec<_> = p.lines[0]
            .words
            .iter()
            .flat_map(|w| w.glyphs.iter())
            .collect();
        assert!((piece.rect.x0 - glyphs[8].bbox.x0).abs() < 1e-9);
        assert!((piece.rect.x1 - glyphs[10].bbox.x1).abs() < 1e-9);
        // La dernière lettre de la ligne se trouve sans sortir des bornes.
        assert_eq!(count(&p, "i", SearchOptions::default()), 4);
        assert_eq!(count(&p, "istanbul", SearchOptions::default()), 1);
    }

    #[test]
    fn sans_blocs_une_ligne_par_groupe() {
        // Sans `analyze`, pas de paragraphes : chaque ligne est cherchée
        // seule, comme avant la jonction des lignes.
        let p = page(&[
            (50.0, 700.0, 12.0, "tout a la fin de"),
            (50.0, 686.0, 12.0, "ligne suivante de"),
        ]);
        assert!(p.blocks.is_empty());
        assert_eq!(count(&p, "de ligne", SearchOptions::default()), 0);
        let found = find_matches(&p, "de", SearchOptions::default());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].pieces[0].line, 0);
        assert_eq!(found[1].pieces[0].line, 1);
    }

    #[test]
    fn ordre_des_occurrences() {
        let mut p = page(&[
            (50.0, 700.0, 10.0, "Gauche alpha un deux trois"),
            (300.0, 700.0, 10.0, "Droite alpha un deux trois"),
            (50.0, 688.0, 10.0, "gauche alpha quatre cinq six"),
            (300.0, 688.0, 10.0, "droite alpha quatre cinq six"),
            (50.0, 676.0, 10.0, "gauche alpha sept huit."),
            (300.0, 676.0, 10.0, "droite alpha sept huit."),
        ]);
        p.analyze();
        let found = find_matches(&p, "alpha", SearchOptions::default());
        assert_eq!(found.len(), 6);
        let xs: Vec<bool> = found.iter().map(|m| m.bbox().x0 < 200.0).collect();
        assert_eq!(
            xs,
            vec![true, true, true, false, false, false],
            "colonne de gauche d'abord"
        );
        assert!(found[0].bbox().y0 > found[1].bbox().y0);
    }
}
