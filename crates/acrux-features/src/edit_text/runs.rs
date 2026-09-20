//! Les **styles** d'un bloc de texte : police, corps et couleur, caractère
//! par caractère.
//!
//! Une ligne mêle souvent plusieurs styles — « du **gras**, de l'*italique*
//! et un [lien](#) » en compte quatre. La recomposer avec une seule police
//! effacerait tout cela ; on relève donc le style de chaque caractère à
//! l'ouverture, et on le **reporte** sur le texte modifié.
//!
//! # Le report
//!
//! On ne sait pas ce que l'utilisateur a fait — il a pu taper, effacer,
//! coller. Mais on connaît le texte d'avant et celui d'après, et cela suffit
//! : ce qui n'a pas bougé au début garde son style, ce qui n'a pas bougé à la
//! fin aussi, et ce qui a été inséré au milieu prend le style de son voisin
//! de gauche. C'est ce que fait n'importe quel traitement de texte, et cela
//! couvre tous les gestes ordinaires.

use acrux_document::Name;

/// Un style de caractère, tel qu'il sera réécrit dans le flux.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Ressource de police (`/F2`).
    pub font: Name,
    /// Corps en espace texte, tel qu'il est écrit dans `Tf`.
    pub size: f64,
    /// Corps en points de page : c'est celui qui se dessine et se mesure.
    pub page_size: f64,
    /// Octets à réémettre pour rétablir la couleur (`0 g`, `1 0 0 rg`…).
    pub fill: Vec<u8>,
    /// Couleur en RVB, pour l'aperçu.
    pub color: [f32; 3],
}

/// Les styles d'un bloc : une table de styles, et un style par caractère.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Styles {
    /// Styles employés, sans doublon.
    pub runs: Vec<Run>,
    /// Style de chaque caractère du texte, par rang dans `runs`.
    pub per_char: Vec<u16>,
    /// Corps du bloc à l'ouverture, en points de page.
    ///
    /// Les corps des styles s'y rapportent : redimensionner le bloc les
    /// change tous dans la même proportion.
    pub base: f64,
}

impl Styles {
    /// Corps du caractère de rang `index`, pour une boîte de corps `size`.
    #[must_use]
    pub fn size_at(&self, index: usize, size: f64) -> f64 {
        let ratio = if self.base > 1e-9 { size / self.base } else { 1.0 };
        self.at(index).map_or(size, |r| r.page_size * ratio)
    }
}

impl Styles {
    /// Vrai si le bloc n'a qu'un seul style — le cas ordinaire.
    #[must_use]
    pub fn uniform(&self) -> bool {
        self.runs.len() <= 1
    }

    /// Style du caractère de rang `index`, à défaut le premier.
    #[must_use]
    pub fn at(&self, index: usize) -> Option<&Run> {
        let id = self.per_char.get(index).copied().unwrap_or(0);
        self.runs.get(id as usize).or_else(|| self.runs.first())
    }

    /// Ajoute un style et rend son rang, sans créer de doublon.
    pub fn intern(&mut self, run: Run) -> u16 {
        if let Some(i) = self.runs.iter().position(|r| *r == run) {
            return u16::try_from(i).unwrap_or(0);
        }
        self.runs.push(run);
        u16::try_from(self.runs.len() - 1).unwrap_or(0)
    }

    /// Reporte ces styles sur un texte modifié.
    ///
    /// `before` est le texte d'où viennent les styles, `after` celui qu'on
    /// veut habiller. Le début et la fin inchangés gardent leur style ; ce
    /// qui a été inséré prend celui du caractère qui le précède.
    #[must_use]
    pub fn carry(&self, before: &str, after: &str) -> Self {
        if self.runs.is_empty() {
            return Self::default();
        }
        let old: Vec<char> = before.chars().collect();
        let new: Vec<char> = after.chars().collect();
        // Début commun.
        let mut head = 0;
        while head < old.len() && head < new.len() && old[head] == new[head] {
            head += 1;
        }
        // Fin commune, sans empiéter sur le début.
        let mut tail = 0;
        while tail < old.len() - head.min(old.len())
            && tail < new.len() - head.min(new.len())
            && old[old.len() - 1 - tail] == new[new.len() - 1 - tail]
        {
            tail += 1;
        }
        let style_of = |i: usize| self.per_char.get(i).copied().unwrap_or(0);
        // Le style de ce qui est inséré : celui du caractère de gauche, à
        // défaut celui du premier caractère conservé à droite.
        let inserted = if head > 0 {
            style_of(head - 1)
        } else if tail > 0 {
            // Rien de commun à gauche : on prend le style du premier
            // caractère conservé à droite.
            style_of(old.len().saturating_sub(tail))
        } else {
            // Tout a changé : le style du bloc reste celui de son début.
            style_of(0)
        };
        let mut per_char = Vec::with_capacity(new.len());
        for i in 0..new.len() {
            let id = if i < head {
                style_of(i)
            } else if i + tail >= new.len() {
                // Queue conservée : on reprend le style depuis la fin.
                let from_end = new.len() - i;
                style_of(old.len().saturating_sub(from_end))
            } else {
                inserted
            };
            per_char.push(id);
        }
        Self {
            runs: self.runs.clone(),
            per_char,
            base: self.base,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn styles(spec: &[u16]) -> Styles {
        let runs = (0..=spec.iter().copied().max().unwrap_or(0))
            .map(|i| Run {
                font: Name::new(&format!("F{i}")),
                size: 10.0,
                page_size: 10.0,
                fill: b"0 g".to_vec(),
                color: [0.0, 0.0, 0.0],
            })
            .collect();
        Styles {
            runs,
            per_char: spec.to_vec(),
            base: 10.0,
        }
    }

    /// Une insertion au milieu prend le style de son voisin de gauche.
    #[test]
    fn linsertion_prend_le_style_de_gauche() {
        let s = styles(&[0, 0, 1, 1, 0, 0]);
        let after = s.carry("abcdef", "abcXYdef");
        assert_eq!(after.per_char, vec![0, 0, 1, 1, 1, 1, 0, 0]);
    }

    /// Ce qui est effacé emporte son style, le reste garde le sien.
    #[test]
    fn leffacement_ne_deplace_pas_les_styles() {
        let s = styles(&[0, 0, 1, 1, 2, 2]);
        let after = s.carry("abcdef", "abef");
        assert_eq!(after.per_char, vec![0, 0, 2, 2]);
    }

    /// Un texte inchangé garde exactement ses styles.
    #[test]
    fn un_texte_inchange_garde_tout() {
        let s = styles(&[0, 1, 2, 1, 0]);
        let after = s.carry("abcde", "abcde");
        assert_eq!(after.per_char, s.per_char);
    }

    /// Taper dans un bloc vide donne le premier style.
    #[test]
    fn un_bloc_vide_prend_le_premier_style() {
        let s = styles(&[0]);
        let after = s.carry("", "neuf");
        assert_eq!(after.per_char, vec![0, 0, 0, 0]);
    }

    /// Tout remplacer donne partout le style du début.
    #[test]
    fn tout_remplacer_garde_le_style_du_debut() {
        let s = styles(&[1, 1, 1]);
        let after = s.carry("abc", "xyz");
        assert_eq!(after.per_char, vec![1, 1, 1]);
    }
}
