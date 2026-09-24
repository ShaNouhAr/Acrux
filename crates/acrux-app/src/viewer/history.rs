//! Historique de la vue : « Vue précédente » et « Vue suivante » (Alt+← et
//! Alt+→, les boutons latéraux de la souris), comme dans Acrobat et dans les
//! navigateurs.
//!
//! Chaque onglet a le sien : il vit dans le document ouvert (`Loaded`), suit
//! l'onglet quand on en change et disparaît quand on le ferme.
//!
//! Seuls les **sauts** s'y inscrivent : un lien ou un signet suivi, « aller à
//! la page », Origine et Fin, une vignette ou un commentaire du panneau, une
//! occurrence de la recherche. Le défilement à la molette, aux flèches ou
//! page par page n'en est pas un — Acrobat ne le compte pas non plus : la
//! pile se remplirait de positions que personne ne cherche à retrouver, et
//! revenir en arrière ne ramènerait nulle part d'utile.
//!
//! Une vue retenue est une page et la part de sa hauteur déjà passée en haut
//! de l'écran, pas des pixels : elle se retrouve après un zoom, un changement
//! de disposition ou une rotation de la vue.

use crate::render_worker::EditOp;

/// Nombre de vues retenues dans chaque sens. Au-delà, la plus ancienne
/// s'efface : c'est la pile d'un lecteur, pas un journal.
pub(super) const LIMIT: usize = 64;

/// Une vue du document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Spot {
    /// Page en haut de la vue.
    pub(super) page: usize,
    /// Part de sa hauteur déjà passée au-dessus du bord de la vue, entre 0
    /// (le haut de la page est au bord) et 1.
    pub(super) frac: f64,
}

impl Spot {
    /// Deux vues qu'on ne distingue pas à l'œil : même page, et moins d'un
    /// cinquantième de page d'écart. Y revenir ne ferait rien bouger.
    pub(super) fn near(self, other: Spot) -> bool {
        self.page == other.page && (self.frac - other.frac).abs() < 0.02
    }
}

/// Vues précédentes et suivantes d'un onglet.
#[derive(Debug, Default)]
pub(super) struct NavHistory {
    /// Vues quittées par un saut, la plus récente à la fin.
    back: Vec<Spot>,
    /// Vues quittées en revenant en arrière, la plus récente à la fin.
    forward: Vec<Spot>,
}

/// Empile sans dépasser [`LIMIT`] : la plus ancienne vue s'efface.
fn push_bounded(stack: &mut Vec<Spot>, spot: Spot) {
    stack.push(spot);
    if stack.len() > LIMIT {
        stack.remove(0);
    }
}

impl NavHistory {
    /// Un saut part de `from`. Comme dans un navigateur, un nouveau chemin
    /// efface les vues « suivantes » : on ne peut plus y avancer.
    pub(super) fn record(&mut self, from: Spot) {
        self.forward.clear();
        // Deux sauts de suite depuis le même endroit (deux liens essayés
        // l'un après l'autre) n'y ramènent qu'une fois.
        if self.back.last().is_some_and(|last| last.near(from)) {
            return;
        }
        push_bounded(&mut self.back, from);
    }

    /// Vue précédente, quand on est en `here` : `here` devient la vue
    /// suivante. `None` si l'on est au début.
    pub(super) fn back(&mut self, here: Spot) -> Option<Spot> {
        step(&mut self.back, &mut self.forward, here)
    }

    /// Vue suivante, l'inverse de [`NavHistory::back`].
    pub(super) fn forward(&mut self, here: Spot) -> Option<Spot> {
        step(&mut self.forward, &mut self.back, here)
    }

    /// Il y a une vue précédente.
    pub(super) fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    /// Il y a une vue suivante.
    pub(super) fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Suit une modification de la structure du document : chaque page
    /// retenue devient `f(page)`, et disparaît si `f` rend `None` (la page
    /// a été supprimée). Sans cela, revenir en arrière après avoir supprimé
    /// une page mènerait à sa voisine.
    pub(super) fn remap(&mut self, f: impl Fn(usize) -> Option<usize>) {
        for stack in [&mut self.back, &mut self.forward] {
            let moved: Vec<Spot> = stack
                .iter()
                .filter_map(|s| f(s.page).map(|page| Spot { page, frac: s.frac }))
                .collect();
            stack.clear();
            // Deux vues devenues voisines identiques n'en font plus qu'une.
            for spot in moved {
                if !stack.last().is_some_and(|last| last.near(spot)) {
                    stack.push(spot);
                }
            }
        }
    }
}

/// Passe d'une pile à l'autre : la cible sort de `from`, la vue actuelle
/// entre dans `to`. Les vues de `from` qu'on ne distingue pas de l'actuelle
/// sont sautées : y « revenir » ne ferait rien, et l'on croirait la touche
/// sans effet.
fn step(from: &mut Vec<Spot>, to: &mut Vec<Spot>, here: Spot) -> Option<Spot> {
    while from.last().is_some_and(|s| s.near(here)) {
        from.pop();
    }
    let target = from.pop()?;
    if !to.last().is_some_and(|last| last.near(here)) {
        push_bounded(to, here);
    }
    Some(target)
}

