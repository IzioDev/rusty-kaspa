//! PSKT output structure.

use crate::pskt::KeySource;
use crate::utils::combine_if_no_conflicts;
use derive_builder::Builder;
use kaspa_consensus_core::tx::{CovenantBinding, ScriptPublicKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Add};

#[derive(Builder, Default, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[builder(default)]
pub struct Output {
    /// The output's amount (serialized as sompi).
    pub amount: u64,
    /// The script for this output, also known as the scriptPubKey.
    pub script_public_key: ScriptPublicKey,
    #[builder(setter(strip_option))]
    #[serde(with = "kaspa_utils::serde_bytes_optional")]
    /// The redeem script for this output.
    pub redeem_script: Option<Vec<u8>>,
    /// A map from public keys needed to spend this output to their
    /// corresponding master key fingerprints and derivation paths.
    pub bip32_derivations: BTreeMap<secp256k1::PublicKey, Option<KeySource>>,
    /// The covenant for this output.
    pub covenant: Option<CovenantBinding>,
    /// Proprietary key-value pairs for this output.
    pub proprietaries: BTreeMap<String, serde_value::Value>,
    #[serde(flatten)]
    /// Unknown key-value pairs for this output.
    pub unknowns: BTreeMap<String, serde_value::Value>,
}

impl Add for Output {
    type Output = Result<Self, CombineError>;

    fn add(mut self, rhs: Self) -> Self::Output {
        if self.amount != rhs.amount {
            return Err(CombineError::AmountMismatch { this: self.amount, that: rhs.amount });
        }
        if self.script_public_key != rhs.script_public_key {
            return Err(CombineError::ScriptPubkeyMismatch { this: self.script_public_key, that: rhs.script_public_key });
        }
        self.redeem_script = match (self.redeem_script.take(), rhs.redeem_script) {
            (None, None) => None,
            (Some(script), None) | (None, Some(script)) => Some(script),
            (Some(script_left), Some(script_right)) if script_left == script_right => Some(script_left),
            (Some(script_left), Some(script_right)) => {
                return Err(CombineError::NotCompatibleRedeemScripts { this: script_left, that: script_right });
            }
        };

        self.covenant = match (self.covenant.take(), rhs.covenant) {
            (None, None) => None,
            (Some(covenant), None) | (None, Some(covenant)) => Some(covenant),
            (Some(covenant_left), Some(covenant_right)) if covenant_left == covenant_right => Some(covenant_left),
            (Some(covenant_left), Some(covenant_right)) => {
                return Err(CombineError::NotCompatibleCovenants { this: covenant_left, that: covenant_right });
            }
        };

        self.bip32_derivations = combine_if_no_conflicts(self.bip32_derivations, rhs.bip32_derivations)?;
        self.proprietaries =
            combine_if_no_conflicts(self.proprietaries, rhs.proprietaries).map_err(CombineError::NotCompatibleProprietary)?;
        self.unknowns = combine_if_no_conflicts(self.unknowns, rhs.unknowns).map_err(CombineError::NotCompatibleUnknownField)?;

        Ok(self)
    }
}

