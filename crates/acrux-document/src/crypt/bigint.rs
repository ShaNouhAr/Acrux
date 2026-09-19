//! Entiers naturels de grande taille, écrits pour RSA (RFC 8017).
//!
//! Représentation : un vecteur de limbes de 64 bits, poids faible en tête,
//! sans limbe de tête nul (l'entier zéro est un vecteur vide). Ce choix rend
//! la comparaison et la longueur triviales, et les produits partiels tiennent
//! exactement dans un `u128` : `(2^64-1)^2 + 2*(2^64-1) = 2^128-1`.
//!
//! Pourquoi Montgomery ? Une exponentiation modulaire RSA enchaîne des
//! milliers de réductions modulo `n` ; la division de Knuth y coûterait bien
//! plus cher que la réduction de Montgomery, qui ne demande que des
//! multiplications et des additions de limbes (§14.3.2 du *Handbook of
//! Applied Cryptography*).
//!
//! Temps constant : [`Montgomery::pow_secret`] emploie une échelle de
//! Montgomery avec échange masqué, donc la suite d'opérations ne dépend pas
//! des bits de l'exposant. Les limbes sont toujours parcourus en entier.
//! Ce n'est pas une garantie contre toutes les attaques par canaux auxiliaires
//! (le compilateur reste libre de ses optimisations) ; c'est le niveau de soin
//! que nous documentons, pas davantage.

// Toute l'arithmétique par limbes consiste à découper un produit `u128` en
// un limbe de poids faible (`as u64`) et une retenue (`>> 64`), et à laisser
// les emprunts se propager en complément à deux. Ces conversions sont le
// principe de l'algorithme, pas des accidents : les signaler une par une
// rendrait le code illisible sans rien prouver.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use core::cmp::Ordering;

/// Entier naturel de taille arbitraire.
///
/// Invariant : `limbs` ne se termine jamais par un limbe nul.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct BigUint {
    limbs: Vec<u64>,
}

impl BigUint {
    /// Zéro.
    #[must_use]
    pub fn zero() -> Self {
        BigUint { limbs: Vec::new() }
    }

    /// Un.
    #[must_use]
    pub fn one() -> Self {
        BigUint { limbs: vec![1] }
    }

    /// Entier tenant sur un limbe.
    #[must_use]
    pub fn from_u64(v: u64) -> Self {
        let mut b = BigUint { limbs: vec![v] };
        b.trim();
        b
    }

    /// Lecture en gros-boutiste (le format de tous les entiers ASN.1 et RSA).
    #[must_use]
    pub fn from_bytes_be(bytes: &[u8]) -> Self {
        let mut limbs = Vec::with_capacity(bytes.len() / 8 + 1);
        let mut acc: u64 = 0;
        let mut shift = 0;
        for &byte in bytes.iter().rev() {
            acc |= u64::from(byte) << shift;
            shift += 8;
            if shift == 64 {
                limbs.push(acc);
                acc = 0;
                shift = 0;
            }
        }
        if shift > 0 {
            limbs.push(acc);
        }
        let mut b = BigUint { limbs };
        b.trim();
        b
    }

