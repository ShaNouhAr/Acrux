//! Animations de l'interface.
//!
//! Une animation n'est pas là pour faire joli : elle dit **d'où vient ce
//! qu'on voit**. Un panneau qui glisse montre qu'il vient du bord, une page
//! qui se déplace en continu garde l'œil sur la ligne qu'on lisait, une
//! fenêtre qui apparaît en fondu se distingue d'un changement de contenu.
//! Une animation qui n'apprend rien est du temps perdu à l'utilisateur ; il
//! n'y en a donc que quelques-unes.
//!
//! Chaque valeur animée avance **vers sa cible** à chaque pas de temps, d'une
//! fraction de ce qui reste. Cette approche exponentielle a trois qualités :
//! elle ne dépend pas de la cadence d'affichage, elle repart proprement si la
//! cible change en cours de route (un second coup de molette), et elle
//! ralentit en arrivant, ce que l'œil attend.

/// Une valeur qui rejoint sa cible en douceur.
#[derive(Debug, Clone, Copy)]
pub struct Anim {
    /// Valeur affichée.
    pub value: f64,
    /// Valeur visée.
    pub target: f64,
    /// Durée approximative pour couvrir l'écart, en secondes.
    pub duration: f64,
    /// En deçà de cet écart, on colle à la cible et l'animation s'arrête.
    pub epsilon: f64,
}

impl Anim {
    /// Valeur fixe, déjà arrivée.
    #[must_use]
    pub fn new(value: f64, duration: f64) -> Self {
        Anim {
            value,
            target: value,
            duration: duration.max(0.001),
            epsilon: 0.5,
        }
    }

    /// Change la cible sans saut.
    pub fn go_to(&mut self, target: f64) {
        self.target = target;
    }

    /// Change la valeur **et** la cible : aucun mouvement.
    pub fn set(&mut self, value: f64) {
        self.value = value;
        self.target = value;
    }

    /// Vrai s'il reste du chemin.
    #[must_use]
    pub fn running(&self) -> bool {
        (self.target - self.value).abs() > self.epsilon
    }

    /// Avance de `dt` secondes. Rend vrai si la valeur a changé.
    pub fn step(&mut self, dt: f64) -> bool {
        if !self.running() {
            if (self.target - self.value).abs() > 0.0 {
                self.value = self.target;
                return true;
            }
            return false;
        }
        // Fraction restante après `dt` : une exponentielle, calée pour couvrir
        // 95 % de l'écart en `duration`.
        let k = (-3.0 * dt / self.duration).exp();
        self.value = self.target - (self.target - self.value) * k;
        if (self.target - self.value).abs() <= self.epsilon {
            self.value = self.target;
        }
        true
    }
}

/// Horloge d'animation : le temps écoulé depuis le pas précédent.
///
/// Le temps est borné : après une pause du programme (fenêtre réduite, machine
/// en veille), un `dt` de plusieurs secondes ferait sauter toutes les
/// animations d'un coup.
#[derive(Debug)]
pub struct Clock {
    last: std::time::Instant,
}

impl Default for Clock {
    fn default() -> Self {
        Clock {
            last: std::time::Instant::now(),
        }
    }
}

impl Clock {
    /// Secondes écoulées depuis le dernier appel, au plus 1/20 s.
    pub fn tick(&mut self) -> f64 {
        let now = std::time::Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        dt.min(0.05)
    }
}

/// Adoucissement d'une progression de 0 à 1 : départ franc, arrivée douce.
#[must_use]
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn une_valeur_rejoint_sa_cible_et_sarrete() {
        let mut a = Anim::new(0.0, 0.2);
        a.go_to(100.0);
        assert!(a.running());
        let mut steps = 0;
        while a.step(1.0 / 60.0) && steps < 1_000 {
            steps += 1;
        }
        assert!((a.value - 100.0).abs() < 0.001, "{}", a.value);
        assert!(!a.running());
        // Environ un cinquième de seconde à 60 Hz, pas dix.
        assert!(steps < 30, "{steps} pas");
    }

    #[test]
    fn changer_de_cible_en_route_ne_saute_pas() {
        let mut a = Anim::new(0.0, 0.2);
        a.go_to(100.0);
        a.step(0.05);
        let milieu = a.value;
        assert!(milieu > 0.0 && milieu < 100.0);
        a.go_to(0.0);
        a.step(0.001);
        assert!(a.value < milieu, "on repart de là où l'on était");
    }

    #[test]
    fn le_temps_est_borne() {
        let mut c = Clock::default();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let dt = c.tick();
        assert!(dt > 0.0 && dt <= 0.05);
    }

    #[test]
    fn ladoucissement_va_de_zero_a_un() {
        assert!(ease_out(0.0).abs() < 1e-9);
        assert!((ease_out(1.0) - 1.0).abs() < 1e-9);
        assert!(ease_out(0.5) > 0.5, "l'arrivée est douce, le départ franc");
        assert!((ease_out(-3.0)).abs() < 1e-9);
    }
}
