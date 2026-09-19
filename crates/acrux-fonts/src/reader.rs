//! Lecteur d'octets grand-boutiste (big-endian) borné.
//!
//! Toutes les tables de polices (TrueType/OpenType, CFF, en-têtes PFB…)
//! sont écrites en big-endian. Ce lecteur ne panique jamais : chaque lecture
//! hors des limites renvoie `None`, ce qui permet aux parseurs de traiter une
//! police tronquée ou hostile comme une donnée manquante et non comme une
//! erreur fatale (CHARTE_PROJET.md §1.4).
//!
//! Types de la spécification OpenType (« Data types ») pris en charge :
//! uint8, int8, uint16, int16, uint24, uint32, int32, Fixed (16.16),
//! F2Dot14 (2.14), Offset16/Offset32 et Tag.

/// Curseur de lecture sur une tranche d'octets.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Lecteur positionné au début de `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Lecteur positionné à l'octet `pos` (ou en fin de données si `pos`
    /// dépasse la longueur : les lectures suivantes échoueront proprement).
    #[must_use]
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos: pos.min(data.len()),
        }
    }

    /// Données sous-jacentes.
    #[must_use]
    pub const fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Position courante.
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Nombre d'octets restant à lire.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Vrai si tout a été lu.
    #[must_use]
    pub const fn is_eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    /// Déplace le curseur ; `None` si la position dépasse la fin.
    pub fn seek(&mut self, pos: usize) -> Option<()> {
        if pos > self.data.len() {
            return None;
        }
        self.pos = pos;
        Some(())
    }

    /// Avance de `n` octets ; `None` si cela dépasse la fin.
    pub fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        self.seek(end)
    }

    /// Lit `n` octets.
    pub fn read_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    /// Tableau de taille fixe.
    fn read_array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let bytes = self.read_bytes(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Some(out)
    }

    /// uint8.
    pub fn read_u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    /// int8.
    pub fn read_i8(&mut self) -> Option<i8> {
        self.read_u8().map(|b| i8::from_ne_bytes([b]))
    }

    /// uint16.
    pub fn read_u16(&mut self) -> Option<u16> {
        self.read_array::<2>().map(u16::from_be_bytes)
    }

    /// int16.
    pub fn read_i16(&mut self) -> Option<i16> {
        self.read_array::<2>().map(i16::from_be_bytes)
    }

    /// uint24 (utilisé par les offsets CFF de taille 3).
    pub fn read_u24(&mut self) -> Option<u32> {
        let b = self.read_array::<3>()?;
        Some(u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }

    /// uint32.
    pub fn read_u32(&mut self) -> Option<u32> {
        self.read_array::<4>().map(u32::from_be_bytes)
    }

    /// int32.
    pub fn read_i32(&mut self) -> Option<i32> {
        self.read_array::<4>().map(i32::from_be_bytes)
    }

    /// Nombre non signé sur `size` octets (1 à 4), comme les offsets CFF
    /// (TN #5176 §4, type `OffSize`).
    pub fn read_offset(&mut self, size: u8) -> Option<u32> {
        match size {
            1 => self.read_u8().map(u32::from),
            2 => self.read_u16().map(u32::from),
            3 => self.read_u24(),
            4 => self.read_u32(),
            _ => None,
        }
    }

    /// Fixed 16.16 signé.
    pub fn read_fixed(&mut self) -> Option<f64> {
        self.read_i32().map(|v| f64::from(v) / 65536.0)
    }

    /// F2Dot14 : 2 bits entiers signés, 14 bits fractionnaires.
    pub fn read_f2dot14(&mut self) -> Option<f64> {
        self.read_i16().map(|v| f64::from(v) / 16384.0)
    }

    /// Tag de quatre octets (`'head'`, `'OTTO'`…).
    pub fn read_tag(&mut self) -> Option<[u8; 4]> {
        self.read_array::<4>()
    }

    /// Sous-lecteur sur la tranche `[offset, offset + len)`, tronquée à la
    /// fin des données si nécessaire (les polices incorporées dans les PDF
    /// sont souvent coupées avant la fin d'une table) ; `None` si l'offset
    /// est lui-même hors des données.
    #[must_use]
    pub fn sub(&self, offset: usize, len: usize) -> Option<Reader<'a>> {
        let end = offset.checked_add(len)?.min(self.data.len());
        let slice = self.data.get(offset..end)?;
        Some(Reader::new(slice))
    }
}

/// uint16 à la position `pos` d'une tranche.
#[must_use]
pub fn u16_at(data: &[u8], pos: usize) -> Option<u16> {
    Reader::at(data, pos).read_u16()
}

/// int16 à la position `pos` d'une tranche.
#[must_use]
pub fn i16_at(data: &[u8], pos: usize) -> Option<i16> {
    Reader::at(data, pos).read_i16()
}

/// uint32 à la position `pos` d'une tranche.
#[must_use]
pub fn u32_at(data: &[u8], pos: usize) -> Option<u32> {
    Reader::at(data, pos).read_u32()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn reads_all_types() {
        let data = [
            0x01, 0xFF, 0x12, 0x34, 0xFF, 0xFE, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x10, 0xFF,
            0xFF, 0xFF, 0xFF, 0x00, 0x01, 0x80, 0x00, 0x40, 0x00, b'h', b'e', b'a', b'd',
        ];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_u8(), Some(1));
        assert_eq!(r.read_i8(), Some(-1));
        assert_eq!(r.read_u16(), Some(0x1234));
        assert_eq!(r.read_i16(), Some(-2));
        assert_eq!(r.read_u24(), Some(0x0000_0102));
        assert_eq!(r.read_u32(), Some(16));
        assert_eq!(r.read_i32(), Some(-1));
        assert_eq!(r.read_fixed(), Some(1.5));
        assert_eq!(r.read_f2dot14(), Some(1.0));
        assert_eq!(r.read_tag(), Some(*b"head"));
        assert!(r.is_eof());
        assert_eq!(r.read_u8(), None);
    }

    #[test]
    fn bounded_reads_never_panic() {
        let data = [1u8, 2, 3];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_u32(), None);
        assert_eq!(r.pos(), 0);
        assert_eq!(r.read_bytes(4), None);
        assert_eq!(r.skip(4), None);
        assert_eq!(r.seek(3), Some(()));
        assert!(r.is_eof());
        assert_eq!(r.read_offset(5), None);
        assert!(Reader::at(&data, 99).is_eof());
        assert!(r.sub(5, 1).is_none());
        assert_eq!(r.sub(1, 100).map(|s| s.remaining()), Some(2));
        assert_eq!(u16_at(&data, 2), None);
        assert_eq!(u32_at(&data, 0), None);
        assert_eq!(i16_at(&data, 0), Some(0x0102));
    }

    #[test]
    fn offsets_of_every_size() {
        let data = [0x01, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04];
        let mut r = Reader::new(&data);
        assert_eq!(r.read_offset(1), Some(1));
        assert_eq!(r.read_offset(2), Some(2));
        assert_eq!(r.read_offset(3), Some(3));
        assert_eq!(r.read_offset(4), Some(4));
    }
}
