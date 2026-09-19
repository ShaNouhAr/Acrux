//! Algorithme bidirectionnel Unicode (UAX #9), pour un paragraphe.
//!
//! Le texte est stocké en **ordre logique** (l'ordre de frappe et de lecture)
//! mais dessiné en **ordre visuel** (de gauche à droite sur la page). Quand
//! une phrase mélange du latin et de l'hébreu ou de l'arabe, il faut décider
//! quels morceaux s'inversent : c'est tout l'objet de cet algorithme.
//!
//! ## Règles implémentées
//!
//! - **P2, P3** : direction du paragraphe déduite du premier caractère fort,
//!   les isolats étant sautés ([`paragraph_level`]).
//! - **X1 à X8** : niveaux explicites, avec la pile de directions, les
//!   enchâssements (`LRE`/`RLE`), les forçages (`LRO`/`RLO`), les dépilages
//!   (`PDF`) et les isolats (`LRI`/`RLI`/`FSI`/`PDI`), y compris les
//!   dépassements de profondeur (limite 125).
//! - **X9** : les caractères de formatage et les neutres bornés sont retirés
//!   du calcul (ils gardent, en sortie, le niveau de leur voisin de gauche).
//! - **X10** : découpe en suites de passages isolants, avec `sos` et `eos`.
//! - **W1 à W7** : marques, chiffres européens et arabes, séparateurs.
//! - **N1, N2** : neutres entre deux directions.
//! - **I1, I2** : niveaux implicites.
//! - **L1, L2** : remise à niveau des blancs de fin et inversion des
//!   passages ([`reorder_visual`]).
//!
//! ## Ce que nous ne faisons pas
//!
//! - **N0** (paires de parenthèses appariées, `BidiBrackets.txt`) : une
//!   parenthèse isolée dans un passage de droite à gauche est traitée par N1
//!   et N2 comme un neutre ordinaire. Le miroir des parenthèses (`L4`) est
//!   laissé au rendu, qui dispose de la table de miroir de la police.
//! - Le découpage en paragraphes (**P1**) : l'appelant passe un paragraphe.
//! - Les niveaux de ligne (**L1** suppose ici que la ligne est le
//!   paragraphe entier).

use crate::unicode::bidi_class::{bidi_class, BidiClass};

/// Profondeur maximale d'enchâssement (UAX #9, `max_depth`).
const MAX_DEPTH: u8 = 125;

/// Direction de base demandée pour le paragraphe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaseDirection {
    /// Déduite du premier caractère fort (règles P2 et P3).
    #[default]
    Auto,
    /// Forcée de gauche à droite (niveau 0).
    LeftToRight,
    /// Forcée de droite à gauche (niveau 1).
    RightToLeft,
}

/// Passage de texte homogène en direction, repéré par des indices d'octets
/// dans la chaîne d'origine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Indice d'octet du premier caractère (inclus).
    pub start: usize,
    /// Indice d'octet de fin (exclu).
    pub end: usize,
    /// Niveau bidirectionnel : pair = gauche à droite, impair = droite à
    /// gauche.
    pub level: u8,
}

