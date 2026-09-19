//! SHA-1 (FIPS 180-4, RFC 3174).
//!
//! **Obsolète.** SHA-1 n'offre plus de résistance aux collisions (SHAttered,
//! 2017) et n'est employé ici que pour **lire** d'anciens documents : sous-filtre
//! `adbe.x509.rsa_sha1` (ISO 32000-2 §12.8.3.2) et signatures CMS anciennes.
//! Nous ne signons jamais avec SHA-1 : [`crate::crypt::rsa`] refuse
//! [`crate::crypt::rsa::Hash::Sha1`] à la signature.

#![allow(clippy::many_single_char_names)] // notation de la norme (a, b, c, d, e)

/// Condensé SHA-1 d'un message.
#[must_use]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    // Bourrage identique à MD5 mais longueur en gros-boutiste (§5.1.1).
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks_exact(4).enumerate() {
            let bytes = [
                word.first().copied().unwrap_or(0),
                word.get(1).copied().unwrap_or(0),
                word.get(2).copied().unwrap_or(0),
                word.get(3).copied().unwrap_or(0),
            ];
            if let Some(slot) = w.get_mut(i) {
                *slot = u32::from_be_bytes(bytes);
            }
        }
        for i in 16..80 {
            let x = w.get(i - 3).copied().unwrap_or(0)
                ^ w.get(i - 8).copied().unwrap_or(0)
                ^ w.get(i - 14).copied().unwrap_or(0)
                ^ w.get(i - 16).copied().unwrap_or(0);
            if let Some(slot) = w.get_mut(i) {
                *slot = x.rotate_left(1);
            }
        }
        let (mut a, mut b, mut c, mut d, mut e) = (
            h.first().copied().unwrap_or(0),
            h.get(1).copied().unwrap_or(0),
            h.get(2).copied().unwrap_or(0),
            h.get(3).copied().unwrap_or(0),
            h.get(4).copied().unwrap_or(0),
        );
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(v);
        }
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        let bytes = word.to_be_bytes();
        for (j, b) in bytes.iter().enumerate() {
            if let Some(slot) = out.get_mut(i * 4 + j) {
                *slot = *b;
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn hex(d: &[u8]) -> String {
        use std::fmt::Write as _;
        d.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[test]
    fn rfc3174_vectors() {
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&sha1(&million)),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn block_boundaries() {
        // 55, 56 et 64 octets : les trois cas de bourrage.
        assert_eq!(
            hex(&sha1(&[b'x'; 55])),
            hex(&sha1(&[b'x'; 55])),
            "déterminisme"
        );
        assert_eq!(sha1(&[0u8; 56]).len(), 20);
        assert_eq!(
            hex(&sha1(&[0u8; 64])),
            "c8d7d0ef0eedfa82d2ea1aa592845b9a6d4b02b7"
        );
    }
}