/// Vrai si `op` supprime, insère ou déplace des pages : les vues retenues
/// avant elle ne désignent plus les mêmes pages après.
pub(super) fn moves_pages(op: &EditOp) -> bool {
    matches!(
        op,
        EditOp::Delete { .. }
            | EditOp::Insert { .. }
            | EditOp::InsertBlank { .. }
            | EditOp::Reorder { .. }
    )
}

/// Où va la page `page` après la modification `op` : `None` si elle a été
/// supprimée. `inserted` est le nombre de pages que `op` a ajoutées (une
/// insertion ne dit pas d'avance combien le fichier source en apporte). Les
/// modifications qui ne touchent pas à la structure la laissent en place.
pub(super) fn page_after(op: &EditOp, page: usize, inserted: usize) -> Option<usize> {
    match op {
        EditOp::Delete { pages } => {
            if pages.contains(&page) {
                return None;
            }
            let mut before: Vec<usize> = pages.iter().copied().filter(|&d| d < page).collect();
            before.sort_unstable();
            before.dedup();
            Some(page - before.len())
        }
        // `order[i]` est la page d'origine de la page `i` : une page
        // dupliquée y figure deux fois, et l'on garde sa première place.
        EditOp::Reorder { order } => order.iter().position(|&o| o == page),
        EditOp::Insert { at, .. } | EditOp::InsertBlank { at, .. } => {
            Some(if page < *at { page } else { page + inserted })
        }
        _ => Some(page),
    }
}

