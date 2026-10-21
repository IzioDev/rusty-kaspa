//! KCC-3 P2PK origins. Legacy BIP32 derivation is deliberately independent.

use crate::{error::Error, result::Result};
use borsh::{BorshDeserialize, BorshSerialize};
use kaspa_bip32::{ChildNumber, DerivationPath, ExtendedPrivateKey, ExtendedPublicKey, KeyFingerprint};
use kaspa_consensus_core::network::NetworkType;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

pub const MAX_INDEX: u32 = (1 << 31) - 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignatureScheme {
    #[default]
    Schnorr,
    Ecdsa,
}

/// A validated purpose-44 account origin. Deserialization always validates it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountDerivation {
    network: NetworkType,
    signature_scheme: SignatureScheme,
    account_index: u32,
}

impl AccountDerivation {
    pub fn try_new(network: NetworkType, signature_scheme: SignatureScheme, account_index: u32) -> Result<Self> {
        ChildNumber::new(account_index, true)?;
        Ok(Self { network, signature_scheme, account_index })
    }

    pub fn network(&self) -> NetworkType {
        self.network
    }
    pub fn signature_scheme(&self) -> SignatureScheme {
        self.signature_scheme
    }
    pub fn account_index(&self) -> u32 {
        self.account_index
    }

    pub fn account_path(&self) -> Result<DerivationPath> {
        let coin = match self.network {
            NetworkType::Mainnet => 111111,
            NetworkType::Testnet => 111112,
            NetworkType::Simnet => 111113,
            NetworkType::Devnet => 111114,
        };
        Ok(format!("m/44'/{coin}'/{}'/{}'", self.signature_scheme as u32, self.account_index).parse()?)
    }

    pub fn address_path(&self, change: u32, index: u32) -> Result<DerivationPath> {
        if change > 1 {
            return Err(Error::custom("KCC-3 change must be 0 or 1"));
        }
        ChildNumber::new(index, false)?;
        Ok(format!("{}/{change}/{index}", self.account_path()?).parse()?)
    }

    pub fn from_address_path(path: &DerivationPath) -> Result<(Self, u32, u32)> {
        let parts = path.iter().collect::<Vec<_>>();
        if parts.len() != 6 || parts[..4].iter().any(|p| !p.is_hardened()) || parts[4..].iter().any(|p| p.is_hardened()) {
            return Err(Error::custom("Invalid KCC-3 address path depth or hardening"));
        }
        let origin = Self::try_from(parts[..4].iter().fold("m".to_string(), |s, p| format!("{s}/{p}")))?;
        origin.address_path(parts[4].index(), parts[5].index())?;
        Ok((origin, parts[4].index(), parts[5].index()))
    }

    pub fn derive_account(
        &self,
        master: &ExtendedPrivateKey<secp256k1::SecretKey>,
    ) -> Result<ExtendedPrivateKey<secp256k1::SecretKey>> {
        if master.attrs().depth != 0 {
            return Err(Error::custom("KCC-3 derivation requires a master private key"));
        }
        self.account_path()?.iter().try_fold(master.clone(), |key, child| {
            key.derive_child(child).map_err(|err| Self::child_error(err, key.attrs().depth + 1, child.index()))
        })
    }

    fn child_error(error: kaspa_bip32::Error, depth: u8, index: u32) -> Error {
        match error {
            kaspa_bip32::Error::ScalarOutOfRangeError(_)
            | kaspa_bip32::Error::Crypto(
                secp256k1::Error::InvalidTweak | secp256k1::Error::InvalidSecretKey | secp256k1::Error::InvalidPublicKey,
            ) => Error::InvalidChild { depth, index },
            error => error.into(),
        }
    }

    pub fn derive_private_key(
        &self,
        master: &ExtendedPrivateKey<secp256k1::SecretKey>,
        change: u32,
        index: u32,
    ) -> Result<secp256k1::SecretKey> {
        self.address_path(change, index)?;
        let chain = self
            .derive_account(master)?
            .derive_child(ChildNumber::new(change, false)?)
            .map_err(|error| Self::child_error(error, 5, change))?;
        Ok(*chain.derive_child(ChildNumber::new(index, false)?).map_err(|error| Self::child_error(error, 6, index))?.private_key())
    }
}

impl TryFrom<String> for AccountDerivation {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        let path = DerivationPath::from_str(&value)?;
        let parts = path.iter().collect::<Vec<_>>();
        if parts.len() != 4 || parts.iter().any(|p| !p.is_hardened()) || parts[0].index() != 44 {
            return Err(Error::custom("Invalid KCC-3 P2PK account origin"));
        }
        let network = match parts[1].index() {
            111111 => NetworkType::Mainnet,
            111112 => NetworkType::Testnet,
            111113 => NetworkType::Simnet,
            111114 => NetworkType::Devnet,
            _ => return Err(Error::custom("Unassigned KCC-3 coin type")),
        };
        let scheme = match parts[2].index() {
            0 => SignatureScheme::Schnorr,
            1 => SignatureScheme::Ecdsa,
            _ => return Err(Error::custom("Unassigned KCC-3 signature scheme")),
        };
        Self::try_new(network, scheme, parts[3].index())
    }
}

