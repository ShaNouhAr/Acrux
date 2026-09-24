//! Thème : couleurs (RVB 8 bits) et dimensions en pixels logiques (96 dpi ;
//! multiplier par l'échelle DPI de la fenêtre).

/// Couleur RVB 8 bits.
pub type Rgb = (u8, u8, u8);

/// Thème sombre par défaut (celui d'Acrobat moderne, en plus sobre).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Fond de la zone de document.
    pub canvas: Rgb,
    /// Ombre des pages.
    pub page_shadow: Rgb,
    /// Fond des barres (état, outils).
    pub bar: Rgb,
    /// Séparateurs.
    pub separator: Rgb,
    /// Texte principal.
    pub text: Rgb,
    /// Texte secondaire.
    pub text_dim: Rgb,
    /// Accent (sélection, focus).
    pub accent: Rgb,
    /// Fond d'un bouton survolé.
    pub hover: Rgb,
    /// Fond d'un bouton secondaire survolé. Il se lit par contraste avec le
    /// bouton au repos (`hover`) : en thème sombre il **éclaircit**, comme
    /// sous Windows 11 — assombrir un bouton déjà sombre le ferait paraître
    /// enfoncé, pas survolé. En thème clair, il assombrit.
    pub button_hover: Rgb,
    /// Fond d'une info-bulle (légèrement détaché des barres pour se lire
    /// par-dessus n'importe quel fond).
    pub tip_bg: Rgb,
    /// Teinte d'une erreur ou d'un danger (mot de passe faible, saisie
    /// refusée), lisible sur le fond des barres.
    pub danger: Rgb,
    /// Teinte d'une mise en garde, entre le danger et l'accent.
    pub warning: Rgb,
    /// Hauteur de la barre d'état (px logiques).
    pub status_height: u32,
    /// Hauteur de la barre d'outils (px logiques).
    pub toolbar_height: u32,
    /// Hauteur de la barre d'onglets (px logiques).
    pub tab_height: u32,
    /// Taille du texte d'interface (px logiques).
    pub font_size: f32,
}

impl Theme {
    /// Thème sombre.
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            canvas: (0x50, 0x52, 0x56),
            page_shadow: (0x30, 0x31, 0x33),
            bar: (0x2B, 0x2C, 0x2F),
            separator: (0x1E, 0x1F, 0x21),
            text: (0xE8, 0xE8, 0xE8),
            text_dim: (0xA0, 0xA3, 0xA8),
            accent: (0x4C, 0x8B, 0xF5),
            hover: (0x3E, 0x40, 0x45),
            button_hover: (0x4A, 0x4C, 0x52),
            tip_bg: (0x1B, 0x1C, 0x1E),
            danger: (0xE5, 0x53, 0x53),
            warning: (0xE0, 0xA1, 0x3A),
            status_height: 26,
            toolbar_height: 40,
            tab_height: 30,
            font_size: 13.0,
        }
    }

    /// Thème clair.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            canvas: (0xE4, 0xE5, 0xE8),
            page_shadow: (0xB8, 0xB9, 0xBC),
            bar: (0xF4, 0xF4, 0xF6),
            separator: (0xD0, 0xD1, 0xD4),
            text: (0x20, 0x20, 0x22),
            text_dim: (0x6A, 0x6C, 0x70),
            accent: (0x2F, 0x6F, 0xE0),
            hover: (0xDE, 0xDF, 0xE3),
            button_hover: (0xD0, 0xD1, 0xD4),
            tip_bg: (0xFF, 0xFF, 0xFF),
            danger: (0xC6, 0x28, 0x28),
            warning: (0xB0, 0x6A, 0x00),
            status_height: 26,
            toolbar_height: 40,
            tab_height: 30,
            font_size: 13.0,
        }
    }

    /// Vrai pour un thème sombre. On juge sur la luminance des barres, pas
    /// sur l'égalité avec [`Theme::dark`] : un thème retouché ou ajouté plus
    /// tard (contraste élevé) garde ainsi le bon bouton, la bonne barre de
    /// titre et la bonne préférence enregistrée.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        let (r, g, b) = self.bar;
        // Pondération de Rec. 601, en entiers : assez fine pour séparer un
        // fond sombre d'un fond clair.
        u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 < 128 * 1000
    }

    /// Encre d'un contrôle **désactivé** : à mi-chemin entre le texte
    /// secondaire et le fond des barres.
    ///
    /// `text_dim` seul ne suffisait pas : un bouton grisé restait presque
    /// aussi lisible qu'un bouton actif, et l'on cliquait dessus pour rien.
    /// À mi-chemin, l'icône se lit encore — on sait ce qu'elle ferait — mais
    /// se voit nettement éteinte.
    #[must_use]
    pub const fn disabled(&self) -> Rgb {
        (
            self.text_dim.0.midpoint(self.bar.0),
            self.text_dim.1.midpoint(self.bar.1),
            self.text_dim.2.midpoint(self.bar.2),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chaque_theme_se_reconnait() {
        assert!(Theme::dark().is_dark());
        assert!(!Theme::light().is_dark());
    }

    /// L'encre désactivée tombe entre le texte secondaire et le fond, dans
    /// les deux thèmes : plus pâle que `text_dim`, jamais invisible.
    #[test]
    fn le_desactive_est_entre_le_texte_et_le_fond() {
        for t in [Theme::dark(), Theme::light()] {
            let d = t.disabled();
            for (c, (lo, hi)) in [
                (d.0, (t.text_dim.0, t.bar.0)),
                (d.1, (t.text_dim.1, t.bar.1)),
                (d.2, (t.text_dim.2, t.bar.2)),
            ] {
                assert!(c > lo.min(hi) && c < lo.max(hi), "{d:?}");
            }
        }
        assert_eq!(Theme::dark().disabled(), (0x65, 0x67, 0x6B));
        assert_eq!(Theme::light().disabled(), (0xAF, 0xB0, 0xB3));
    }
}
