//! PSKT input structure.

use crate::pskt::{KeySource, PartialSigs};
use crate::utils::{Error as CombineMapErr, combine_if_no_conflicts};
use derive_builder::Builder;
use kaspa_consensus_core::constants::MAX_TX_IN_SEQUENCE_NUM;
use kaspa_consensus_core::{
    hashing::sighash_type::{SIG_HASH_ALL, SigHashType},
    tx::{TransactionId, TransactionOutpoint, UtxoEntry},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, marker::PhantomData, ops::Add};

#[derive(Builder, Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
#[builder(default)]
#[builder(setter(skip))]
pub struct Input {
    #[builder(setter(strip_option))]
    pub utxo_entry: Option<UtxoEntry>,
    #[builder(setter)]
    pub previous_outpoint: TransactionOutpoint,
    /// The sequence number of this input.
    ///
    /// If omitted, assumed to be the final sequence number
    pub sequence: Option<u64>,
    #[builder(setter)]
    /// The minimum Unix timestamp that this input requires to be set as the transaction's lock time.
    pub min_time: Option<u64>,
    /// A map from public keys to their corresponding signature as would be
    /// pushed to the stack from a scriptSig.
    pub partial_sigs: PartialSigs,
    #[builder(setter)]
    /// The sighash type to be used for this input. Signatures for this input
    /// must use the sighash type.
    pub sighash_type: SigHashType,
    #[serde(with = "kaspa_utils::serde_bytes_optional")]
    #[builder(setter(strip_option))]
    /// The redeem script for this input.
    pub redeem_script: Option<Vec<u8>>,
    #[builder(setter(strip_option))]
    pub sig_op_count: Option<u8>,
    /// A map from public keys needed to sign this input to their corresponding
    /// master key fingerprints and derivation paths.
    pub bip32_derivations: BTreeMap<secp256k1::PublicKey, Option<KeySource>>,
    #[serde(with = "kaspa_utils::serde_bytes_optional")]
    /// The finalized, fully-constructed scriptSig with signatures and any other
    /// scripts necessary for this input to pass validation.
    pub final_script_sig: Option<Vec<u8>>,
    #[serde(skip_serializing, default)]
    pub(crate) hidden: PhantomData<()>, // prevents manual filling of fields
    #[builder(setter)]
    /// Proprietary key-value pairs for this input.
    pub proprietaries: BTreeMap<String, serde_value::Value>,
    #[serde(flatten)]
    #[builder(setter)]
    /// Unknown key-value pairs for this input.
    pub unknowns: BTreeMap<String, serde_value::Value>,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            utxo_entry: Default::default(),
            previous_outpoint: Default::default(),
            sequence: Default::default(),
            min_time: Default::default(),
            partial_sigs: Default::default(),
            sighash_type: SIG_HASH_ALL,
            redeem_script: Default::default(),
            sig_op_count: Default::default(),
            bip32_derivations: Default::default(),
            final_script_sig: Default::default(),
            hidden: Default::default(),
            proprietaries: Default::default(),
            unknowns: Default::default(),
        }
    }
}

impl Add for Input {
    type Output = Result<Self, CombineError>;