impl Run {
    /// Vrai si le passage se dessine de droite à gauche.
    #[must_use]
    pub const fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// Résultat complet de l'algorithme sur un paragraphe.
#[derive(Debug, Clone)]
pub struct BidiInfo {
    /// Niveau de base du paragraphe (0 ou 1).
    pub paragraph_level: u8,
    /// Niveau résolu de chaque **caractère** (et non de chaque octet).
    pub levels: Vec<u8>,
    /// Indice d'octet de chaque caractère, plus la longueur totale.
    pub offsets: Vec<usize>,
}

impl BidiInfo {
    /// Vrai si le paragraphe contient au moins un passage de droite à
    /// gauche : le rendu peut alors sauter le réordonnancement.
    #[must_use]
    pub fn has_rtl(&self) -> bool {
        self.paragraph_level % 2 == 1 || self.levels.iter().any(|l| l % 2 == 1)
    }
}

/// État d'une entrée de la pile de directions (X1).
#[derive(Debug, Clone, Copy)]
struct StackEntry {
    level: u8,
    /// Classe imposée par un forçage `LRO`/`RLO`, sinon `None`.
    override_class: Option<BidiClass>,
    isolate: bool,
}

/// Niveau de base d'un paragraphe (règles P2 et P3).
///
/// Le premier caractère fort (`L`, `R` ou `AL`) décide ; les portions
/// encadrées par un isolat sont sautées. Sans caractère fort, le niveau est
/// 0.
#[must_use]
pub fn paragraph_level(text: &str, base: BaseDirection) -> u8 {
    match base {
        BaseDirection::LeftToRight => return 0,
        BaseDirection::RightToLeft => return 1,
        BaseDirection::Auto => {}
    }
    let classes: Vec<BidiClass> = text.chars().map(bidi_class).collect();
    first_strong_level(&classes, 0, classes.len())
}

/// Niveau déduit du premier caractère fort de `classes[start..end]`.
fn first_strong_level(classes: &[BidiClass], start: usize, end: usize) -> u8 {
    let mut depth = 0u32;
    let mut i = start;
    while i < end {
        let c = classes[i];
        if c.is_isolate_initiator() {
            depth += 1;
        } else if c == BidiClass::Pdi {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            match c {
                BidiClass::L => return 0,
                BidiClass::R | BidiClass::Al => return 1,
                _ => {}
            }
        }
        i += 1;
    }
    0
}

/// Indice du `PDI` correspondant à chaque initiateur d'isolat (BD9).
///
/// Une position qui n'est pas un initiateur, ou dont l'isolat n'est jamais
/// refermé, reçoit `classes.len()`.
fn matching_pdi(classes: &[BidiClass]) -> Vec<usize> {
    let n = classes.len();
    let mut matches = vec![n; n];
    let mut open: Vec<usize> = Vec::new();
    for (i, c) in classes.iter().enumerate() {
        if c.is_isolate_initiator() {
            open.push(i);
        } else if *c == BidiClass::Pdi {
            if let Some(start) = open.pop() {
                matches[start] = i;
            }
        }
    }
    matches
}

/// Compteurs de dépassement des règles X (UAX #9, X1).
#[derive(Debug, Default)]
struct Overflow {
    isolates: u32,
    embeddings: u32,
    valid_isolates: u32,
}

/// Contexte mutable des règles X1 à X8.
struct ExplicitPass<'a> {
    classes: &'a mut [BidiClass],
    levels: &'a mut [u8],
    stack: Vec<StackEntry>,
    over: Overflow,
    paragraph_level: u8,
}

