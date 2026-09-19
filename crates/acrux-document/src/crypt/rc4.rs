//! RC4 (algorithme ARCFOUR), chiffrement historique des PDF
//! (ISO 32000-2 §7.6.3, versions 1 et 2 du dictionnaire `/Encrypt`).

/// Chiffre ou déchiffre `data` avec `key` (RC4 est symétrique).
#[must_use]
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s = [0u8; 256];
    for (v, i) in s.iter_mut().zip(0u8..=255) {
        *v = i;
    }
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0u8, 0u8);
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[usize::from(i)]);
        s.swap(usize::from(i), usize::from(j));
        let k = s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))];
        out.push(b ^ k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // Vecteurs classiques (Wikipedia / RFC 6229).
        assert_eq!(
            rc4(b"Key", b"Plaintext"),
            [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
        assert_eq!(rc4(b"Wiki", b"pedia"), [0x10, 0x21, 0xBF, 0x04, 0x20]);
        assert_eq!(
            rc4(b"Secret", b"Attack at dawn"),
            [0x45, 0xA0, 0x1F, 0x64, 0x5F, 0xC3, 0x5B, 0x38, 0x35, 0x52, 0x54, 0x4B, 0x9B, 0xF5]
        );
    }

    #[test]
    fn symmetric() {
        let c = rc4(b"k", b"hello");
        assert_eq!(rc4(b"k", &c), b"hello");
    }
}
