//! Lecture d'un média **sans image** : du son seul.
//!
//! Le son d'un PDF ne s'accompagne pas toujours d'une vidéo. Une annotation
//! `/Sound` (§13.3) n'a par définition rien à montrer, et un `/Screen` peut
//! très bien désigner un MP3. Le lecteur doit donc tenir sans piste vidéo —
//! ce qu'il ne faisait pas : la fin de lecture se jugeait à la seule vidéo, et
//! faute de vidéo elle n'arrivait jamais.
//!
//! Le fichier d'essai est **fabriqué ici**, et il est **silencieux**. Deux
//! raisons : aucun fichier audio n'a à être versionné dans un dépôt de moteur
//! PDF, et une suite de tests qui se met à chanter est une suite de tests
//! qu'on finit par ne plus lancer.
//!
//! Sur une machine sans périphérique audio — certains serveurs d'intégration
//! n'en ont pas — le lecteur ne peut pas s'ouvrir : le test s'abstient au lieu
//! d'échouer.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use acrux_app::platform::media::{Player, State};

/// Fréquence d'échantillonnage du fichier d'essai.
const RATE: u32 = 22_050;
/// Durée du fichier d'essai : assez pour voir la position avancer, assez peu
/// pour que la suite ne traîne pas.
const SECONDS: f64 = 0.4;

/// Écrit un WAV silencieux et rend son chemin.
fn silence() -> PathBuf {
    let frames = (f64::from(RATE) * SECONDS).round().max(0.0) as usize;
    let data = vec![0u8; frames * 2]; // 16 bits, une voie
    let mut wav = Vec::with_capacity(data.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&u32::try_from(36 + data.len()).unwrap_or(0).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
    wav.extend_from_slice(&data);
    let path = std::env::temp_dir().join("acrux-essai-silence.wav");
    std::fs::write(&path, &wav).unwrap_or_default();
    path
}

/// Ouvre le fichier d'essai, ou `None` si la machine n'a pas de son.
fn player() -> Option<Player> {
    Player::open(&silence()).ok()
}

#[test]
fn un_media_sans_image_souvre_et_annonce_sa_duree() {
    let Some(player) = player() else {
        return; // Aucun périphérique audio sur cette machine.
    };
    assert!(player.size().is_none(), "un son n'a pas de dimensions");
    let duree = player.duration();
    assert!(
        (duree - SECONDS).abs() < 0.1,
        "durée {duree} s, attendue {SECONDS} s"
    );
    assert_eq!(player.state(), State::Paused);
}

#[test]
fn la_lecture_dun_son_avance_puis_se_termine() {
    let Some(mut player) = player() else {
        return;
    };
    assert_eq!(player.state(), State::Paused);
    player.play();
    assert_eq!(player.state(), State::Playing);

    // Le lecteur est tiré par la boucle de dessin : sans appel, il ne fait
    // rien. On l'entretient comme le ferait l'application.
    let debut = Instant::now();
    let mut avance = false;
    while debut.elapsed() < Duration::from_secs(5) {
        assert!(player.frame().is_none(), "un son ne rend pas d'image");
        if player.position() > 0.05 {
            avance = true;
        }
        if player.state() == State::Ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(avance, "la position n'a pas bougé");
    assert_eq!(
        player.state(),
        State::Ended,
        "la lecture d'un son doit se terminer toute seule"
    );
    // Et la fin, c'est la fin : la position est celle de la durée.
    assert!((player.position() - player.duration()).abs() < 0.05);
}

#[test]
fn on_peut_se_deplacer_et_reprendre_un_son() {
    let Some(mut player) = player() else {
        return;
    };
    player.seek(SECONDS / 2.0);
    assert!((player.position() - SECONDS / 2.0).abs() < 0.05);
    // Reprendre après la fin repart du début (§ comportement d'un lecteur).
    player.seek(SECONDS);
    player.play();
    let debut = Instant::now();
    while debut.elapsed() < Duration::from_secs(5) && player.state() != State::Ended {
        let _ = player.frame();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(player.state(), State::Ended);
    player.play();
    assert_eq!(player.state(), State::Playing);
    assert!(player.position() < 0.1, "la reprise repart du début");
}