impl BorshDeserialize for AccountDerivation {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        Self::try_new(
            BorshDeserialize::deserialize_reader(reader)?,
            BorshDeserialize::deserialize_reader(reader)?,
            BorshDeserialize::deserialize_reader(reader)?,
        )
        .map_err(std::io::Error::other)
    }
}

impl From<AccountDerivation> for String {
    fn from(value: AccountDerivation) -> Self {
        // All fields are validated and private; formatting cannot fail.
        let coin = match value.network {
            NetworkType::Mainnet => 111111,
            NetworkType::Testnet => 111112,
            NetworkType::Simnet => 111113,
            NetworkType::Devnet => 111114,
        };
        format!("m/44'/{coin}'/{}'/{}'", value.signature_scheme as u32, value.account_index)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize)]
#[serde(try_from = "PublicKeyDescription", into = "PublicKeyDescription")]
pub struct AccountPublicKey {
    derivation: AccountDerivation,
    master_fingerprint: Option<KeyFingerprint>,
    xpub: ExtendedPublicKey<secp256k1::PublicKey>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublicKeyDescription {
    origin: AccountDerivation,
    master_fingerprint: Option<KeyFingerprint>,
    xpub: ExtendedPublicKey<secp256k1::PublicKey>,
}

impl AccountPublicKey {
    pub fn try_new(
        derivation: AccountDerivation,
        master_fingerprint: Option<KeyFingerprint>,
        xpub: ExtendedPublicKey<secp256k1::PublicKey>,
    ) -> Result<Self> {
        if xpub.attrs().depth != 4 || xpub.attrs().child_number != ChildNumber::new(derivation.account_index, true)? {
            return Err(Error::custom("KCC-3 account xpub depth/child does not match its origin"));
        }
        Ok(Self { derivation, master_fingerprint, xpub })
    }
    pub fn derivation(&self) -> &AccountDerivation {
        &self.derivation
    }
    pub fn xpub(&self) -> &ExtendedPublicKey<secp256k1::PublicKey> {
        &self.xpub
    }
    pub fn master_fingerprint(&self) -> Option<KeyFingerprint> {
        self.master_fingerprint
    }
    pub fn derive_public_key(&self, change: u32, index: u32) -> Result<ExtendedPublicKey<secp256k1::PublicKey>> {
        self.derivation.address_path(change, index)?;
        let chain = self
            .xpub
            .derive_child(ChildNumber::new(change, false)?)
            .map_err(|error| AccountDerivation::child_error(error, 5, change))?;
        chain.derive_child(ChildNumber::new(index, false)?).map_err(|error| AccountDerivation::child_error(error, 6, index))
    }
}

impl TryFrom<PublicKeyDescription> for AccountPublicKey {
    type Error = Error;
    fn try_from(value: PublicKeyDescription) -> Result<Self> {
        Self::try_new(value.origin, value.master_fingerprint, value.xpub)
    }
}
impl From<AccountPublicKey> for PublicKeyDescription {
    fn from(value: AccountPublicKey) -> Self {
        Self { origin: value.derivation, master_fingerprint: value.master_fingerprint, xpub: value.xpub }
    }
}
impl BorshDeserialize for AccountPublicKey {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        Self::try_new(
            BorshDeserialize::deserialize_reader(reader)?,
            BorshDeserialize::deserialize_reader(reader)?,
            BorshDeserialize::deserialize_reader(reader)?,
        )
        .map_err(std::io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_children_are_rejected() -> Result<()> {
        let master = ExtendedPrivateKey::<secp256k1::SecretKey>::new([11; 32])?;
        // Overflow and zero/infinity child results must fail at every depth.
        for tweak in [[255; 32], master.private_key().negate().secret_bytes()] {
            let private_error = kaspa_bip32::PrivateKey::derive_child(master.private_key(), tweak).unwrap_err();
            let public_error = kaspa_bip32::PublicKey::derive_child(master.public_key().public_key(), tweak).unwrap_err();
            for error in [private_error, public_error] {
                for depth in 1..=6 {
                    assert!(
                        matches!(AccountDerivation::child_error(error.clone(), depth, 7), Error::InvalidChild { depth: actual, index: 7 } if actual == depth)
                    );
                }
            }
        }
        let origin = AccountDerivation::try_new(NetworkType::Mainnet, SignatureScheme::Schnorr, 7)?;
        let private = origin.derive_account(&master)?;
        assert_eq!(origin.account_index(), 7);
        assert_eq!(private.attrs().child_number, ChildNumber::new(7, true)?);
        let account = AccountPublicKey::try_new(origin.clone(), None, private.public_key())?;
        let public = account.derive_public_key(1, 7)?;
        assert_eq!(public.attrs().child_number, ChildNumber::new(7, false)?);
        assert_eq!(public.to_bytes(), origin.derive_private_key(&master, 1, 7)?.public_key(secp256k1::SECP256K1).serialize());
        Ok(())
    }

    #[test]
    fn p2pk_conformance_vectors() -> Result<()> {
        // Pinned KCC-0003 conformance.json SHA-256:
        // c938b3449f8c6782c194785b7300c5ddb7862b8bfd94e57d8605c80e647be793.
        let master = ExtendedPrivateKey::<secp256k1::SecretKey>::new((0u8..16).collect::<Vec<_>>())?;
        for (path, public, chain_code) in [
            (
                "m/44'/111111'/0'/0'/0/0",
                "023bd098775f14349db55c9544b6ef5e9813c662655616c3caf5152f1ef32247a7",
                "c28c9fc45765cdf6b168aeebeac81808e25adcb894ebbeaac2c4281fa25c2bcb",
            ),
            (
                "m/44'/111111'/1'/0'/0/0",
                "0258b57f2ec05ac730a4509a40f881083fa03722e9a25616ed3367c23330242021",
                "d9c0616cba37444f966d10b1a5c2cac85cae5693ad2bcd770e346c3ce22e43ad",
            ),
            (
                "m/44'/111112'/0'/0'/0/0",
                "024fc9e8d8feb491c9a5e568d0aaa823406597f64672a0f632d59a81efd0bd61e6",
                "18e432bc583abadbdcb467c45041cb994b0a7a09ec062cdec62e48a736f7ce96",
            ),
            (
                "m/44'/111113'/0'/2'/1/7",
                "0388f52c8e1a5b8a7fb200d614b8b8d3523745af5c9899a67c647e84da51a3afcf",
                "a02848e8c664310fe41ec290e2933d86c5ef9f4315c095b517d8f3f6638d95dc",
            ),
            (
                "m/44'/111114'/1'/0'/0/1",
                "0342e69f16ea0cb98cd1304dd1c52a7e58ef546084388831da14db15c7a8aa9d03",
                "7b09de9039bc626829bd629f62db1f6c5a1dd0f426bc7fde8bb534f7865b6d2b",
            ),
        ] {
            let (origin, change, index) = AccountDerivation::from_address_path(&path.parse()?)?;
            let account = AccountPublicKey::try_new(
                origin.clone(),
                Some(master.public_key().fingerprint()),
                origin.derive_account(&master)?.public_key(),
            )?;
            let key = account.derive_public_key(change, index)?;
            assert_eq!(faster_hex::hex_string(&key.to_bytes()), public, "{path}");
            assert_eq!(faster_hex::hex_string(&key.attrs().chain_code), chain_code, "{path}");
            assert_eq!(key.public_key(), &origin.derive_private_key(&master, change, index)?.public_key(secp256k1::SECP256K1));
        }
        Ok(())
    }

    #[test]
    fn all_networks_schemes_chains_and_import_boundaries() -> Result<()> {
        let master = ExtendedPrivateKey::<secp256k1::SecretKey>::new([42; 32])?;
        let mut keys = std::collections::BTreeSet::new();
        for network in [NetworkType::Mainnet, NetworkType::Testnet, NetworkType::Simnet, NetworkType::Devnet] {
            for scheme in [SignatureScheme::Schnorr, SignatureScheme::Ecdsa] {
                let origin = AccountDerivation::try_new(network, scheme, MAX_INDEX)?;
                let account = AccountPublicKey::try_new(origin.clone(), None, origin.derive_account(&master)?.public_key())?;
                assert!(keys.insert(account.xpub().to_bytes()));
                let json = serde_json::to_string(&account).unwrap();
                assert_eq!(serde_json::from_str::<AccountPublicKey>(&json).unwrap(), account);
                assert_eq!(borsh::from_slice::<AccountPublicKey>(&borsh::to_vec(&account).unwrap()).unwrap(), account);
                for change in [0, 1] {
                    for index in [0, 8, MAX_INDEX] {
                        assert_eq!(
                            account.derive_public_key(change, index)?.to_bytes(),
                            origin.derive_private_key(&master, change, index)?.public_key(secp256k1::SECP256K1).serialize()
                        );
                    }
                }
                assert!(account.derive_public_key(2, 0).is_err());
                assert!(account.derive_public_key(0, MAX_INDEX + 1).is_err());
                assert!(AccountPublicKey::try_new(origin, None, master.public_key()).is_err());
            }
        }
        for path in [
            "m/44'/111110'/0'/0'",
            "m/44'/111111'/2'/0'",
            "m/44'/111111'/0'/2147483648'",
            "m/44/111111'/0'/0'",
            "m/44'/111111'/0'/0'/0",
            "m/45'/111111'/0'/0'",
        ] {
            assert!(AccountDerivation::try_from(path.to_string()).is_err(), "{path}");
        }
        for path in
            ["m/44'/111111'/0'/0'/2/0", "m/44'/111111'/0'/0'/0'/0", "m/44'/111111'/0'/0'/0/2147483648", "m/44'/111111'/0'/0'/0"]
        {
            assert!(
                path.parse::<DerivationPath>().ok().and_then(|p| AccountDerivation::from_address_path(&p).ok()).is_none(),
                "{path}"
            );
        }
        Ok(())
    }
}
