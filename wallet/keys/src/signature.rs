//!
//! Signature scheme primitives shared by wallet crates.
//!

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum SignatureScheme {
    ECDSA,
    Schnorr,
}

impl SignatureScheme {
    pub fn is_ecdsa(self) -> bool {
        matches!(self, Self::ECDSA)
    }

    pub fn is_schnorr(self) -> bool {
        matches!(self, Self::Schnorr)
    }
}
