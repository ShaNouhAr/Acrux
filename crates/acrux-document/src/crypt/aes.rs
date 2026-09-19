//! AES (FIPS 197) en mode CBC, tel qu'employé par les PDF
//! (ISO 32000-2 §7.6.3.3 : AESV2 = AES-128, AESV3 = AES-256), plus AES-ECB
//! sans remplissage pour l'algorithme 2.B de la révision 6.
//!
//! Implémentation directe des tables S-box, sans optimisation par tables
//! combinées : la lisibilité prime (voir CHARTE_PROJET.md).

#![allow(clippy::many_single_char_names, clippy::too_many_lines)] // notation des normes FIPS / RFC

use acrux_core::{Error, Result};

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

fn inv_sbox() -> [u8; 256] {
    let mut inv = [0u8; 256];
    for (i, &v) in (0u8..=255).zip(SBOX.iter()) {
        inv[usize::from(v)] = i;
    }
    inv
}

/// Multiplication dans GF(2^8) avec le polynôme x^8 + x^4 + x^3 + x + 1.
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// Clé étendue pour AES-128 (10 tours), AES-192 (12) ou AES-256 (14).
pub struct Aes {
    round_keys: Vec<[u8; 16]>,
    inv: [u8; 256],
}

impl Aes {
    /// Prépare une clé de 16, 24 ou 32 octets.
    ///
    /// # Errors
    /// Longueur de clé invalide.
    pub fn new(key: &[u8]) -> Result<Self> {
        let nk = match key.len() {
            16 => 4,
            24 => 6,
            32 => 8,
            n => {
                return Err(Error::Corrupt(format!(
                    "longueur de clé AES invalide : {n}"
                )))
            }
        };
        let rounds = nk + 6;
        let total_words = 4 * (rounds + 1);
        let mut w: Vec<[u8; 4]> = Vec::with_capacity(total_words);
        for i in 0..nk {
            w.push([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
        }
        let mut rcon = 1u8;
        for i in nk..total_words {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [
                    SBOX[usize::from(t[1])] ^ rcon,
                    SBOX[usize::from(t[2])],
                    SBOX[usize::from(t[3])],
                    SBOX[usize::from(t[0])],
                ];
                rcon = gmul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                t = [
                    SBOX[usize::from(t[0])],
                    SBOX[usize::from(t[1])],
                    SBOX[usize::from(t[2])],
                    SBOX[usize::from(t[3])],
                ];
            }
            let p = w[i - nk];
            w.push([p[0] ^ t[0], p[1] ^ t[1], p[2] ^ t[2], p[3] ^ t[3]]);
        }
        let round_keys = (0..=rounds)
            .map(|r| {
                let mut k = [0u8; 16];
                for c in 0..4 {
                    k[c * 4..c * 4 + 4].copy_from_slice(&w[r * 4 + c]);
                }
                k
            })
            .collect();
        Ok(Self {
            round_keys,
            inv: inv_sbox(),
        })
    }

    fn add_round_key(state: &mut [u8; 16], k: &[u8; 16]) {
        for (s, k) in state.iter_mut().zip(k) {
            *s ^= k;
        }
    }

    /// Chiffre un bloc de 16 octets.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let rounds = self.round_keys.len() - 1;
        Self::add_round_key(block, &self.round_keys[0]);
        for r in 1..=rounds {
            for b in block.iter_mut() {
                *b = SBOX[usize::from(*b)];
            }
            // ShiftRows : l'état est stocké colonne par colonne (index = col*4 + row).
            let s = *block;
            for row in 1..4 {
                for col in 0..4 {
                    block[col * 4 + row] = s[((col + row) % 4) * 4 + row];
                }
            }
            if r != rounds {
                for col in 0..4 {
                    let c = &mut block[col * 4..col * 4 + 4];
                    let (a0, a1, a2, a3) = (c[0], c[1], c[2], c[3]);
                    c[0] = gmul(a0, 2) ^ gmul(a1, 3) ^ a2 ^ a3;
                    c[1] = a0 ^ gmul(a1, 2) ^ gmul(a2, 3) ^ a3;
                    c[2] = a0 ^ a1 ^ gmul(a2, 2) ^ gmul(a3, 3);
                    c[3] = gmul(a0, 3) ^ a1 ^ a2 ^ gmul(a3, 2);
                }
            }
            Self::add_round_key(block, &self.round_keys[r]);
        }
    }

