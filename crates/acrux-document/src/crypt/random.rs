//! Aléa cryptographique : clés de fichier, sels, IV et identifiants.
//!
//! Le générateur est ChaCha20 (RFC 8439 §2.3) employé en flux de clé : on
//! chiffre un compteur, et ce qui sort est indiscernable du hasard tant que
//! la clé reste secrète. Toute la sécurité tient donc dans la **graine** :
//! 32 octets que personne ne peut deviner.
//!
//! Jusqu'à la 0.22, la clé AES-256 d'un document protégé sortait d'un
//! xorshift semé par « longueur du fichier ^ horloge » : 64 bits d'état,
//! dont la plupart se retrouvent en connaissant la date d'enregistrement.
//! Qui devinait la graine retrouvait la clé, sans toucher au mot de passe.
//!
//! La graine vient ici de l'entropie que la bibliothèque standard tire du
//! système d'exploitation **sans code `unsafe`** : les clés SipHash de
//! [`RandomState`]. std les demande au système (ProcessPrng sous Windows)
//! une seule fois par fil, puis se contente d'incrémenter l'une d'elles à
//! chaque `RandomState::new` : pour recueillir plusieurs tirages, il faut
//! donc plusieurs fils neufs. On y mêle l'horloge, le PID, les adresses
//! (ASLR) et la gigue du lancement des fils — ce qui ne remplace pas le
//! système, mais évite le pire si std changeait un jour de source. Une
//! source du système peut enfin être branchée par [`set_os_source`] (la
//! couche plate-forme de l'application, seule autorisée à appeler l'API
//! Windows).
//!
//! Les tests, eux, veulent un aléa **reproductible** : [`ChaCha20Rng`] se
//! sème à la main, et tout ce qui consomme de l'aléa prend un
//! `&mut dyn Random` injectable.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::sha2::sha256;

/// Source d'octets aléatoires.
pub trait Random {
    /// Remplit `out` d'octets aléatoires.
    fn fill(&mut self, out: &mut [u8]);

    /// Entier de 64 bits aléatoire.
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill(&mut b);
        u64::from_le_bytes(b)
    }

    /// Vecteur d'initialisation AES (16 octets).
    fn iv(&mut self) -> [u8; 16] {
        let mut b = [0u8; 16];
        self.fill(&mut b);
        b
    }
}

/// Constantes « expand 32-byte k » (RFC 8439 §2.3).
const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// Quart de tour (RFC 8439 §2.1) : additions modulo 2³², ou exclusif et
/// rotations — rien qui puisse déborder.
fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// Bloc ChaCha20 (RFC 8439 §2.3) : 64 octets de flux de clé pour une clé, un
/// compteur de bloc et un nonce.
#[must_use]
pub fn chacha20_block(key: &[u32; 8], counter: u32, nonce: &[u32; 3]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[..4].copy_from_slice(&SIGMA);
    state[4..12].copy_from_slice(key);
    state[12] = counter;
    state[13..].copy_from_slice(nonce);
    let mut w = state;
    // Vingt tours : dix fois une colonne puis une diagonale.
    for _ in 0..10 {
        quarter(&mut w, 0, 4, 8, 12);
        quarter(&mut w, 1, 5, 9, 13);
        quarter(&mut w, 2, 6, 10, 14);
        quarter(&mut w, 3, 7, 11, 15);
        quarter(&mut w, 0, 5, 10, 15);
        quarter(&mut w, 1, 6, 11, 12);
        quarter(&mut w, 2, 7, 8, 13);
        quarter(&mut w, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for ((chunk, mixed), initial) in out.chunks_exact_mut(4).zip(w).zip(state) {
        chunk.copy_from_slice(&mixed.wrapping_add(initial).to_le_bytes());
    }
    out
}

/// Mots de 32 bits petit-boutistes d'une clé de 32 octets.
#[must_use]
pub fn key_words(bytes: &[u8; 32]) -> [u32; 8] {
    let mut words = [0u32; 8];
    for (word, b) in words.iter_mut().zip(bytes.chunks_exact(4)) {
        *word = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    }
    words
}

/// Générateur ChaCha20 à graine explicite : le même flux pour la même
/// graine. C'est le générateur **injectable** des tests, et le moteur de
/// [`SystemRandom`], qui ne fait que lui fournir une graine imprévisible.
#[derive(Clone)]
pub struct ChaCha20Rng {
    key: [u32; 8],
    nonce: [u32; 3],
    counter: u32,
    buf: [u8; 64],
    pos: usize,
}

impl std::fmt::Debug for ChaCha20Rng {
    /// L'état est un secret : il ne s'imprime pas, même dans un journal.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChaCha20Rng { .. }")
    }
}

impl ChaCha20Rng {
    /// Générateur semé par 32 octets (la clé ChaCha20), nonce et compteur à
    /// zéro.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            key: key_words(&seed),
            nonce: [0; 3],
            counter: 0,
            buf: [0; 64],
            pos: 64,
        }
    }

    /// Calcule le bloc suivant. Au bout des 2³² blocs d'un nonce (256 Gio),
    /// on passe au nonce suivant : le flux continue sans jamais repasser par
    /// un bloc déjà servi.
    fn refill(&mut self) {
        self.buf = chacha20_block(&self.key, self.counter, &self.nonce);
        self.pos = 0;
        if let Some(next) = self.counter.checked_add(1) {
            self.counter = next;
        } else {
            self.counter = 0;
            for word in &mut self.nonce {
                *word = word.wrapping_add(1);
                if *word != 0 {
                    break;
                }
            }
        }
    }
}