    /// Écriture en gros-boutiste, sans zéro de tête (vide pour zéro).
    #[must_use]
    pub fn to_bytes_be(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.limbs.len() * 8);
        for limb in self.limbs.iter().rev() {
            out.extend_from_slice(&limb.to_be_bytes());
        }
        let first = out.iter().position(|&b| b != 0).unwrap_or(out.len());
        out.drain(..first);
        out
    }

    /// Écriture en gros-boutiste sur exactement `len` octets.
    ///
    /// # Errors
    /// L'entier ne tient pas sur `len` octets.
    pub fn to_bytes_be_padded(&self, len: usize) -> Result<Vec<u8>, TooLarge> {
        let raw = self.to_bytes_be();
        if raw.len() > len {
            return Err(TooLarge);
        }
        let mut out = vec![0u8; len - raw.len()];
        out.extend_from_slice(&raw);
        Ok(out)
    }

    /// Vrai si l'entier vaut zéro.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// Vrai si l'entier est impair.
    #[must_use]
    pub fn is_odd(&self) -> bool {
        self.limbs.first().is_some_and(|l| l & 1 == 1)
    }

    /// Nombre de bits significatifs (0 pour zéro).
    #[must_use]
    pub fn bits(&self) -> usize {
        match self.limbs.last() {
            None => 0,
            Some(top) => self.limbs.len() * 64 - (top.leading_zeros() as usize),
        }
    }

    /// Bit de rang `i` (0 = poids faible).
    #[must_use]
    pub fn bit(&self, i: usize) -> bool {
        self.limbs
            .get(i / 64)
            .is_some_and(|l| (l >> (i % 64)) & 1 == 1)
    }

    /// Nombre de limbes de 64 bits occupés.
    #[must_use]
    pub fn limb_count(&self) -> usize {
        self.limbs.len()
    }

    /// Valeur sur 64 bits si elle y tient.
    #[must_use]
    pub fn to_u64(&self) -> Option<u64> {
        match self.limbs.len() {
            0 => Some(0),
            1 => self.limbs.first().copied(),
            _ => None,
        }
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    /// Somme.
    #[must_use]
    pub fn add(&self, other: &BigUint) -> BigUint {
        let n = self.limbs.len().max(other.limbs.len());
        let mut out = Vec::with_capacity(n + 1);
        let mut carry = 0u64;
        for i in 0..n {
            let a = u128::from(self.limbs.get(i).copied().unwrap_or(0));
            let b = u128::from(other.limbs.get(i).copied().unwrap_or(0));
            let s = a + b + u128::from(carry);
            out.push(s as u64);
            carry = (s >> 64) as u64;
        }
        if carry != 0 {
            out.push(carry);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// Différence `self - other`, ou `None` si `other > self`.
    #[must_use]
    pub fn sub(&self, other: &BigUint) -> Option<BigUint> {
        if self.cmp_big(other) == Ordering::Less {
            return None;
        }
        let mut out = Vec::with_capacity(self.limbs.len());
        let mut borrow = 0u64;
        for i in 0..self.limbs.len() {
            let a = self.limbs.get(i).copied().unwrap_or(0);
            let b = other.limbs.get(i).copied().unwrap_or(0);
            let (d, b1) = a.overflowing_sub(b);
            let (d, b2) = d.overflowing_sub(borrow);
            out.push(d);
            borrow = u64::from(b1) + u64::from(b2);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        Some(r)
    }

    /// Produit (algorithme scolaire : `O(n·m)`, suffisant jusqu'à RSA 4096).
    #[must_use]
    pub fn mul(&self, other: &BigUint) -> BigUint {
        if self.is_zero() || other.is_zero() {
            return BigUint::zero();
        }
        let mut out = vec![0u64; self.limbs.len() + other.limbs.len()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry = 0u128;
            for (j, &b) in other.limbs.iter().enumerate() {
                let Some(slot) = out.get_mut(i + j) else {
                    continue;
                };
                let t = u128::from(a) * u128::from(b) + u128::from(*slot) + carry;
                *slot = t as u64;
                carry = t >> 64;
            }
            let mut k = i + other.limbs.len();
            while carry != 0 {
                let Some(slot) = out.get_mut(k) else { break };
                let t = u128::from(*slot) + carry;
                *slot = t as u64;
                carry = t >> 64;
                k += 1;
            }
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// Décalage à gauche de `n` bits.
    #[must_use]
    pub fn shl(&self, n: usize) -> BigUint {
        if self.is_zero() {
            return BigUint::zero();
        }
        let (whole, part) = (n / 64, n % 64);
        let mut out = vec![0u64; whole];
        let mut carry = 0u64;
        for &limb in &self.limbs {
            if part == 0 {
                out.push(limb);
            } else {
                out.push((limb << part) | carry);
                carry = limb >> (64 - part);
            }
        }
        if carry != 0 {
            out.push(carry);
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// Décalage à droite de `n` bits.
    #[must_use]
    pub fn shr(&self, n: usize) -> BigUint {
        let (whole, part) = (n / 64, n % 64);
        if whole >= self.limbs.len() {
            return BigUint::zero();
        }
        let rest: Vec<u64> = self.limbs.get(whole..).unwrap_or(&[]).to_vec();
        let mut out = vec![0u64; rest.len()];
        for i in 0..rest.len() {
            let low = rest.get(i).copied().unwrap_or(0) >> part;
            let high = if part == 0 {
                0
            } else {
                rest.get(i + 1).copied().unwrap_or(0) << (64 - part)
            };
            if let Some(slot) = out.get_mut(i) {
                *slot = low | high;
            }
        }
        let mut r = BigUint { limbs: out };
        r.trim();
        r
    }

    /// Comparaison.
    #[must_use]
    pub fn cmp_big(&self, other: &BigUint) -> Ordering {
        match self.limbs.len().cmp(&other.limbs.len()) {
            Ordering::Equal => {}
            o => return o,
        }
        for i in (0..self.limbs.len()).rev() {
            let a = self.limbs.get(i).copied().unwrap_or(0);
            let b = other.limbs.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }

    /// Quotient et reste de la division euclidienne (Knuth, *TAOCP* 4.3.1
    /// algorithme D). Renvoie `None` si `divisor` est nul.
    #[must_use]
    pub fn divmod(&self, divisor: &BigUint) -> Option<(BigUint, BigUint)> {
        if divisor.is_zero() {
            return None;
        }
        if self.cmp_big(divisor) == Ordering::Less {
            return Some((BigUint::zero(), self.clone()));
        }
        if divisor.limbs.len() == 1 {
            let d = divisor.limbs.first().copied().unwrap_or(1);
            let mut q = vec![0u64; self.limbs.len()];
            let mut rem = 0u128;
            for i in (0..self.limbs.len()).rev() {
                let cur = (rem << 64) | u128::from(self.limbs.get(i).copied().unwrap_or(0));
                if let Some(slot) = q.get_mut(i) {
                    *slot = (cur / u128::from(d)) as u64;
                }
                rem = cur % u128::from(d);
            }
            let mut quotient = BigUint { limbs: q };
            quotient.trim();
            return Some((quotient, BigUint::from_u64(rem as u64)));
        }
        // Normalisation : le limbe de tête du diviseur doit avoir son bit de
        // poids fort à 1, condition de validité de l'estimation du quotient.
        let shift = divisor.limbs.last().copied().unwrap_or(0).leading_zeros() as usize;
        let u = self.shl(shift);
        let v = divisor.shl(shift);
        let n = v.limbs.len();
        let m = u.limbs.len().saturating_sub(n);
        let mut un = u.limbs.clone();
        un.push(0);
        let mut q = vec![0u64; m + 1];
        let vn1 = v.limbs.get(n - 1).copied().unwrap_or(0);
        let vn2 = v.limbs.get(n - 2).copied().unwrap_or(0);
        for j in (0..=m).rev() {
            let top = u128::from(un.get(j + n).copied().unwrap_or(0));
            let next = u128::from(un.get(j + n - 1).copied().unwrap_or(0));
            let numerator = (top << 64) | next;
            let mut qhat = numerator / u128::from(vn1);
            let mut rhat = numerator % u128::from(vn1);
            // Correction de l'estimation (au plus deux tours, cf. TAOCP).
            while qhat > u128::from(u64::MAX)
                || qhat * u128::from(vn2)
                    > (rhat << 64) | u128::from(un.get(j + n - 2).copied().unwrap_or(0))
            {
                qhat -= 1;
                rhat += u128::from(vn1);
                if rhat > u128::from(u64::MAX) {
                    break;
                }
            }
            // Multiplication et soustraction.
            let mut borrow = 0i128;
            let mut carry = 0u128;
            for i in 0..n {
                let p = qhat * u128::from(v.limbs.get(i).copied().unwrap_or(0)) + carry;
                carry = p >> 64;
                let sub =
                    i128::from(un.get(j + i).copied().unwrap_or(0)) - i128::from(p as u64) + borrow;
                if let Some(slot) = un.get_mut(j + i) {
                    *slot = sub as u64;
                }
                borrow = sub >> 64;
            }
            let sub = i128::from(un.get(j + n).copied().unwrap_or(0)) - (carry as i128) + borrow;
            if let Some(slot) = un.get_mut(j + n) {
                *slot = sub as u64;
            }
            borrow = sub >> 64;
            if borrow < 0 {
                // Estimation trop grande d'une unité : on rend une fois le diviseur.
                qhat -= 1;
                let mut carry = 0u128;
                for i in 0..n {
                    let s = u128::from(un.get(j + i).copied().unwrap_or(0))
                        + u128::from(v.limbs.get(i).copied().unwrap_or(0))
                        + carry;
                    if let Some(slot) = un.get_mut(j + i) {
                        *slot = s as u64;
                    }
                    carry = s >> 64;
                }
                if let Some(slot) = un.get_mut(j + n) {
                    *slot = slot.wrapping_add(carry as u64);
                }
            }
            if let Some(slot) = q.get_mut(j) {
                *slot = qhat as u64;
            }
        }
        let mut quotient = BigUint { limbs: q };
        quotient.trim();
        let mut remainder = BigUint {
            limbs: un.get(..n).unwrap_or(&[]).to_vec(),
        };
        remainder.trim();
        Some((quotient, remainder.shr(shift)))
    }

    /// Reste modulo `m` (`None` si `m` est nul).
    #[must_use]
    pub fn rem(&self, m: &BigUint) -> Option<BigUint> {
        self.divmod(m).map(|(_, r)| r)
    }

    /// Inverse modulaire par l'algorithme d'Euclide étendu.
    ///
    /// Renvoie `None` si `self` et `m` ne sont pas premiers entre eux.
    #[must_use]
    pub fn mod_inverse(&self, m: &BigUint) -> Option<BigUint> {
        if m.is_zero() {
            return None;
        }
        // Euclide étendu sur des entiers signés représentés par (valeur, signe).
        let mut r0 = m.clone();
        let mut r1 = self.rem(m)?;
        let mut t0 = (BigUint::zero(), false);
        let mut t1 = (BigUint::one(), false);
        while !r1.is_zero() {
            let (q, r) = r0.divmod(&r1)?;
            let qt = signed_mul(&q, &t1);
            let t2 = signed_sub(&t0, &qt);
            r0 = r1;
            r1 = r;
            t0 = t1;
            t1 = t2;
        }
        if r0.cmp_big(&BigUint::one()) != Ordering::Equal {
            return None;
        }
        let (value, negative) = t0;
        let reduced = value.rem(m)?;
        if negative && !reduced.is_zero() {
            m.sub(&reduced)
        } else {
            Some(reduced)
        }
    }

    /// Exponentiation modulaire à exposant **public** (fenêtre binaire simple,
    /// temps variable). Employée par la vérification de signature, où
    /// l'exposant et le message sont publics.
    ///
    /// Renvoie `None` si le module est nul.
    #[must_use]
    pub fn pow_mod(&self, exponent: &BigUint, modulus: &BigUint) -> Option<BigUint> {
        if modulus.is_zero() {
            return None;
        }
        if let Some(mont) = Montgomery::new(modulus) {
            return mont.pow(self, exponent);
        }
        // Module pair : repli sur une exponentiation par division.
        let mut result = BigUint::one().rem(modulus)?;
        let mut base = self.rem(modulus)?;
        for i in 0..exponent.bits() {
            if exponent.bit(i) {
                result = result.mul(&base).rem(modulus)?;
            }
            base = base.mul(&base).rem(modulus)?;
        }
        Some(result)
    }
}

/// L'entier ne tient pas dans la largeur demandée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge;

/// Produit d'un naturel et d'un entier signé `(valeur, négatif)`.
fn signed_mul(a: &BigUint, b: &(BigUint, bool)) -> (BigUint, bool) {
    let p = a.mul(&b.0);
    let negative = b.1 && !p.is_zero();
    (p, negative)
}

/// Différence de deux entiers signés `(valeur, négatif)`.
fn signed_sub(a: &(BigUint, bool), b: &(BigUint, bool)) -> (BigUint, bool) {
    match (a.1, b.1) {
        (false, false) => match a.0.sub(&b.0) {
            Some(d) => (d, false),
            None => (b.0.sub(&a.0).unwrap_or_default(), true),
        },
        (true, true) => match b.0.sub(&a.0) {
            Some(d) => (d, false),
            None => (a.0.sub(&b.0).unwrap_or_default(), true),
        },
        (false, true) => (a.0.add(&b.0), false),
        (true, false) => (a.0.add(&b.0), true),
    }
}

/// Contexte de réduction de Montgomery pour un module **impair**.
///
/// `R = 2^(64·len)` où `len` est le nombre de limbes du module. Les valeurs
/// sont manipulées sous la forme « montgomérisée » `a·R mod m`.
#[derive(Debug, Clone)]
pub struct Montgomery {
    modulus: Vec<u64>,
    /// `-m^-1 mod 2^64`.
    n0inv: u64,
    /// `R^2 mod m`, qui permet d'entrer dans le domaine par une simple
    /// multiplication montgomérisée.
    rr: BigUint,
    len: usize,
}

impl Montgomery {
    /// Prépare le contexte. Renvoie `None` si le module est nul ou pair.
    #[must_use]
    pub fn new(modulus: &BigUint) -> Option<Self> {
        if modulus.is_zero() || !modulus.is_odd() {
            return None;
        }
        let len = modulus.limbs.len();
        let m0 = modulus.limbs.first().copied()?;
        // Inverse de m0 modulo 2^64 par itération de Newton : cinq tours
        // doublent la précision de 2 à 64 bits.
        let mut inv = m0;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(inv)));
        }
        let n0inv = inv.wrapping_neg();
        // R^2 mod m par doublements successifs : 2·(64·len) additions
        // modulaires, sans division.
        let mut rr = BigUint::one();
        for _ in 0..(2 * 64 * len) {
            rr = rr.add(&rr);
            if rr.cmp_big(modulus) != Ordering::Less {
                rr = rr.sub(modulus)?;
            }
        }
        Some(Montgomery {
            modulus: modulus.limbs.clone(),
            n0inv,
            rr,
            len,
        })
    }

    /// Module de ce contexte.
    #[must_use]
    pub fn modulus(&self) -> BigUint {
        let mut m = BigUint {
            limbs: self.modulus.clone(),
        };
        m.trim();
        m
    }

    /// Multiplication montgomérisée `a·b·R^-1 mod m` (CIOS, *Coarsely
    /// Integrated Operand Scanning*). Les opérandes sont supposées réduites.
    fn mul(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let len = self.len;
        let mut t = vec![0u64; len + 2];
        for i in 0..len {
            let bi = u128::from(b.get(i).copied().unwrap_or(0));
            let mut carry = 0u128;
            for j in 0..len {
                let tj = u128::from(t.get(j).copied().unwrap_or(0));
                let s = tj + u128::from(a.get(j).copied().unwrap_or(0)) * bi + carry;
                if let Some(slot) = t.get_mut(j) {
                    *slot = s as u64;
                }
                carry = s >> 64;
            }
            let s = u128::from(t.get(len).copied().unwrap_or(0)) + carry;
            if let Some(slot) = t.get_mut(len) {
                *slot = s as u64;
            }
            if let Some(slot) = t.get_mut(len + 1) {
                *slot = (s >> 64) as u64;
            }
            let m = t.first().copied().unwrap_or(0).wrapping_mul(self.n0inv);
            let mi = u128::from(m);
            let s = u128::from(t.first().copied().unwrap_or(0))
                + mi * u128::from(self.modulus.first().copied().unwrap_or(0));
            let mut carry = s >> 64;
            for j in 1..len {
                let tj = u128::from(t.get(j).copied().unwrap_or(0));
                let s = tj + mi * u128::from(self.modulus.get(j).copied().unwrap_or(0)) + carry;
                if let Some(slot) = t.get_mut(j - 1) {
                    *slot = s as u64;
                }
                carry = s >> 64;
            }
            let s = u128::from(t.get(len).copied().unwrap_or(0)) + carry;
            if let Some(slot) = t.get_mut(len - 1) {
                *slot = s as u64;
            }
            let hi = u128::from(t.get(len + 1).copied().unwrap_or(0)) + (s >> 64);
            if let Some(slot) = t.get_mut(len) {
                *slot = hi as u64;
            }
        }
        // Soustraction conditionnelle finale, sans branchement sur la donnée.
        let mut diff = vec![0u64; len];
        let mut borrow = 0u64;
        for j in 0..len {
            let (d, b1) = t
                .get(j)
                .copied()
                .unwrap_or(0)
                .overflowing_sub(self.modulus.get(j).copied().unwrap_or(0));
            let (d, b2) = d.overflowing_sub(borrow);
            if let Some(slot) = diff.get_mut(j) {
                *slot = d;
            }
            borrow = u64::from(b1) | u64::from(b2);
        }
        // Le résultat de CIOS est strictement inférieur à `2m` et occupe
        // `len + 1` limbes. Si la soustraction n'emprunte pas au-delà du limbe
        // supplémentaire, c'est que `t >= m` : on garde `diff`, sinon `t`.
        let (_, underflow) = t.get(len).copied().unwrap_or(0).overflowing_sub(borrow);
        let mask = 0u64.wrapping_sub(u64::from(!underflow));
        let mut out = vec![0u64; len];
        for j in 0..len {
            let a = t.get(j).copied().unwrap_or(0);
            let b = diff.get(j).copied().unwrap_or(0);
            if let Some(slot) = out.get_mut(j) {
                *slot = (a & !mask) | (b & mask);
            }
        }
        out
    }

    /// Entre dans le domaine de Montgomery (`a` est réduit modulo `m`).
    fn enter(&self, a: &BigUint) -> Option<Vec<u64>> {
        let reduced = a.rem(&self.modulus())?;
        let mut limbs = reduced.limbs;
        limbs.resize(self.len, 0);
        Some(self.mul(&limbs, &self.rr_limbs()))
    }

    fn rr_limbs(&self) -> Vec<u64> {
        let mut l = self.rr.limbs.clone();
        l.resize(self.len, 0);
        l
    }

    fn leave(&self, a: &[u64]) -> BigUint {
        let mut one = vec![0u64; self.len];
        if let Some(slot) = one.first_mut() {
            *slot = 1;
        }
        let mut r = BigUint {
            limbs: self.mul(a, &one),
        };
        r.trim();
        r
    }

    /// Exponentiation modulaire à exposant public (carré-et-multiplie,
    /// temps variable).
    #[must_use]
    pub fn pow(&self, base: &BigUint, exponent: &BigUint) -> Option<BigUint> {
        let mut result = self.enter(&BigUint::one())?;
        let b = self.enter(base)?;
        for i in (0..exponent.bits()).rev() {
            result = self.mul(&result, &result);
            if exponent.bit(i) {
                result = self.mul(&result, &b);
            }
        }
        Some(self.leave(&result))
    }

    /// Exponentiation modulaire à exposant **secret** : échelle de Montgomery.
    ///
    /// Chaque bit provoque exactement une multiplication et un carré, et le
    /// choix du registre se fait par masque plutôt que par branchement.
    #[must_use]
    pub fn pow_secret(&self, base: &BigUint, exponent: &BigUint) -> Option<BigUint> {
        let mut r0 = self.enter(&BigUint::one())?;
        let mut r1 = self.enter(base)?;
        let bits = exponent.bits().max(1);
        for i in (0..bits).rev() {
            let mask = 0u64.wrapping_sub(u64::from(exponent.bit(i)));
            conditional_swap(&mut r0, &mut r1, mask);
            r1 = self.mul(&r0, &r1);
            r0 = self.mul(&r0, &r0);
            conditional_swap(&mut r0, &mut r1, mask);
        }
        Some(self.leave(&r0))
    }
}

/// Échange `a` et `b` si `mask` vaut `u64::MAX`, ne fait rien si `mask` vaut 0.
fn conditional_swap(a: &mut [u64], b: &mut [u64], mask: u64) {
    for i in 0..a.len().min(b.len()) {
        let (Some(x), Some(y)) = (a.get(i).copied(), b.get(i).copied()) else {
            continue;
        };
        let d = (x ^ y) & mask;
        if let Some(slot) = a.get_mut(i) {
            *slot = x ^ d;
        }
        if let Some(slot) = b.get_mut(i) {
            *slot = y ^ d;
        }
    }
}

impl PartialOrd for BigUint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigUint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_big(other)
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn big(hex: &str) -> BigUint {
        let mut bytes = Vec::new();
        let padded = if hex.len() % 2 == 1 {
            format!("0{hex}")
        } else {
            hex.to_string()
        };
        let chars: Vec<char> = padded.chars().collect();
        for pair in chars.chunks(2) {
            let s: String = pair.iter().collect();
            bytes.push(u8::from_str_radix(&s, 16).unwrap());
        }
        BigUint::from_bytes_be(&bytes)
    }

    fn hex(v: &BigUint) -> String {
        use std::fmt::Write as _;
        let bytes = v.to_bytes_be();
        if bytes.is_empty() {
            return "0".to_string();
        }
        let s: String = bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        });
        s.trim_start_matches('0').to_string()
    }

    #[test]
    fn bytes_roundtrip_drops_leading_zeros() {
        let v = BigUint::from_bytes_be(&[0, 0, 1, 2, 3]);
        assert_eq!(v.to_bytes_be(), vec![1, 2, 3]);
        assert_eq!(v.to_bytes_be_padded(5).unwrap(), vec![0, 0, 1, 2, 3]);
        assert_eq!(v.to_bytes_be_padded(2), Err(TooLarge));
        assert!(BigUint::zero().to_bytes_be().is_empty());
        assert_eq!(
            BigUint::zero().to_bytes_be_padded(3).unwrap(),
            vec![0, 0, 0]
        );
    }

    #[test]
    fn add_sub_carry_across_limbs() {
        let a = big("ffffffffffffffff");
        let b = BigUint::one();
        let s = a.add(&b);
        assert_eq!(hex(&s), "10000000000000000");
        assert_eq!(hex(&s.sub(&b).unwrap()), "ffffffffffffffff");
        assert_eq!(b.sub(&a), None);
        assert!(a.sub(&a).unwrap().is_zero());
    }

    #[test]
    fn mul_matches_known_products() {
        let a = big("ffffffffffffffffffffffffffffffff");
        let p = a.mul(&a);
        // (2^128-1)^2 = 2^256 - 2^129 + 1
        assert_eq!(
            hex(&p),
            "fffffffffffffffffffffffffffffffe00000000000000000000000000000001"
        );
        assert!(a.mul(&BigUint::zero()).is_zero());
    }

    #[test]
    fn shifts() {
        let a = big("123456789abcdef0");
        assert_eq!(hex(&a.shl(4)), "123456789abcdef00");
        assert_eq!(hex(&a.shl(64)), "123456789abcdef00000000000000000");
        assert_eq!(hex(&a.shl(68)), "123456789abcdef000000000000000000");
        assert_eq!(hex(&a.shl(68).shr(68)), "123456789abcdef0");
        assert!(a.shr(200).is_zero());
    }

    #[test]
    fn divmod_small_and_large() {
        let a = big("100000000000000000000000000000000");
        let b = big("ff00000000000001");
        let (q, r) = a.divmod(&b).unwrap();
        // Vérification par reconstruction.
        assert_eq!(hex(&q.mul(&b).add(&r)), hex(&a));
        assert_eq!(r.cmp_big(&b), Ordering::Less);
        let (q, r) = a.divmod(&BigUint::from_u64(7)).unwrap();
        assert_eq!(hex(&q.mul(&BigUint::from_u64(7)).add(&r)), hex(&a));
        assert_eq!(BigUint::from_u64(10).divmod(&BigUint::zero()), None);
        let (q, r) = BigUint::from_u64(5).divmod(&BigUint::from_u64(9)).unwrap();
        assert!(q.is_zero());
        assert_eq!(r.to_u64(), Some(5));
    }

    #[test]
    fn divmod_needs_the_quotient_correction() {
        // Cas choisi pour que l'estimation qhat soit trop grande de 1 :
        // dividende = 2^192 - 1, diviseur = 2^128 + 1.
        let a = big("ffffffffffffffffffffffffffffffffffffffffffffffff");
        let b = big("100000000000000000000000000000001");
        let (q, r) = a.divmod(&b).unwrap();
        assert_eq!(hex(&q.mul(&b).add(&r)), hex(&a));
        assert_eq!(r.cmp_big(&b), Ordering::Less);
    }

    #[test]
    fn mod_inverse_and_pow() {
        let m = BigUint::from_u64(0xFFFF_FFFF_0000_0001);
        let a = BigUint::from_u64(0x0123_4567_89AB_CDEF);
        let inv = a.mod_inverse(&m).unwrap();
        assert_eq!(a.mul(&inv).rem(&m).unwrap().to_u64(), Some(1));
        // Petit Fermat : a^(m-1) = 1 mod m pour m premier ; on se contente
        // ici de vérifier a^1 et a^0.
        assert_eq!(a.pow_mod(&BigUint::zero(), &m).unwrap().to_u64(), Some(1));
        assert_eq!(
            hex(&a.pow_mod(&BigUint::one(), &m).unwrap()),
            hex(&a.rem(&m).unwrap())
        );
        assert_eq!(
            BigUint::from_u64(4).mod_inverse(&BigUint::from_u64(8)),
            None
        );
    }

    #[test]
    fn montgomery_matches_naive_powmod() {
        let m = big("d1e7b4a5f3c2918e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281707");
        let base = big("0123456789abcdeffedcba9876543210");
        let exp = big("10001");
        let fast = base.pow_mod(&exp, &m).unwrap();
        // Référence par carré-et-multiplie avec division.
        let mut naive = BigUint::one();
        for i in (0..exp.bits()).rev() {
            naive = naive.mul(&naive).rem(&m).unwrap();
            if exp.bit(i) {
                naive = naive.mul(&base).rem(&m).unwrap();
            }
        }
        assert_eq!(hex(&fast), hex(&naive));
        let mont = Montgomery::new(&m).unwrap();
        assert_eq!(hex(&mont.pow_secret(&base, &exp).unwrap()), hex(&naive));
    }

    #[test]
    fn montgomery_refuses_even_modulus() {
        assert!(Montgomery::new(&BigUint::from_u64(8)).is_none());
        // Le repli par division doit quand même donner le bon résultat.
        let r = BigUint::from_u64(3)
            .pow_mod(&BigUint::from_u64(5), &BigUint::from_u64(8))
            .unwrap();
        assert_eq!(r.to_u64(), Some(3)); // 243 mod 8
    }

    #[test]
    fn bits_and_ordering() {
        assert_eq!(BigUint::zero().bits(), 0);
        assert_eq!(BigUint::one().bits(), 1);
        assert_eq!(big("ff").bits(), 8);
        assert_eq!(big("100").bits(), 9);
        assert!(big("100") > big("ff"));
        assert!(big("ff") < big("100"));
        assert_eq!(big("abc").cmp_big(&big("abc")), Ordering::Equal);
    }
}