    /// Déchiffre un bloc de 16 octets.
    pub fn decrypt_block(&self, block: &mut [u8; 16]) {
        let rounds = self.round_keys.len() - 1;
        Self::add_round_key(block, &self.round_keys[rounds]);
        for r in (0..rounds).rev() {
            // InvShiftRows
            let s = *block;
            for row in 1..4 {
                for col in 0..4 {
                    block[((col + row) % 4) * 4 + row] = s[col * 4 + row];
                }
            }
            for b in block.iter_mut() {
                *b = self.inv[usize::from(*b)];
            }
            Self::add_round_key(block, &self.round_keys[r]);
            if r != 0 {
                for col in 0..4 {
                    let c = &mut block[col * 4..col * 4 + 4];
                    let (a0, a1, a2, a3) = (c[0], c[1], c[2], c[3]);
                    c[0] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
                    c[1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
                    c[2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
                    c[3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
                }
            }
        }
    }

    /// Déchiffrement CBC : les 16 premiers octets sont l'IV, remplissage PKCS#5
    /// retiré (§7.6.3.3). Tolérant : bloc final incomplet ignoré, remplissage
    /// invalide conservé (comportement d'Acrobat sur les fichiers abîmés).
    #[must_use]
    pub fn cbc_decrypt(&self, data: &[u8]) -> Vec<u8> {
        if data.len() < 32 {
            return Vec::new();
        }
        let mut prev = [0u8; 16];
        prev.copy_from_slice(&data[..16]);
        let mut out = Vec::with_capacity(data.len() - 16);
        for chunk in data[16..].chunks_exact(16) {
            let mut block = [0u8; 16];
            block.copy_from_slice(chunk);
            let cipher = block;
            self.decrypt_block(&mut block);
            for (b, p) in block.iter_mut().zip(&prev) {
                *b ^= p;
            }
            out.extend_from_slice(&block);
            prev = cipher;
        }
        if let Some(&pad) = out.last() {
            let pad = usize::from(pad);
            if (1..=16).contains(&pad)
                && pad <= out.len()
                && out[out.len() - pad..]
                    .iter()
                    .all(|&b| usize::from(b) == pad)
            {
                out.truncate(out.len() - pad);
            }
        }
        out
    }

    /// Chiffrement CBC avec IV donné et remplissage PKCS#5.
    #[must_use]
    pub fn cbc_encrypt(&self, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
        let pad = 16 - data.len() % 16;
        let mut padded = data.to_vec();
        padded.extend(std::iter::repeat_n(u8::try_from(pad).unwrap_or(16), pad));
        let mut out = Vec::with_capacity(16 + padded.len());
        out.extend_from_slice(iv);
        let mut prev = *iv;
        for chunk in padded.chunks_exact(16) {
            let mut block = [0u8; 16];
            for (i, b) in block.iter_mut().enumerate() {
                *b = chunk[i] ^ prev[i];
            }
            self.encrypt_block(&mut block);
            out.extend_from_slice(&block);
            prev = block;
        }
        out
    }

    /// Chiffrement CBC **sans** remplissage ni IV en sortie (algorithme 2.B, §7.6.4.3.4).
    /// La longueur des données doit être un multiple de 16.
    #[must_use]
    pub fn cbc_encrypt_no_padding(&self, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut prev = *iv;
        for chunk in data.chunks_exact(16) {
            let mut block = [0u8; 16];
            for (i, b) in block.iter_mut().enumerate() {
                *b = chunk[i] ^ prev[i];
            }
            self.encrypt_block(&mut block);
            out.extend_from_slice(&block);
            prev = block;
        }
        out
    }

    /// Déchiffrement CBC sans remplissage avec IV explicite (validation des
    /// clés de la révision 6 : `/UE`, `/OE`, `/Perms`).
    #[must_use]
    pub fn cbc_decrypt_no_padding(&self, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut prev = *iv;
        for chunk in data.chunks_exact(16) {
            let mut block = [0u8; 16];
            block.copy_from_slice(chunk);
            let cipher = block;
            self.decrypt_block(&mut block);
            for (b, p) in block.iter_mut().zip(&prev) {
                *b ^= p;
            }
            out.extend_from_slice(&block);
            prev = cipher;
        }
        out
    }

    /// Déchiffrement ECB d'un bloc unique (`/Perms`, §7.6.4.4.12).
    #[must_use]
    pub fn ecb_decrypt_block(&self, data: &[u8; 16]) -> [u8; 16] {
        let mut b = *data;
        self.decrypt_block(&mut b);
        b
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    fn h(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn fips_197_vectors() {
        // AES-128
        let aes = Aes::new(&h("000102030405060708090a0b0c0d0e0f")).unwrap();
        let mut b = [0u8; 16];
        b.copy_from_slice(&h("00112233445566778899aabbccddeeff"));
        aes.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), h("69c4e0d86a7b0430d8cdb78070b4c55a"));
        aes.decrypt_block(&mut b);
        assert_eq!(b.to_vec(), h("00112233445566778899aabbccddeeff"));
        // AES-192
        let aes = Aes::new(&h("000102030405060708090a0b0c0d0e0f1011121314151617")).unwrap();
        b.copy_from_slice(&h("00112233445566778899aabbccddeeff"));
        aes.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), h("dda97ca4864cdfe06eaf70a0ec0d7191"));
        // AES-256
        let aes = Aes::new(&h(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        ))
        .unwrap();
        b.copy_from_slice(&h("00112233445566778899aabbccddeeff"));
        aes.encrypt_block(&mut b);
        assert_eq!(b.to_vec(), h("8ea2b7ca516745bfeafc49904b496089"));
        aes.decrypt_block(&mut b);
        assert_eq!(b.to_vec(), h("00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn cbc_roundtrip_with_padding() {
        let aes = Aes::new(&[7u8; 16]).unwrap();
        let iv = [3u8; 16];
        for len in [0usize, 1, 15, 16, 17, 40] {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let c = aes.cbc_encrypt(&iv, &data);
            assert_eq!(c.len(), 16 + (len / 16 + 1) * 16);
            assert_eq!(aes.cbc_decrypt(&c), data);
        }
    }

    #[test]
    fn cbc_no_padding_roundtrip() {
        let aes = Aes::new(&[1u8; 32]).unwrap();
        let iv = [0u8; 16];
        let data = [9u8; 48];
        let c = aes.cbc_encrypt_no_padding(&iv, &data);
        assert_eq!(aes.cbc_decrypt_no_padding(&iv, &c), data);
    }

    #[test]
    fn bad_key_length() {
        assert!(Aes::new(&[0u8; 5]).is_err());
    }
}