impl Random for ChaCha20Rng {
    fn fill(&mut self, out: &mut [u8]) {
        let mut written = 0;
        while written < out.len() {
            if self.pos >= self.buf.len() {
                self.refill();
            }
            let n = (self.buf.len() - self.pos).min(out.len() - written);
            out[written..written + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            // Ce qui est servi ne reste pas dans le tampon.
            self.buf[self.pos..self.pos + n].fill(0);
            self.pos += n;
            written += n;
        }
    }
}

/// Aléa de production : un [`ChaCha20Rng`] semé depuis la réserve du
/// processus, elle-même semée une fois par l'entropie du système.
///
/// Chaque générateur a sa propre graine, et la réserve change de clé à
/// chaque tirage (« effacement rapide de clé ») : connaître l'état présent
/// ne rend pas les graines déjà servies.
#[derive(Debug, Clone)]
pub struct SystemRandom(ChaCha20Rng);

/// Réserve du processus, semée au premier besoin.
static POOL: Mutex<Option<ChaCha20Rng>> = Mutex::new(None);

/// Compteur mêlé à chaque graine : deux générateurs créés dans la même
/// nanoseconde n'ont pas la même.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Source d'entropie du système branchée par l'application, s'il y en a une.
static OS_SOURCE: OnceLock<fn(&mut [u8]) -> bool> = OnceLock::new();

/// Branche une source d'entropie du système (par exemple `BCryptGenRandom`
/// sous Windows) : ses octets sont mêlés à la réserve et à chaque graine.
/// Elle rend faux si elle n'a rien pu fournir. Rend faux si une source était
/// déjà branchée (la première reste).
pub fn set_os_source(source: fn(&mut [u8]) -> bool) -> bool {
    OS_SOURCE.set(source).is_ok()
}

impl Default for SystemRandom {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemRandom {
    /// Nouveau générateur. Le premier appel du processus sème la réserve
    /// (quelques fils lancés et rejoints, une fraction de milliseconde) ;
    /// les suivants ne coûtent qu'un verrou et deux blocs ChaCha20.
    #[must_use]
    pub fn new() -> Self {
        let mut child = [0u8; 32];
        {
            let mut guard = POOL.lock().unwrap_or_else(PoisonError::into_inner);
            let pool = guard.get_or_insert_with(|| ChaCha20Rng::from_seed(gather_entropy()));
            pool.fill(&mut child);
            let mut next = [0u8; 32];
            pool.fill(&mut next);
            *pool = ChaCha20Rng::from_seed(next);
        }
        let mut input = Vec::with_capacity(96);
        input.extend_from_slice(&child);
        input.extend_from_slice(&nanos_since_epoch().to_le_bytes());
        input.extend_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        input.extend_from_slice(&os_bytes());
        let seed = sha256(&input);
        input.fill(0);
        SystemRandom(ChaCha20Rng::from_seed(seed))
    }
}

impl Random for SystemRandom {
    fn fill(&mut self, out: &mut [u8]) {
        self.0.fill(out);
    }
}

/// Nanosecondes depuis 1970 (0 si l'horloge est avant l'époque).
fn nanos_since_epoch() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// 32 octets de la source du système branchée, ou rien.
fn os_bytes() -> Vec<u8> {
    let Some(source) = OS_SOURCE.get() else {
        return Vec::new();
    };
    let mut buf = vec![0u8; 32];
    if source(&mut buf) {
        buf
    } else {
        Vec::new()
    }
}

/// Deux mots SipHash sous des clés neuves : sur un fil qui vient de naître,
/// std vient de les tirer du système.
fn thread_entropy() -> [u64; 2] {
    let s = RandomState::new();
    [s.hash_one(0xA5u64), s.hash_one(0x5Au64)]
}

/// Graine de la réserve : un SHA-256 de tout ce que le processus sait tirer
/// d'imprévisible sans appel système direct.
fn gather_entropy() -> [u8; 32] {
    let mut input: Vec<u8> = Vec::with_capacity(512);
    let start = Instant::now();
    // Quatre fils neufs, quatre tirages du système. `Builder::spawn` et non
    // `thread::spawn`, qui panique si le fil ne peut pas naître : un fil
    // manquant se passe, un plantage non.
    let mut handles = Vec::with_capacity(4);
    for _ in 0..4 {
        let spawned = std::thread::Builder::new()
            .name("acrux-entropie".into())
            .spawn(thread_entropy);
        if let Ok(handle) = spawned {
            handles.push(handle);
        }
        input.extend_from_slice(&start.elapsed().as_nanos().to_le_bytes());
    }
    for word in thread_entropy() {
        input.extend_from_slice(&word.to_le_bytes());
    }
    for handle in handles {
        if let Ok(words) = handle.join() {
            for word in words {
                input.extend_from_slice(&word.to_le_bytes());
            }
        }
        // La durée de chaque attente dépend de l'ordonnanceur : quelques
        // bits de gigue de plus.
        input.extend_from_slice(&start.elapsed().as_nanos().to_le_bytes());
    }
    input.extend_from_slice(&nanos_since_epoch().to_le_bytes());
    input.extend_from_slice(format!("{:?}", Instant::now()).as_bytes());
    input.extend_from_slice(&std::process::id().to_le_bytes());
    input.extend_from_slice(format!("{:?}", std::thread::current().id()).as_bytes());
    // Adresses d'une variable de pile, d'un tas et d'un statique :
    // l'ASLR les déplace d'un lancement à l'autre.
    let local = 0u8;
    let boxed = Box::new(0u64);
    for address in [
        std::ptr::from_ref(&local).addr(),
        std::ptr::from_ref(&*boxed).addr(),
        std::ptr::from_ref(&COUNTER).addr(),
    ] {
        input.extend_from_slice(&address.to_le_bytes());
    }
    input.extend_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    input.extend_from_slice(&os_bytes());
    let seed = sha256(&input);
    input.fill(0);
    seed
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// RFC 8439 §2.3.2 : clé 00..1f, nonce 00:00:00:09:00:00:00:4a:00:00:00:00,
    /// compteur 1.
    #[test]
    fn chacha20_rfc8439_block() {
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        let nonce = [0x0900_0000, 0x4a00_0000, 0];
        let block = chacha20_block(&key_words(&key), 1, &nonce);
        let expected = hex("10 f1 e7 e4 d1 3b 59 15 50 0f dd 1f a3 20 71 c4 \
             c7 d1 f4 c7 33 c0 68 03 04 22 aa 9a c3 d4 6c 4e \
             d2 82 64 46 07 9f aa 09 14 c2 d7 05 d9 8b 02 a2 \
             b5 12 9c d1 de 16 4e b9 cb d0 83 e8 a2 50 3c 4e");
        assert_eq!(block.to_vec(), expected);
    }

    /// RFC 8439 annexe A.1, vecteurs 1 et 2 : clé et nonce nuls, compteurs 0
    /// puis 1. Le générateur semé à zéro doit servir exactement ce flux, bloc
    /// après bloc, quelle que soit la taille des tirages.
    #[test]
    fn chacha20_rfc8439_keystream() {
        let expected = hex("76 b8 e0 ad a0 f1 3d 90 40 5d 6a e5 53 86 bd 28 \
             bd d2 19 b8 a0 8d ed 1a a8 36 ef cc 8b 77 0d c7 \
             da 41 59 7c 51 57 48 8d 77 24 e0 3f b8 d8 4a 37 \
             6a 43 b8 f4 15 18 a1 1c c3 87 b6 69 b2 ee 65 86 \
             9f 07 e7 be 55 51 38 7a 98 ba 97 7c 73 2d 08 0d \
             cb 0f 29 a0 48 e3 65 69 12 c6 53 3e 32 ee 7a ed \
             29 b7 21 76 9c e6 4e 43 d5 71 33 b0 74 d8 39 d5 \
             31 ed 1f 28 51 0a fb 45 ac e1 0a 1f 4b 79 4d 6f");
        let mut rng = ChaCha20Rng::from_seed([0; 32]);
        let mut got = Vec::new();
        // Des tirages de tailles inégales, qui chevauchent les blocs.
        for n in [1usize, 15, 16, 30, 2, 64] {
            let mut part = vec![0u8; n];
            rng.fill(&mut part);
            got.extend_from_slice(&part);
        }
        assert_eq!(got, expected);
        // Annexe A.1, vecteur 5 : nonce …02, compteur 0.
        let block = chacha20_block(&[0; 8], 0, &[0, 0, 0x0200_0000]);
        assert_eq!(block[..8], hex("c2 c6 4d 37 8c d5 36 37")[..]);
    }

    #[test]
    fn seeded_is_deterministic() {
        let (mut a, mut b) = (
            ChaCha20Rng::from_seed([7; 32]),
            ChaCha20Rng::from_seed([7; 32]),
        );
        let (mut x, mut y) = (vec![0u8; 1000], vec![0u8; 1000]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
        let mut c = ChaCha20Rng::from_seed([8; 32]);
        let mut z = vec![0u8; 1000];
        c.fill(&mut z);
        assert_ne!(x, z);
    }

    #[test]
    fn system_random_differs() {
        let (mut a, mut b) = (SystemRandom::new(), SystemRandom::new());
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        a.fill(&mut x);
        b.fill(&mut y);
        assert_ne!(x, y);
        let mut seen = HashSet::new();
        for _ in 0..10_000 {
            assert!(seen.insert(a.iv()), "IV répété");
        }
    }

    /// Répartition des octets sur 1 Mio : chaque valeur est attendue 4096
    /// fois ; un générateur biaisé sortirait nettement des bornes.
    #[test]
    fn system_random_is_balanced() {
        let mut rng = SystemRandom::new();
        let mut data = vec![0u8; 1 << 20];
        rng.fill(&mut data);
        let mut counts = [0u32; 256];
        for &b in &data {
            counts[usize::from(b)] += 1;
        }
        for (value, &n) in counts.iter().enumerate() {
            assert!((3400..=4800).contains(&n), "octet {value} : {n} fois");
        }
        // χ² à 255 degrés de liberté : autour de 255, au-delà de 400 le
        // hasard est très improbable.
        let chi2: f64 = counts
            .iter()
            .map(|&n| (f64::from(n) - 4096.0).powi(2) / 4096.0)
            .sum();
        assert!(chi2 < 400.0, "χ² = {chi2}");
    }

    /// Sentinelle : si std cessait de tirer des clés neuves pour chaque fil,
    /// deux fils donneraient les mêmes mots et ce test tomberait.
    #[test]
    fn entropy_from_threads_differs() {
        let a = std::thread::Builder::new()
            .spawn(thread_entropy)
            .unwrap()
            .join()
            .unwrap();
        let b = std::thread::Builder::new()
            .spawn(thread_entropy)
            .unwrap()
            .join()
            .unwrap();
        assert_ne!(a, b);
        assert_ne!(gather_entropy(), gather_entropy());
    }

    #[test]
    fn counter_wraps_without_panic() {
        let mut rng = ChaCha20Rng::from_seed([1; 32]);
        rng.counter = u32::MAX;
        let mut out = [0u8; 256];
        rng.fill(&mut out);
        assert_eq!(rng.nonce[0], 1);
        assert!(out.iter().any(|&b| b != 0));
        // Le nonce qui déborde à son tour reporte sur le mot suivant.
        rng.nonce = [u32::MAX, 0, 0];
        rng.counter = u32::MAX;
        rng.pos = 64;
        rng.fill(&mut out);
        assert_eq!(rng.nonce, [0, 1, 0]);
    }
}
