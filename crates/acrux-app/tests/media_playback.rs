//! Lecture d'un vrai fichier vidéo, de bout en bout.
//!
//! Le test a besoin d'un média : il le prend dans `ACRUX_MEDIA_TEST` et
//! s'abstient sans lui. Aucun fichier vidéo n'est versionné — un dépôt de
//! moteur PDF n'a pas à peser un mégaoctet de plus pour cela — et rien n'est
//! téléchargé pendant les tests.
//!
//! Ce qui est vérifié :
//!
//! - le média s'ouvre, annonce ses dimensions et sa durée ;
//! - les images décodées ont la bonne taille et **ne sont pas noires** ;
//! - elles se suivent dans le temps ;
//! - un déplacement dans le média rend bien une image de cet endroit-là ;
//! - deux endroits différents donnent deux images différentes, ce qui prouve
//!   que le déplacement fait quelque chose.

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use acrux_app::platform::media::{Player, State};

/// Fichier de test, ou `None` s'il n'y en a pas.
fn media() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("ACRUX_MEDIA_TEST")?);
    path.is_file().then_some(path)
}

/// Luminance moyenne d'une image, pour distinguer du noir d'une vraie image.
fn brightness(bgra: &[u8]) -> f64 {
    if bgra.is_empty() {
        return 0.0;
    }
    let sum: u64 = bgra
        .chunks_exact(4)
        .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
        .sum();
    #[allow(clippy::cast_precision_loss)] // quelques millions de pixels
    let moyenne = sum as f64 / (bgra.len() / 4) as f64 / 3.0;
    moyenne
}

#[test]
fn une_video_se_decode_en_images() {
    let Some(path) = media() else {
        eprintln!("ACRUX_MEDIA_TEST non défini : test ignoré");
        return;
    };
    let mut player = Player::open(&path).expect("ouverture du média");
    let (width, height) = player.size().expect("dimensions de la vidéo");
    assert!(width >= 16 && height >= 16, "{width}×{height}");
    assert!(player.duration() > 0.1, "durée {}", player.duration());
    assert_eq!(
        player.state(),
        State::Paused,
        "à l'arrêt tant qu'on ne joue pas"
    );

    // Première image, sans même démarrer : c'est l'affiche du média.
    let frame = player.frame().expect("une première image");
    assert_eq!(frame.width, width);
    assert_eq!(frame.height, height);
    assert_eq!(frame.bgra.len(), width as usize * height as usize * 4);
    let clair = brightness(&frame.bgra);
    assert!(clair > 4.0, "image entièrement noire (luminance {clair})");
}