impl ExplicitPass<'_> {
    fn top(&self) -> StackEntry {
        *self.stack.last().unwrap_or(&StackEntry {
            level: self.paragraph_level,
            override_class: None,
            isolate: false,
        })
    }

    /// Prochain niveau pair (`rtl` faux) ou impair (`rtl` vrai) strictement
    /// supérieur au sommet de pile ; `None` en cas de dépassement.
    fn next_level(&self, rtl: bool) -> Option<u8> {
        let top = self.top().level;
        let candidate = if rtl { (top + 1) | 1 } else { (top + 2) & !1 };
        (candidate <= MAX_DEPTH && self.over.isolates == 0 && self.over.embeddings == 0)
            .then_some(candidate)
    }

    /// X2 à X5 : enchâssement ou forçage.
    fn push_embedding(&mut self, rtl: bool, override_class: Option<BidiClass>) {
        if let Some(level) = self.next_level(rtl) {
            self.stack.push(StackEntry {
                level,
                override_class,
                isolate: false,
            });
        } else if self.over.isolates == 0 {
            self.over.embeddings += 1;
        }
    }

    /// X5a à X5c : initiateur d'isolat. Le caractère lui-même garde le
    /// niveau courant et subit le forçage éventuel.
    fn push_isolate(&mut self, index: usize, rtl: bool) {
        let top = self.top();
        self.levels[index] = top.level;
        if let Some(forced) = top.override_class {
            self.classes[index] = forced;
        }
        if let Some(level) = self.next_level(rtl) {
            self.over.valid_isolates += 1;
            self.stack.push(StackEntry {
                level,
                override_class: None,
                isolate: true,
            });
        } else {
            self.over.isolates += 1;
        }
    }

    /// X6a : fin d'isolat.
    fn pop_isolate(&mut self, index: usize) {
        if self.over.isolates > 0 {
            self.over.isolates -= 1;
        } else if self.over.valid_isolates > 0 {
            self.over.embeddings = 0;
            while !self.top().isolate && self.stack.len() > 1 {
                self.stack.pop();
            }
            self.stack.pop();
            self.over.valid_isolates -= 1;
        }
        let top = self.top();
        self.levels[index] = top.level;
        if let Some(forced) = top.override_class {
            self.classes[index] = forced;
        }
    }

    /// X7 : dépilage d'un enchâssement ou d'un forçage.
    fn pop_embedding(&mut self, index: usize) {
        if self.over.isolates > 0 {
            // Rien : le dépilage appartient à un isolat en dépassement.
        } else if self.over.embeddings > 0 {
            self.over.embeddings -= 1;
        } else if !self.top().isolate && self.stack.len() > 1 {
            self.stack.pop();
        }
        self.levels[index] = self.top().level;
    }

    /// X6 : caractère ordinaire.
    fn plain(&mut self, index: usize) {
        let top = self.top();
        self.levels[index] = top.level;
        if let Some(forced) = top.override_class {
            self.classes[index] = forced;
        }
    }
}

/// Applique les règles X1 à X8 et renvoie les niveaux explicites.
fn explicit_levels(
    classes: &mut [BidiClass],
    original: &[BidiClass],
    paragraph_level: u8,
) -> Vec<u8> {
    let n = classes.len();
    let mut levels = vec![paragraph_level; n];
    let pdi = matching_pdi(original);
    let mut pass = ExplicitPass {
        classes,
        levels: &mut levels,
        stack: vec![StackEntry {
            level: paragraph_level,
            override_class: None,
            isolate: false,
        }],
        over: Overflow::default(),
        paragraph_level,
    };
    for i in 0..n {
        match original[i] {
            BidiClass::Rle => {
                pass.levels[i] = pass.top().level;
                pass.push_embedding(true, None);
            }
            BidiClass::Lre => {
                pass.levels[i] = pass.top().level;
                pass.push_embedding(false, None);
            }
            BidiClass::Rlo => {
                pass.levels[i] = pass.top().level;
                pass.push_embedding(true, Some(BidiClass::R));
            }
            BidiClass::Lro => {
                pass.levels[i] = pass.top().level;
                pass.push_embedding(false, Some(BidiClass::L));
            }
            BidiClass::Rli => pass.push_isolate(i, true),
            BidiClass::Lri => pass.push_isolate(i, false),
            BidiClass::Fsi => {
                // X5c : la direction vient du premier caractère fort du
                // contenu de l'isolat.
                let end = pdi[i].min(n);
                let rtl = first_strong_level(original, i + 1, end) == 1;
                pass.push_isolate(i, rtl);
            }
            BidiClass::Pdi => pass.pop_isolate(i),
            BidiClass::Pdf => pass.pop_embedding(i),
            BidiClass::B => {
                pass.levels[i] = paragraph_level;
            }
            _ => pass.plain(i),
        }
    }
    levels
}

/// Une suite de passages isolants (X10) : les indices concernés, dans
/// l'ordre logique, plus les directions d'entrée et de sortie.
struct Sequence {
    indices: Vec<usize>,
    level: u8,
    sos: BidiClass,
    eos: BidiClass,
}

/// Direction (`L` ou `R`) associée au plus grand des deux niveaux.
fn direction_of(level_a: u8, level_b: u8) -> BidiClass {
    if level_a.max(level_b) % 2 == 1 {
        BidiClass::R
    } else {
        BidiClass::L
    }
}