    fn add(mut self, rhs: Self) -> Self::Output {
        if self.previous_outpoint.transaction_id != rhs.previous_outpoint.transaction_id {
            return Err(CombineError::PreviousTxidMismatch {
                this: self.previous_outpoint.transaction_id,
                that: rhs.previous_outpoint.transaction_id,
            });
        }

        if self.previous_outpoint.index != rhs.previous_outpoint.index {
            return Err(CombineError::SpentOutputIndexMismatch {
                this: self.previous_outpoint.index,
                that: rhs.previous_outpoint.index,
            });
        }
        self.utxo_entry = match (self.utxo_entry.take(), rhs.utxo_entry) {
            (None, None) => None,
            (Some(utxo), None) | (None, Some(utxo)) => Some(utxo),
            (Some(left), Some(right)) if left == right => Some(left),
            (Some(left), Some(right)) => return Err(CombineError::NotCompatibleUtxos { this: left, that: right }),
        };

        // default: disable relative sequence locks and mark as finalized for lock-time check
        let lhs_sequence = self.sequence.unwrap_or(MAX_TX_IN_SEQUENCE_NUM);
        let rhs_sequence = rhs.sequence.unwrap_or(MAX_TX_IN_SEQUENCE_NUM);
        if lhs_sequence != rhs_sequence {
            return Err(CombineError::SequenceMismatch { this: lhs_sequence, that: rhs_sequence });
        }
        self.sequence = self.sequence.or(rhs.sequence);

        if self.min_time != rhs.min_time {
            return Err(CombineError::MinTimeMismatch { this: self.min_time, that: rhs.min_time });
        }

        self.sig_op_count = match (self.sig_op_count, rhs.sig_op_count) {
            (None, None) => None,
            (Some(count), None) | (None, Some(count)) => Some(count),
            (Some(left), Some(right)) if left == right => Some(left),
            (Some(left), Some(right)) => return Err(CombineError::SigOpCountMismatch { this: left, that: right }),
        };

        self.partial_sigs = combine_if_no_conflicts(self.partial_sigs, rhs.partial_sigs).map_err(|error| {
            CombineError::ConflictingPartialSignature { public_key: error.field, this: error.lhs, that: error.rhs }
        })?;

        // if no conflicts
        self.redeem_script = match (self.redeem_script.take(), rhs.redeem_script) {
            (None, None) => None,
            (Some(script), None) | (None, Some(script)) => Some(script),
            (Some(script_left), Some(script_right)) if script_left == script_right => Some(script_left),
            (Some(script_left), Some(script_right)) => {
                return Err(CombineError::NotCompatibleRedeemScripts { this: script_left, that: script_right });
            }
        };

        // if no conflicts
        self.final_script_sig = match (self.final_script_sig.take(), rhs.final_script_sig) {
            (None, None) => None,
            (Some(script), None) | (None, Some(script)) => Some(script),
            (Some(script_left), Some(script_right)) if script_left == script_right => Some(script_left),
            (Some(script_left), Some(script_right)) => {
                return Err(CombineError::NotCompatibleFinalScripts { this: script_left, that: script_right });
            }
        };

        self.bip32_derivations = combine_if_no_conflicts(self.bip32_derivations, rhs.bip32_derivations)?;
        self.proprietaries =
            combine_if_no_conflicts(self.proprietaries, rhs.proprietaries).map_err(CombineError::NotCompatibleProprietary)?;
        self.unknowns = combine_if_no_conflicts(self.unknowns, rhs.unknowns).map_err(CombineError::NotCompatibleUnknownField)?;

        Ok(self)
    }
}