#[test]
fn les_images_avancent_dans_le_temps() {
    let Some(path) = media() else {
        eprintln!("ACRUX_MEDIA_TEST non défini : test ignoré");
        return;
    };
    let mut player = Player::open(&path).expect("ouverture du média");
    player.play();
    assert_eq!(player.state(), State::Playing);

    let mut instants = Vec::new();
    let debut = std::time::Instant::now();
    // Une seconde de lecture, en tirant les images comme le ferait la boucle
    // de dessin.
    while debut.elapsed().as_secs_f64() < 1.0 {
        if let Some(frame) = player.frame() {
            let t = frame.time;
            if instants.last() != Some(&t) {
                instants.push(t);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    player.pause();

    assert!(
        instants.len() >= 5,
        "trop peu d'images en une seconde : {}",
        instants.len()
    );
    assert!(
        instants.windows(2).all(|w| w[1] >= w[0]),
        "les instants doivent croître : {instants:?}"
    );
    let parcouru = instants.last().unwrap() - instants.first().unwrap();
    assert!(
        parcouru > 0.3 && parcouru < 3.0,
        "une seconde de lecture a parcouru {parcouru:.2} s de média"
    );
    assert!(player.position() > 0.2, "position {}", player.position());
}

#[test]
fn se_deplacer_change_limage() {
    let Some(path) = media() else {
        eprintln!("ACRUX_MEDIA_TEST non défini : test ignoré");
        return;
    };
    let mut player = Player::open(&path).expect("ouverture du média");
    let duration = player.duration();
    if duration < 1.0 {
        eprintln!("média trop court pour éprouver le déplacement : test ignoré");
        return;
    }

    let prendre = |player: &mut Player, at: f64| -> (Vec<u8>, f64) {
        player.seek(at);
        // La première image après un déplacement peut demander quelques
        // décodages : le point clé précède l'instant visé.
        for _ in 0..200 {
            if let Some(frame) = player.frame() {
                if frame.time + 0.5 >= at {
                    return (frame.bgra.clone(), frame.time);
                }
            }
        }
        panic!("aucune image à {at} s");
    };

    let (debut, t0) = prendre(&mut player, duration * 0.1);
    let (fin, t1) = prendre(&mut player, duration * 0.8);
    assert!(t1 > t0, "les instants doivent différer : {t0} puis {t1}");
    assert_eq!(debut.len(), fin.len());
    let differents = debut
        .iter()
        .zip(fin.iter())
        .filter(|(a, b)| a.abs_diff(**b) > 8)
        .count();
    assert!(
        differents * 20 > debut.len(),
        "les deux images se ressemblent trop : {differents} composantes sur {}",
        debut.len()
    );
    // Revenir au début rend une image du début.
    let (retour, t2) = prendre(&mut player, 0.0);
    assert!(t2 < t1, "retour à {t2} après {t1}");
    assert_eq!(retour.len(), debut.len());
}

#[test]
fn la_pause_arrete_le_temps() {
    let Some(path) = media() else {
        eprintln!("ACRUX_MEDIA_TEST non défini : test ignoré");
        return;
    };
    let mut player = Player::open(&path).expect("ouverture du média");
    player.play();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let _ = player.frame();
    player.pause();
    let arret = player.position();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let toujours = player.position();
    assert!(
        (toujours - arret).abs() < 0.01,
        "la position a bougé en pause : {arret} puis {toujours}"
    );
    player.play();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        player.position() > arret + 0.1,
        "la reprise n'avance pas : {} contre {arret}",
        player.position()
    );
}

/// Écrit une image du média dans un PNG, pour la regarder.
///
/// Ignoré par défaut : c'est un outil de mise au point, pas une vérification.
///
/// ```text
/// $env:ACRUX_MEDIA_TEST = "C:\chemin\video.mp4"
/// cargo test -p acrux-app --test media_playback -- --ignored voir_une_image
/// ```
#[test]
#[ignore = "outil de mise au point : écrit un fichier"]
fn voir_une_image() {
    let Some(path) = media() else {
        eprintln!("ACRUX_MEDIA_TEST non défini");
        return;
    };
    let mut player = Player::open(&path).expect("ouverture");
    let at = player.duration() * 0.35;
    player.seek(at);
    let mut image = None;
    for _ in 0..300 {
        if let Some(frame) = player.frame() {
            if frame.time + 0.5 >= at {
                image = Some((frame.width, frame.height, frame.bgra.clone(), frame.time));
                break;
            }
        }
    }
    let (width, height, bgra, time) = image.expect("une image");
    // BGRA → RGB, puis un PNG écrit par notre propre compresseur.
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for p in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[p[2], p[1], p[0]]);
    }
    let mut raw = Vec::new();
    for y in 0..height as usize {
        raw.push(0);
        raw.extend_from_slice(&rgb[y * width as usize * 3..(y + 1) * width as usize * 3]);
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let chunk = |kind: &[u8; 4], body: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = kind.to_vec();
        crc_input.extend_from_slice(body);
        let mut crc = 0xFFFF_FFFFu32;
        for byte in &crc_input {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr, &mut png);
    chunk(b"IDAT", &acrux_codecs::flate::compress(&raw, 6), &mut png);
    chunk(b"IEND", &[], &mut png);

    let out = std::env::var_os("ACRUX_MEDIA_OUT").map_or_else(
        || std::env::temp_dir().join("acrux-image-video.png"),
        PathBuf::from,
    );
    std::fs::write(&out, &png).expect("écriture");
    println!(
        "image à {time:.2} s écrite : {} ({width}×{height})",
        out.display()
    );
}