/// Passages de même niveau, en ignorant les caractères retirés par X9.
fn level_runs(levels: &[u8], removed: &[bool]) -> Vec<Vec<usize>> {
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_level = None;
    for (i, &level) in levels.iter().enumerate() {
        if removed[i] {
            continue;
        }
        if current_level != Some(level) {
            if !current.is_empty() {
                runs.push(std::mem::take(&mut current));
            }
            current_level = Some(level);
        }
        current.push(i);
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// Construit les suites de passages isolants (X10).
fn isolating_sequences(
    original: &[BidiClass],
    levels: &[u8],
    removed: &[bool],
    paragraph_level: u8,
) -> Vec<Sequence> {
    let runs = level_runs(levels, removed);
    let pdi = matching_pdi(original);
    // Pour chaque passage, l'indice du passage qui commence par son PDI.
    let mut run_of_index = vec![usize::MAX; original.len()];
    for (r, run) in runs.iter().enumerate() {
        if let Some(&first) = run.first() {
            run_of_index[first] = r;
        }
    }
    let mut used = vec![false; runs.len()];
    let mut sequences = Vec::new();
    for r in 0..runs.len() {
        if used[r] {
            continue;
        }
        // Un passage commençant par un PDI apparié est rattaché à son
        // initiateur, pas au début d'une nouvelle suite.
        if let Some(&first) = runs[r].first() {
            if original[first] == BidiClass::Pdi && has_matching_initiator(original, &pdi, first) {
                continue;
            }
        }
        let mut indices: Vec<usize> = Vec::new();
        let mut current = r;
        loop {
            used[current] = true;
            indices.extend_from_slice(&runs[current]);
            let Some(&last) = runs[current].last() else {
                break;
            };
            if !original[last].is_isolate_initiator() {
                break;
            }
            let target = pdi[last];
            if target >= original.len() {
                break;
            }
            let next = run_of_index[target];
            if next == usize::MAX || used[next] {
                break;
            }
            current = next;
        }
        sequences.push(build_sequence(
            indices,
            original,
            levels,
            removed,
            paragraph_level,
        ));
    }
    sequences
}

/// Vrai si le `PDI` en `index` referme un isolat ouvert.
fn has_matching_initiator(original: &[BidiClass], pdi: &[usize], index: usize) -> bool {
    original
        .iter()
        .enumerate()
        .any(|(i, c)| c.is_isolate_initiator() && pdi[i] == index)
}

/// Calcule `sos` et `eos` d'une suite (X10).
fn build_sequence(
    indices: Vec<usize>,
    original: &[BidiClass],
    levels: &[u8],
    removed: &[bool],
    paragraph_level: u8,
) -> Sequence {
    let level = indices.first().map_or(paragraph_level, |&i| levels[i]);
    let before = indices
        .first()
        .and_then(|&i| (0..i).rev().find(|&j| !removed[j]))
        .map_or(paragraph_level, |j| levels[j]);
    let sos = direction_of(level, before);
    let last = indices.last().copied().unwrap_or(0);
    let open_isolate = indices
        .last()
        .is_some_and(|&i| original[i].is_isolate_initiator());
    let after = if open_isolate {
        paragraph_level
    } else {
        (last + 1..levels.len())
            .find(|&j| !removed[j])
            .map_or(paragraph_level, |j| levels[j])
    };
    let eos = direction_of(level, after);
    Sequence {
        indices,
        level,
        sos,
        eos,
    }
}

/// Règles W1 à W7 sur une suite.
fn resolve_weak(classes: &mut [BidiClass], seq: &Sequence) {
    weak_w1(classes, seq);
    weak_w2_w3(classes, seq);
    weak_w4(classes, seq);
    weak_w5_w6(classes, seq);
    weak_w7(classes, seq);
}

/// W1 : une marque non espaçante prend la classe de son voisin de gauche.
fn weak_w1(classes: &mut [BidiClass], seq: &Sequence) {
    let mut previous = seq.sos;
    for &i in &seq.indices {
        if classes[i] == BidiClass::Nsm {
            classes[i] = if previous.is_isolate_initiator() || previous == BidiClass::Pdi {
                BidiClass::On
            } else {
                previous
            };
        }
        previous = classes[i];
    }
}

/// W2 : `EN` devient `AN` après un `AL` ; W3 : `AL` devient `R`.
fn weak_w2_w3(classes: &mut [BidiClass], seq: &Sequence) {
    let mut strong = seq.sos;
    for &i in &seq.indices {
        match classes[i] {
            BidiClass::L | BidiClass::R | BidiClass::Al => strong = classes[i],
            BidiClass::En if strong == BidiClass::Al => classes[i] = BidiClass::An,
            _ => {}
        }
    }
    for &i in &seq.indices {
        if classes[i] == BidiClass::Al {
            classes[i] = BidiClass::R;
        }
    }
}

/// W4 : un séparateur unique entre deux chiffres de même type disparaît.
fn weak_w4(classes: &mut [BidiClass], seq: &Sequence) {
    let n = seq.indices.len();
    for k in 1..n.saturating_sub(1) {
        let (before, here, after) = (
            classes[seq.indices[k - 1]],
            classes[seq.indices[k]],
            classes[seq.indices[k + 1]],
        );
        let merged = match here {
            BidiClass::Es | BidiClass::Cs if before == BidiClass::En && after == BidiClass::En => {
                Some(BidiClass::En)
            }
            BidiClass::Cs if before == BidiClass::An && after == BidiClass::An => {
                Some(BidiClass::An)
            }
            _ => None,
        };
        if let Some(class) = merged {
            classes[seq.indices[k]] = class;
        }
    }
}

/// W5 : une suite de `ET` collée à un `EN` devient `EN` ; W6 : ce qui reste
/// de `ET`, `ES` et `CS` devient `ON`.
fn weak_w5_w6(classes: &mut [BidiClass], seq: &Sequence) {
    let n = seq.indices.len();
    let mut k = 0;
    while k < n {
        if classes[seq.indices[k]] != BidiClass::Et {
            k += 1;
            continue;
        }
        let mut end = k;
        while end < n && classes[seq.indices[end]] == BidiClass::Et {
            end += 1;
        }
        let before = k
            .checked_sub(1)
            .map_or(seq.sos, |p| classes[seq.indices[p]]);
        let after = if end < n {
            classes[seq.indices[end]]
        } else {
            seq.eos
        };
        if before == BidiClass::En || after == BidiClass::En {
            for &i in &seq.indices[k..end] {
                classes[i] = BidiClass::En;
            }
        }
        k = end;
    }
    for &i in &seq.indices {
        if matches!(classes[i], BidiClass::Et | BidiClass::Es | BidiClass::Cs) {
            classes[i] = BidiClass::On;
        }
    }
}

/// W7 : `EN` devient `L` si le dernier caractère fort avant lui est `L`.
fn weak_w7(classes: &mut [BidiClass], seq: &Sequence) {
    let mut strong = seq.sos;
    for &i in &seq.indices {
        match classes[i] {
            BidiClass::L | BidiClass::R => strong = classes[i],
            BidiClass::En if strong == BidiClass::L => classes[i] = BidiClass::L,
            _ => {}
        }
    }
}

/// Direction forte équivalente d'une classe pour les règles N (les chiffres
/// comptent comme `R`).
fn neutral_context(class: BidiClass) -> Option<BidiClass> {
    match class {
        BidiClass::L => Some(BidiClass::L),
        BidiClass::R | BidiClass::En | BidiClass::An => Some(BidiClass::R),
        _ => None,
    }
}

/// Règles N1 et N2 : neutres entre deux directions, puis repli sur la
/// direction d'enchâssement.
fn resolve_neutrals(classes: &mut [BidiClass], seq: &Sequence) {
    let n = seq.indices.len();
    let embedding = if seq.level % 2 == 1 {
        BidiClass::R
    } else {
        BidiClass::L
    };
    let mut k = 0;
    while k < n {
        if !classes[seq.indices[k]].is_neutral_or_isolate() {
            k += 1;
            continue;
        }
        let mut end = k;
        while end < n && classes[seq.indices[end]].is_neutral_or_isolate() {
            end += 1;
        }
        let before = k
            .checked_sub(1)
            .and_then(|p| neutral_context(classes[seq.indices[p]]))
            .unwrap_or(seq.sos);
        let after = if end < n {
            neutral_context(classes[seq.indices[end]]).unwrap_or(embedding)
        } else {
            seq.eos
        };
        let resolved = if before == after { before } else { embedding };
        for &i in &seq.indices[k..end] {
            classes[i] = resolved;
        }
        k = end;
    }
}

/// Règles I1 et I2 : niveaux implicites.
fn resolve_implicit(classes: &[BidiClass], levels: &mut [u8], seq: &Sequence) {
    for &i in &seq.indices {
        let level = levels[i];
        let bump = if level % 2 == 0 {
            match classes[i] {
                BidiClass::R => 1,
                BidiClass::An | BidiClass::En => 2,
                _ => 0,
            }
        } else {
            match classes[i] {
                BidiClass::L | BidiClass::En | BidiClass::An => 1,
                _ => 0,
            }
        };
        levels[i] = level.saturating_add(bump);
    }
}

/// Règle L1 : séparateurs et blancs de fin reviennent au niveau du
/// paragraphe.
fn reset_whitespace(original: &[BidiClass], levels: &mut [u8], paragraph_level: u8) {
    // `resettable` est vrai tant que l'on n'a croisé, en remontant, que des
    // blancs, des formatages d'isolat et des caractères retirés par X9 :
    // c'est exactement la « suite de blancs en fin de ligne » de L1.
    let mut resettable = true;
    for i in (0..original.len()).rev() {
        match original[i] {
            BidiClass::S | BidiClass::B => {
                levels[i] = paragraph_level;
                resettable = true;
            }
            BidiClass::Ws | BidiClass::Fsi | BidiClass::Lri | BidiClass::Rli | BidiClass::Pdi => {
                if resettable {
                    levels[i] = paragraph_level;
                }
            }
            class if class.is_removed_by_x9() => {
                if resettable {
                    levels[i] = paragraph_level;
                }
            }
            _ => resettable = false,
        }
    }
}

/// Résout les niveaux bidirectionnels d'un paragraphe.
///
/// Le vecteur `levels` a **un élément par caractère** (pas par octet) ;
/// `offsets` donne l'indice d'octet de chaque caractère, suivi de la
/// longueur totale de la chaîne.
#[must_use]
pub fn resolve(text: &str, base: BaseDirection) -> BidiInfo {
    let chars: Vec<char> = text.chars().collect();
    let mut offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    offsets.push(text.len());
    let original: Vec<BidiClass> = chars.iter().copied().map(bidi_class).collect();
    let paragraph_level = match base {
        BaseDirection::LeftToRight => 0,
        BaseDirection::RightToLeft => 1,
        BaseDirection::Auto => first_strong_level(&original, 0, original.len()),
    };
    let mut classes = original.clone();
    let mut levels = explicit_levels(&mut classes, &original, paragraph_level);
    let removed: Vec<bool> = original.iter().map(|c| c.is_removed_by_x9()).collect();
    for seq in isolating_sequences(&original, &levels, &removed, paragraph_level) {
        resolve_weak(&mut classes, &seq);
        resolve_neutrals(&mut classes, &seq);
        resolve_implicit(&classes, &mut levels, &seq);
    }
    // X9 « en conservant » : un caractère retiré prend le niveau de son
    // voisin de gauche, ce qui évite de couper un passage en deux.
    for i in 0..levels.len() {
        if removed[i] {
            levels[i] = if i == 0 {
                paragraph_level
            } else {
                levels[i - 1]
            };
        }
    }
    reset_whitespace(&original, &mut levels, paragraph_level);
    BidiInfo {
        paragraph_level,
        levels,
        offsets,
    }
}

/// Passages du paragraphe dans l'**ordre visuel**, de gauche à droite.
///
/// Chaque passage porte son niveau : un passage de niveau impair doit être
/// composé de droite à gauche (le shaper inverse alors l'ordre des glyphes).
/// Les indices sont des **indices d'octets** dans `text`, directement
/// utilisables avec `&text[run.start..run.end]`.
///
/// ```
/// use acrux_fonts::unicode::bidi::{reorder_visual, BaseDirection};
///
/// let runs = reorder_visual("abc אבג", BaseDirection::Auto);
/// assert_eq!(runs.len(), 2);
/// assert!(!runs[0].is_rtl());
/// assert!(runs[1].is_rtl());
/// ```
#[must_use]
pub fn reorder_visual(text: &str, base: BaseDirection) -> Vec<Run> {
    let info = resolve(text, base);
    let n = info.levels.len();
    if n == 0 {
        return Vec::new();
    }
    // Passages logiques de niveau constant.
    let mut logical: Vec<(usize, usize, u8)> = Vec::new();
    let mut start = 0usize;
    for i in 1..=n {
        if i == n || info.levels[i] != info.levels[start] {
            logical.push((start, i, info.levels[start]));
            start = i;
        }
    }
    // L2 : inverser les passages contigus du niveau le plus haut jusqu'au
    // plus petit niveau impair.
    let highest = logical.iter().map(|r| r.2).max().unwrap_or(0);
    let lowest_odd = logical
        .iter()
        .map(|r| r.2)
        .filter(|l| l % 2 == 1)
        .min()
        .unwrap_or(highest + 1);
    let mut order: Vec<usize> = (0..logical.len()).collect();
    let mut level = highest;
    while level >= lowest_odd && level > 0 {
        let mut i = 0;
        while i < order.len() {
            if logical[order[i]].2 >= level {
                let mut j = i;
                while j < order.len() && logical[order[j]].2 >= level {
                    j += 1;
                }
                order[i..j].reverse();
                i = j;
            } else {
                i += 1;
            }
        }
        level -= 1;
    }
    order
        .into_iter()
        .map(|k| {
            let (first, last, level) = logical[k];
            Run {
                start: info.offsets[first],
                end: info.offsets[last],
                level,
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn pure_latin_is_one_ltr_run() {
        let runs = reorder_visual("Bonjour le monde", BaseDirection::Auto);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].level, 0);
        assert_eq!((runs[0].start, runs[0].end), (0, 16));
    }

    #[test]
    fn empty_text() {
        assert!(reorder_visual("", BaseDirection::Auto).is_empty());
        assert_eq!(paragraph_level("", BaseDirection::Auto), 0);
    }

    /// P2/P3 : le premier caractère fort décide.
    #[test]
    fn paragraph_direction() {
        assert_eq!(paragraph_level("hello", BaseDirection::Auto), 0);
        assert_eq!(paragraph_level("שלום", BaseDirection::Auto), 1);
        assert_eq!(paragraph_level("123 שלום", BaseDirection::Auto), 1);
        assert_eq!(paragraph_level("שלום", BaseDirection::LeftToRight), 0);
        assert_eq!(paragraph_level("hello", BaseDirection::RightToLeft), 1);
    }

    /// Latin puis hébreu : deux passages, l'hébreu au niveau 1.
    #[test]
    fn latin_then_hebrew() {
        let text = "abc אבג";
        let runs = reorder_visual(text, BaseDirection::Auto);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].level, 0);
        assert_eq!(&text[runs[0].start..runs[0].end], "abc ");
        assert_eq!(runs[1].level, 1);
        assert_eq!(&text[runs[1].start..runs[1].end], "אבג");
    }

    /// Paragraphe hébreu contenant un mot latin : le latin passe au niveau
    /// 2 et se retrouve **à droite** dans l'ordre visuel.
    #[test]
    fn hebrew_paragraph_with_latin_word() {
        let text = "אבג abc";
        let runs = reorder_visual(text, BaseDirection::Auto);
        assert_eq!(runs.len(), 2);
        // Ordre visuel : le passage latin (niveau 2) vient en premier.
        assert_eq!(runs[0].level, 2);
        assert_eq!(&text[runs[0].start..runs[0].end], "abc");
        assert_eq!(runs[1].level, 1);
        assert_eq!(&text[runs[1].start..runs[1].end], "אבג ");
    }

    /// W2 : un chiffre européen après une lettre arabe devient un chiffre
    /// arabe, donc niveau 2 dans un paragraphe de droite à gauche.
    #[test]
    fn arabic_number_after_arabic_letter() {
        let text = "\u{0628}\u{0627}\u{0628} 12";
        let info = resolve(text, BaseDirection::Auto);
        assert_eq!(info.paragraph_level, 1);
        let last = info.levels.last().copied().unwrap();
        assert_eq!(last, 2, "les chiffres montent d'un niveau pair");
    }

    /// W7 : un chiffre après du latin reste au niveau du latin.
    #[test]
    fn european_number_after_latin() {
        let info = resolve("abc 12 אבג", BaseDirection::Auto);
        assert_eq!(info.levels[4], 0);
        assert_eq!(info.levels[5], 0);
    }

    /// L1 : les blancs de fin de paragraphe reviennent au niveau de base.
    #[test]
    fn trailing_whitespace_is_reset() {
        let text = "abc אבג  ";
        let info = resolve(text, BaseDirection::Auto);
        let n = info.levels.len();
        assert_eq!(info.levels[n - 1], 0);
        assert_eq!(info.levels[n - 2], 0);
    }

    /// X6 : un forçage `RLO` met tout le contenu en droite à gauche.
    #[test]
    fn right_to_left_override() {
        let text = "a\u{202E}bc\u{202C}d";
        let info = resolve(text, BaseDirection::LeftToRight);
        assert_eq!(info.levels[2], 1, "b forcé à droite à gauche");
        assert_eq!(info.levels[3], 1, "c forcé à droite à gauche");
        assert_eq!(info.levels[5], 0, "d revenu à gauche à droite");
    }

    /// X5a : un isolat `RLI` encadre son contenu sans contaminer la suite.
    #[test]
    fn right_to_left_isolate() {
        let text = "a\u{2067}abc\u{2069}b";
        let info = resolve(text, BaseDirection::LeftToRight);
        assert_eq!(info.levels[0], 0);
        assert_eq!(info.levels[2], 2, "le latin dans un isolat RTL");
        assert_eq!(info.levels[6], 0);
    }

    /// N1 : un neutre entre deux passages de droite à gauche suit la
    /// direction, pas le paragraphe.
    #[test]
    fn neutral_between_two_rtl() {
        let text = "אבג-אבג";
        let info = resolve(text, BaseDirection::Auto);
        assert_eq!(info.levels[3], 1, "le tiret reste au niveau 1");
    }

    /// Un paragraphe entièrement neutre garde le niveau de base.
    #[test]
    fn only_neutrals() {
        let info = resolve("... ---", BaseDirection::Auto);
        assert!(info.levels.iter().all(|&l| l == 0));
        assert!(!info.has_rtl());
    }

    #[test]
    fn has_rtl_detects_mixed_text() {
        assert!(resolve("abc אבג", BaseDirection::Auto).has_rtl());
        assert!(!resolve("abc", BaseDirection::Auto).has_rtl());
    }

    /// Les niveaux ne dépassent jamais la profondeur maximale.
    #[test]
    fn deep_nesting_is_bounded() {
        let text: String = std::iter::repeat_n('\u{202B}', 200).collect::<String>() + "a";
        let info = resolve(&text, BaseDirection::LeftToRight);
        assert!(info.levels.iter().all(|&l| l <= MAX_DEPTH + 1));
    }

    /// Un `PDI` orphelin ne fait pas paniquer l'algorithme.
    #[test]
    fn unmatched_pdi() {
        let info = resolve("a\u{2069}b", BaseDirection::LeftToRight);
        assert_eq!(info.levels.len(), 3);
    }
}