/// Error combining two input maps.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum CombineError {
    #[error("The previous txids are not the same")]
    PreviousTxidMismatch {
        /// Attempted to combine a PSKT with `this` previous txid.
        this: TransactionId,
        /// Into a PSKT with `that` previous txid.
        that: TransactionId,
    },
    #[error("The spent output indexes are not the same")]
    SpentOutputIndexMismatch {
        /// Attempted to combine a PSKT with `this` spent output index.
        this: u32,
        /// Into a PSKT with `that` spent output index.
        that: u32,
    },
    #[error("The sequence numbers are not the same")]
    SequenceMismatch { this: u64, that: u64 },
    #[error("The minimum lock times are not the same")]
    MinTimeMismatch { this: Option<u64>, that: Option<u64> },
    #[error("The sig op counts are not the same")]
    SigOpCountMismatch { this: u8, that: u8 },
    #[error("Two different redeem scripts detected")]
    NotCompatibleRedeemScripts { this: Vec<u8>, that: Vec<u8> },
    #[error("Two different utxos detected")]
    NotCompatibleUtxos { this: UtxoEntry, that: UtxoEntry },
    #[error("Two different partial signatures for the same public key")]
    ConflictingPartialSignature { public_key: secp256k1::PublicKey, this: crate::pskt::Signature, that: crate::pskt::Signature },
    #[error("Two different final script signatures detected")]
    NotCompatibleFinalScripts { this: Vec<u8>, that: Vec<u8> },

    #[error("Two different derivations for the same key")]
    NotCompatibleBip32Derivations(#[from] CombineMapErr<secp256k1::PublicKey, Option<KeySource>>),
    #[error("Two different unknown field values")]
    NotCompatibleUnknownField(CombineMapErr<String, serde_value::Value>),
    #[error("Two different proprietary values")]
    NotCompatibleProprietary(CombineMapErr<String, serde_value::Value>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pskt::Signature;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint};
    use secp256k1::{Keypair, rand::thread_rng};
    use std::str::FromStr;

    fn transaction_id(byte: u8) -> TransactionId {
        TransactionId::from_str(&format!("{byte:02x}").repeat(32)).unwrap()
    }

    fn keypair() -> Keypair {
        Keypair::new(secp256k1::SECP256K1, &mut thread_rng())
    }

    fn signature(keypair: &Keypair, byte: u8) -> Signature {
        let message = secp256k1::Message::from_digest_slice(&[byte; 32]).unwrap();
        Signature::Schnorr(keypair.sign_schnorr(message))
    }

    fn assert_conflict(left: Input, right: Input, predicate: impl FnOnce(CombineError) -> bool) {
        match left + right {
            Ok(_) => panic!("expected combine conflict"),
            Err(error) => assert!(predicate(error)),
        }
    }

    #[test]
    fn combine_carries_one_sided_mergeable_fields() {
        let utxo_entry = UtxoEntry { amount: 1, ..Default::default() };
        let combined = (Input { utxo_entry: Some(utxo_entry.clone()), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.utxo_entry, Some(utxo_entry.clone()));
        let combined = (Input::default() + Input { utxo_entry: Some(utxo_entry.clone()), ..Default::default() }).unwrap();
        assert_eq!(combined.utxo_entry, Some(utxo_entry));

        let combined = (Input { sequence: Some(MAX_TX_IN_SEQUENCE_NUM), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.sequence, Some(MAX_TX_IN_SEQUENCE_NUM));
        let combined = (Input::default() + Input { sequence: Some(MAX_TX_IN_SEQUENCE_NUM), ..Default::default() }).unwrap();
        assert_eq!(combined.sequence, Some(MAX_TX_IN_SEQUENCE_NUM));

        let combined = (Input { sig_op_count: Some(2), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.sig_op_count, Some(2));
        let combined = (Input::default() + Input { sig_op_count: Some(2), ..Default::default() }).unwrap();
        assert_eq!(combined.sig_op_count, Some(2));

        let keypair = keypair();
        let public_key = keypair.public_key();
        let partial_sigs = BTreeMap::from([(public_key, signature(&keypair, 1))]);
        let combined = (Input { partial_sigs: partial_sigs.clone(), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.partial_sigs, partial_sigs.clone());
        let combined = (Input::default() + Input { partial_sigs: partial_sigs.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.partial_sigs, partial_sigs);

        let redeem_script = vec![1, 2, 3];
        let combined = (Input { redeem_script: Some(redeem_script.clone()), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.redeem_script, Some(redeem_script.clone()));
        let combined = (Input::default() + Input { redeem_script: Some(redeem_script.clone()), ..Default::default() }).unwrap();
        assert_eq!(combined.redeem_script, Some(redeem_script));

        let final_script_sig = vec![4, 5, 6];
        let combined = (Input { final_script_sig: Some(final_script_sig.clone()), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.final_script_sig, Some(final_script_sig.clone()));
        let combined = (Input::default() + Input { final_script_sig: Some(final_script_sig.clone()), ..Default::default() }).unwrap();
        assert_eq!(combined.final_script_sig, Some(final_script_sig));

        let bip32_derivations = BTreeMap::from([(public_key, Some(KeySource::new([1, 2, 3, 4], Default::default())))]);
        let combined = (Input { bip32_derivations: bip32_derivations.clone(), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.bip32_derivations, bip32_derivations.clone());
        let combined = (Input::default() + Input { bip32_derivations: bip32_derivations.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.bip32_derivations, bip32_derivations);

        let proprietaries = BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]);
        let combined = (Input { proprietaries: proprietaries.clone(), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.proprietaries, proprietaries.clone());
        let combined = (Input::default() + Input { proprietaries: proprietaries.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.proprietaries, proprietaries);

        let unknowns = BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]);
        let combined = (Input { unknowns: unknowns.clone(), ..Default::default() } + Input::default()).unwrap();
        assert_eq!(combined.unknowns, unknowns.clone());
        let combined = (Input::default() + Input { unknowns: unknowns.clone(), ..Default::default() }).unwrap();
        assert_eq!(combined.unknowns, unknowns);
    }

    #[test]
    fn combine_rejects_conflicting_fields() {
        assert_conflict(
            Input {
                previous_outpoint: TransactionOutpoint { transaction_id: transaction_id(1), ..Default::default() },
                ..Default::default()
            },
            Input {
                previous_outpoint: TransactionOutpoint { transaction_id: transaction_id(2), ..Default::default() },
                ..Default::default()
            },
            |error| matches!(error, CombineError::PreviousTxidMismatch { .. }),
        );

        assert_conflict(
            Input { previous_outpoint: TransactionOutpoint { index: 1, ..Default::default() }, ..Default::default() },
            Input { previous_outpoint: TransactionOutpoint { index: 2, ..Default::default() }, ..Default::default() },
            |error| matches!(error, CombineError::SpentOutputIndexMismatch { this: 1, that: 2 }),
        );

        assert_conflict(
            Input { utxo_entry: Some(UtxoEntry { amount: 1, ..Default::default() }), ..Default::default() },
            Input { utxo_entry: Some(UtxoEntry { amount: 2, ..Default::default() }), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleUtxos { .. }),
        );

        assert_conflict(
            Input { sequence: Some(1), ..Default::default() },
            Input { sequence: Some(2), ..Default::default() },
            |error| matches!(error, CombineError::SequenceMismatch { this: 1, that: 2 }),
        );

        assert_conflict(
            Input { min_time: Some(1), ..Default::default() },
            Input { min_time: Some(2), ..Default::default() },
            |error| matches!(error, CombineError::MinTimeMismatch { this: Some(1), that: Some(2) }),
        );

        assert_conflict(
            Input { sig_op_count: Some(1), ..Default::default() },
            Input { sig_op_count: Some(2), ..Default::default() },
            |error| matches!(error, CombineError::SigOpCountMismatch { this: 1, that: 2 }),
        );

        let keypair = keypair();
        let public_key = keypair.public_key();
        assert_conflict(
            Input { partial_sigs: BTreeMap::from([(public_key, signature(&keypair, 1))]), ..Default::default() },
            Input { partial_sigs: BTreeMap::from([(public_key, signature(&keypair, 2))]), ..Default::default() },
            |error| matches!(error, CombineError::ConflictingPartialSignature { .. }),
        );

        assert_conflict(
            Input { redeem_script: Some(vec![1]), ..Default::default() },
            Input { redeem_script: Some(vec![2]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleRedeemScripts { .. }),
        );

        assert_conflict(
            Input { final_script_sig: Some(vec![1]), ..Default::default() },
            Input { final_script_sig: Some(vec![2]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleFinalScripts { .. }),
        );

        assert_conflict(
            Input { bip32_derivations: BTreeMap::from([(public_key, None)]), ..Default::default() },
            Input {
                bip32_derivations: BTreeMap::from([(public_key, Some(KeySource::new([1, 2, 3, 4], Default::default())))]),
                ..Default::default()
            },
            |error| matches!(error, CombineError::NotCompatibleBip32Derivations(_)),
        );

        assert_conflict(
            Input { proprietaries: BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]), ..Default::default() },
            Input { proprietaries: BTreeMap::from([("key".to_string(), serde_value::Value::U8(2))]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleProprietary(_)),
        );

        assert_conflict(
            Input { unknowns: BTreeMap::from([("key".to_string(), serde_value::Value::U8(1))]), ..Default::default() },
            Input { unknowns: BTreeMap::from([("key".to_string(), serde_value::Value::U8(2))]), ..Default::default() },
            |error| matches!(error, CombineError::NotCompatibleUnknownField(_)),
        );
    }
}