/// L'inverse de [`page_after`], pour une annulation : où revient la page
/// `page` quand on défait `op`. `None` si elle n'existait pas avant (une
/// page insérée). `inserted` est le nombre de pages que `op` avait ajoutées.
///
/// Sans elle, Ctrl+Z après une suppression vidait l'historique de la vue
/// et ramenait l'écran à un décalage en pixels qui ne désignait plus la
/// même page.
pub(super) fn page_before(op: &EditOp, page: usize, inserted: usize) -> Option<usize> {
    match op {
        EditOp::Delete { pages } => {
            // La `page`-ième des pages restées : chaque page supprimée
            // avant elle la repousse d'un cran.
            let mut gone: Vec<usize> = pages.clone();
            gone.sort_unstable();
            gone.dedup();
            let mut before = page;
            for d in gone {
                if d <= before {
                    before += 1;
                }
            }
            Some(before)
        }
        // Une copie revient à la page dont elle est la copie.
        EditOp::Reorder { order } => order.get(page).copied(),
        EditOp::Insert { at, .. } | EditOp::InsertBlank { at, .. } => {
            if page < *at {
                Some(page)
            } else if page >= at + inserted {
                Some(page - inserted)
            } else {
                None
            }
        }
        _ => Some(page),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(page: usize, frac: f64) -> Spot {
        Spot { page, frac }
    }

    #[test]
    fn retour_puis_suivant() {
        let mut h = NavHistory::default();
        h.record(at(0, 0.0)); // A → B
        h.record(at(4, 0.5)); // B → C
        assert!(h.can_back() && !h.can_forward());
        assert_eq!(h.back(at(9, 0.0)), Some(at(4, 0.5)), "de C, on revient à B");
        assert_eq!(h.back(at(4, 0.5)), Some(at(0, 0.0)), "puis à A");
        assert_eq!(h.back(at(0, 0.0)), None, "rien avant A");
        assert_eq!(h.forward(at(0, 0.0)), Some(at(4, 0.5)));
        assert_eq!(h.forward(at(4, 0.5)), Some(at(9, 0.0)));
        assert_eq!(h.forward(at(9, 0.0)), None);
    }

    #[test]
    fn un_saut_efface_les_vues_suivantes() {
        let mut h = NavHistory::default();
        h.record(at(0, 0.0));
        assert_eq!(h.back(at(3, 0.0)), Some(at(0, 0.0)));
        assert!(h.can_forward());
        h.record(at(0, 0.0));
        assert!(!h.can_forward(), "le chemin a changé");
        assert_eq!(h.back(at(7, 0.0)), Some(at(0, 0.0)));
    }

    #[test]
    fn deux_sauts_du_meme_endroit_ne_comptent_qu_une_fois() {
        let mut h = NavHistory::default();
        h.record(at(2, 0.30));
        h.record(at(2, 0.31));
        assert_eq!(h.back(at(5, 0.0)), Some(at(2, 0.30)));
        assert_eq!(h.back(at(2, 0.30)), None);
        // Une autre page, ou un autre endroit de la page, compte.
        h.record(at(2, 0.30));
        h.record(at(2, 0.60));
        h.record(at(3, 0.60));
        assert_eq!(h.back.len(), 3);
    }

    #[test]
    fn revenir_saute_les_vues_ou_l_on_est_deja() {
        let mut h = NavHistory::default();
        h.record(at(0, 0.0));
        h.record(at(6, 0.0));
        // Revenu à la page 6 à la molette : Alt+← doit mener quelque part.
        assert_eq!(h.back(at(6, 0.0)), Some(at(0, 0.0)));
    }

    #[test]
    fn la_pile_est_bornee() {
        let mut h = NavHistory::default();
        for page in 0..LIMIT + 10 {
            h.record(at(page, 0.0));
        }
        assert_eq!(h.back.len(), LIMIT);
        assert_eq!(
            h.back.first(),
            Some(&at(10, 0.0)),
            "les plus anciennes sont parties"
        );
        // Revenir tout en arrière remplit l'autre pile, bornée elle aussi.
        let mut here = at(999, 0.0);
        while let Some(s) = h.back(here) {
            here = s;
        }
        assert_eq!(h.forward.len(), LIMIT);
    }

    #[test]
    fn pile_vide() {
        let mut h = NavHistory::default();
        assert!(!h.can_back() && !h.can_forward());
        assert_eq!(h.back(at(1, 0.0)), None);
        assert_eq!(h.forward(at(1, 0.0)), None);
        assert!(!h.can_forward(), "un retour impossible ne fabrique rien");
    }

    #[test]
    fn suppression_de_pages() {
        let op = EditOp::Delete {
            pages: vec![5, 2, 2],
        };
        assert_eq!(page_after(&op, 0, 0), Some(0));
        assert_eq!(page_after(&op, 2, 0), None);
        assert_eq!(page_after(&op, 3, 0), Some(2));
        assert_eq!(page_after(&op, 5, 0), None);
        assert_eq!(page_after(&op, 6, 0), Some(4));
        let mut h = NavHistory::default();
        h.record(at(3, 0.0));
        h.record(at(5, 0.0));
        h.record(at(6, 0.2));
        h.remap(|p| page_after(&op, p, 0));
        assert_eq!(h.back, vec![at(2, 0.0), at(4, 0.2)]);
    }

    #[test]
    fn reordonnancement_et_duplication() {
        // La page 2 remonte en tête ; la page 0 est dupliquée.
        let op = EditOp::Reorder {
            order: vec![2, 0, 0, 1],
        };
        assert_eq!(page_after(&op, 2, 0), Some(0));
        assert_eq!(page_after(&op, 0, 0), Some(1), "la première place");
        assert_eq!(page_after(&op, 1, 0), Some(3));
        assert_eq!(page_after(&op, 7, 0), None, "absente du nouvel ordre");
    }

    #[test]
    fn insertion_de_pages() {
        let op = EditOp::Insert {
            source: std::sync::Arc::new(crate::render_worker::InsertSource {
                name: String::new(),
                pdf: Vec::new(),
                password: None,
                forms: false,
            }),
            pages: Vec::new(),
            at: 2,
        };
        assert_eq!(page_after(&op, 1, 3), Some(1));
        assert_eq!(page_after(&op, 2, 3), Some(5));
        // Une modification qui ne touche pas à la structure ne déplace rien.
        let rotate = EditOp::Rotate {
            pages: vec![1],
            degrees: 90,
        };
        assert_eq!(page_after(&rotate, 1, 0), Some(1));
        assert!(moves_pages(&op) && !moves_pages(&rotate));
    }

    #[test]
    fn annuler_ramene_chaque_page_a_sa_place() {
        let delete = EditOp::Delete {
            pages: vec![5, 2, 2],
        };
        let reorder = EditOp::Reorder {
            order: vec![2, 0, 0, 1],
        };
        let insert = EditOp::Insert {
            source: std::sync::Arc::new(crate::render_worker::InsertSource {
                name: String::new(),
                pdf: Vec::new(),
                password: None,
                forms: false,
            }),
            pages: Vec::new(),
            at: 2,
        };
        // Pour chaque page restée, défaire après avoir fait ne bouge rien.
        for (op, inserted) in [(&delete, 0), (&insert, 3)] {
            for page in 0..9 {
                if let Some(after) = page_after(op, page, inserted) {
                    assert_eq!(page_before(op, after, inserted), Some(page), "{op:?}");
                }
            }
        }
        for page in 0..3 {
            let back = page_after(&reorder, page, 0).and_then(|a| page_before(&reorder, a, 0));
            assert_eq!(back, Some(page));
        }
        // La copie de la page 0 revient à la page 0 ; une page insérée
        // n'existait pas.
        assert_eq!(page_before(&reorder, 2, 0), Some(0));
        assert_eq!(page_before(&insert, 3, 3), None);
        let rotate = EditOp::Rotate {
            pages: vec![1],
            degrees: 90,
        };
        assert_eq!(page_before(&rotate, 4, 0), Some(4));
    }

    #[test]
    fn remap_fusionne_les_vues_devenues_identiques() {
        let mut h = NavHistory::default();
        h.record(at(1, 0.0));
        h.record(at(2, 0.0));
        h.record(at(3, 0.0));
        // Les pages 1 et 2 fusionnent (une fonction volontairement grossière).
        h.remap(|p| Some(p.clamp(1, 2)));
        assert_eq!(h.back, vec![at(1, 0.0), at(2, 0.0)]);
    }
}