/// Error combining two output maps.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CombineError {
    #[error("The amounts are not the same")]
    AmountMismatch {
        /// Attempted to combine a PSKT with `this` previous txid.
        this: u64,
        /// Into a PSKT with `that` previous txid.
        that: u64,
    },
    #[error("The script_pubkeys are not the same")]
    ScriptPubkeyMismatch {
        /// Attempted to combine a PSKT with `this` script_pubkey.
        this: ScriptPublicKey,
        /// Into a PSKT with `that` script_pubkey.
        that: ScriptPublicKey,
    },
    #[error("Two different redeem scripts detected")]
    NotCompatibleRedeemScripts { this: Vec<u8>, that: Vec<u8> },
    #[error("Two different covenants detected")]
    NotCompatibleCovenants { this: CovenantBinding, that: CovenantBinding },
    #[error("Two different derivations for the same key")]
    NotCompatibleBip32Derivations(#[from] crate::utils::Error<secp256k1::PublicKey, Option<KeySource>>),
    #[error("Two different unknown field values")]
    NotCompatibleUnknownField(crate::utils::Error<String, serde_value::Value>),
    #[error("Two different proprietary values")]
    NotCompatibleProprietary(crate::utils::Error<String, serde_value::Value>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use kaspa_consensus_core::tx::TransactionId;
    use secp256k1::{Keypair, rand::thread_rng};

    fn keypair() -> Keypair {
        Keypair::new(secp256k1::SECP256K1, &mut thread_rng())
    }

    fn covenant(byte: u8) -> CovenantBinding {
        CovenantBinding::new(byte as u16, TransactionId::from_bytes([byte; 32]))
    }

    fn script_public_key(byte: u8) -> ScriptPublicKey {
        ScriptPublicKey::from_vec(0, vec![byte])
    }

    fn assert_conflict(left: Output, right: Output, predicate: impl FnOnce(CombineError) -> bool) {
        match left + right {
            Ok(_) => panic!("expected combine conflict"),
            Err(error) => assert!(predicate(error)),
        }
    }

    #[test]
    fn combine_carries_one_sided_mergeable_fields() {
        let redeem_script = vec![1, 2, 3];
        let combined = (Output { redeem_script: Some(redeem_script.clone()), ..Default::default() } + Output::default()).unwrap();
        assert_eq!(combined.redeem_script, Some(redeem_script.clone()));
        let combined = (Output::default() + Output { redeem_script: Some(redeem_script.clone()), ..Default::default() }).unwrap();
        assert_eq!(combined.redeem_script, Some(redeem_script));

        let covenant = covenant(1);
        let combined = (Output { covenant: Some(covenant), ..Default::default() } + Output::default()).unwrap();
        assert_eq!(combined.covenant, Some(covenant));
        let combined = (Output::default() + Output { covenant: Some(covenant), ..Default::default() }).unwrap();
        assert_eq!(combined.covenant, Some(covenant));

        let keypair = keypair();
        let public_key = keypair.public_key();
        let bip32_derivations = BTreeMap::from([(public_key, Some(KeySource::new([1, 2, 3, 4], Default::default())))]);
        let combined = (Output { bip32_derivations: bip32_derivations.clone(), ..Default::default() } + Output::default()).unwrap();
        assert_eq!(combined.bip32_derivations, bip32_derivations.clone());
        let combined = (Output::default() + Output { bip32_derivations: bip32_derivations.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.bip32_derivations, bip32_derivations);

        let proprietaries = BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]);
        let combined = (Output { proprietaries: proprietaries.clone(), ..Default::default() } + Output::default()).unwrap();
        assert_eq!(combined.proprietaries, proprietaries.clone());
        let combined = (Output::default() + Output { proprietaries: proprietaries.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.proprietaries, proprietaries);

        let unknowns = BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]);
        let combined = (Output { unknowns: unknowns.clone(), ..Default::default() } + Output::default()).unwrap();
        assert_eq!(combined.unknowns, unknowns.clone());
        let combined = (Output::default() + Output { unknowns: unknowns.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.unknowns, unknowns);
    }

    #[test]
    fn combine_rejects_conflicting_fields() {
        assert_conflict(Output { amount: 1, ..Default::default() }, Output { amount: 2, ..Default::default() }, |error| {
            matches!(error, CombineError::AmountMismatch { this: 1, that: 2 })
        });

        assert_conflict(
            Output { script_public_key: script_public_key(1), ..Default::default() },
            Output { script_public_key: script_public_key(2), ..Default::default() },
            |error| matches!(error, CombineError::ScriptPubkeyMismatch { .. }),
        );

        assert_conflict(
            Output { redeem_script: Some(vec![1]), ..Default::default() },
            Output { redeem_script: Some(vec![2]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleRedeemScripts { .. }),
        );

        assert_conflict(
            Output { covenant: Some(covenant(1)), ..Default::default() },
            Output { covenant: Some(covenant(2)), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleCovenants { .. }),
        );

        let keypair = keypair();
        let public_key = keypair.public_key();
        assert_conflict(
            Output { bip32_derivations: BTreeMap::from([(public_key, None)]), ..Default::default() },
            Output {
                bip32_derivations: BTreeMap::from([(public_key, Some(KeySource::new([1, 2, 3, 4], Default::default())))]),
                ..Default::default()
            },
            |error| matches!(error, CombineError::NotCompatibleBip32Derivations(_)),
        );

        assert_conflict(
            Output { proprietaries: BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]), ..Default::default() },
            Output { proprietaries: BTreeMap::from([("key".to_string(), serde_value::Value::U8(2))]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleProprietary(_)),
        );

        assert_conflict(
            Output { unknowns: BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]), ..Default::default() },
            Output { unknowns: BTreeMap::from([("key".to_string(), serde_value::Value::U8(2))]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleUnknownField(_)),
        );
    }
}
