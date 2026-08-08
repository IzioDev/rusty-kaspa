use crate::opcodes::{
    codes::{OpCheckMultiSig, OpCheckMultiSigECDSA},
    to_script_num,
};
use crate::script_builder::{ScriptBuilder, ScriptBuilderError};
use crate::{MAX_PUB_KEYS_PER_MUTLTISIG, TxScriptError, parse_script};
use kaspa_consensus_core::{hashing::sighash::SigHashReusedValuesUnsync, tx::PopulatedTransaction};
use std::borrow::Borrow;
use thiserror::Error;

#[derive(Error, PartialEq, Eq, Debug, Clone)]
pub enum Error {
    // ErrTooManyRequiredSigs is returned from multisig_script when the
    // specified number of required signatures is larger than the number of
    // provided public keys.
    #[error("too many required signatures")]
    ErrTooManyRequiredSigs,
    #[error(transparent)]
    ScriptBuilderError(#[from] ScriptBuilderError),
    #[error("provided public keys should not be empty")]
    EmptyKeys,
    #[error("too many public keys")]
    ErrTooManyPublicKeys,
    #[error("invalid multisig redeem script: {0}")]
    InvalidMultisigRedeemScript(String),
    #[error("invalid multisig threshold: required {required}, public keys {public_keys}")]
    InvalidMultisigThreshold { required: usize, public_keys: usize },
    #[error("invalid multisig public key")]
    InvalidMultisigPublicKey(#[from] secp256k1::Error),
    #[error("multisig redeem script parse error: {0}")]
    TxScriptError(#[from] TxScriptError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultisigSignatureType {
    Schnorr,
    Ecdsa,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Context of a Multisig Redeem script after a successful parsing
pub enum MultisigRedeemScriptContext {
    Schnorr { required: usize, public_keys: Box<[secp256k1::XOnlyPublicKey]> },
    Ecdsa { required: usize, public_keys: Box<[secp256k1::PublicKey]> },
}

pub fn multisig_redeem_script(pub_keys: impl Iterator<Item = impl Borrow<[u8; 32]>>, required: usize) -> Result<Vec<u8>, Error> {
    if pub_keys.size_hint().1.is_some_and(|upper| upper < required) {
        return Err(Error::ErrTooManyRequiredSigs);
    }
    let mut builder = ScriptBuilder::new();
    builder.add_i64(required as i64)?;

    let mut count = 0i64;
    for pub_key in pub_keys {
        count += 1;
        if count > MAX_PUB_KEYS_PER_MUTLTISIG as i64 {
            return Err(Error::ErrTooManyPublicKeys);
        }
        builder.add_data(pub_key.borrow().as_slice())?;
    }

    if (count as usize) < required {
        return Err(Error::ErrTooManyRequiredSigs);
    }
    if count == 0 {
        return Err(Error::EmptyKeys);
    }

    builder.add_i64(count)?;
    builder.add_op(OpCheckMultiSig)?;

    Ok(builder.drain())
}

pub fn multisig_redeem_script_ecdsa(pub_keys: impl Iterator<Item = impl Borrow<[u8; 33]>>, required: usize) -> Result<Vec<u8>, Error> {
    if pub_keys.size_hint().1.is_some_and(|upper| upper < required) {
        return Err(Error::ErrTooManyRequiredSigs);
    }
    let mut builder = ScriptBuilder::new();
    builder.add_i64(required as i64)?;

    let mut count = 0i64;
    for pub_key in pub_keys {
        count += 1;
        if count > MAX_PUB_KEYS_PER_MUTLTISIG as i64 {
            return Err(Error::ErrTooManyPublicKeys);
        }
        builder.add_data(pub_key.borrow().as_slice())?;
    }

    if (count as usize) < required {
        return Err(Error::ErrTooManyRequiredSigs);
    }
    if count == 0 {
        return Err(Error::EmptyKeys);
    }

    builder.add_i64(count)?;
    builder.add_op(OpCheckMultiSigECDSA)?;

    Ok(builder.drain())
}

pub fn parse_multisig_redeem_script(script: &[u8]) -> Result<MultisigRedeemScriptContext, Error> {
    let opcodes = parse_script::<PopulatedTransaction<'_>, SigHashReusedValuesUnsync>(script).collect::<Result<Vec<_>, _>>()?;

    // <required_count> <pubkey>... <pubkey_count> <OP_CHECKMULTISIG>
    if opcodes.len() < 4 {
        return Err(Error::InvalidMultisigRedeemScript("script is too short".to_string()));
    }

    let terminal = opcodes.last().expect("just checked len >= 4");

    let signature_type = match terminal.value() {
        value if value == OpCheckMultiSig => MultisigSignatureType::Schnorr,
        value if value == OpCheckMultiSigECDSA => MultisigSignatureType::Ecdsa,
        _ => return Err(Error::InvalidMultisigRedeemScript("terminal opcode is not CHECKMULTISIG".to_string())),
    };

    let required = usize::try_from(to_script_num(&opcodes[0])?)
        .map_err(|_| Error::InvalidMultisigRedeemScript("required signature count is negative".to_string()))?;

    let public_key_count = usize::try_from(to_script_num(&opcodes[opcodes.len() - 2])?)
        .map_err(|_| Error::InvalidMultisigRedeemScript("public key count is negative".to_string()))?;

    if public_key_count > MAX_PUB_KEYS_PER_MUTLTISIG as usize {
        return Err(Error::InvalidMultisigRedeemScript(format!(
            "public key count {public_key_count} exceeds maximum {MAX_PUB_KEYS_PER_MUTLTISIG}"
        )));
    }

    let key_opcodes = &opcodes[1..opcodes.len() - 2];

    if key_opcodes.len() != public_key_count {
        return Err(Error::InvalidMultisigRedeemScript(format!(
            "declared public key count {public_key_count} does not match {} pushed keys",
            key_opcodes.len()
        )));
    }

    if required == 0 || required > public_key_count {
        return Err(Error::InvalidMultisigThreshold { required, public_keys: public_key_count });
    }

    match signature_type {
        MultisigSignatureType::Schnorr => {
            let public_keys = key_opcodes
                .iter()
                .map(|opcode| {
                    if !opcode.is_push_opcode() {
                        return Err(Error::InvalidMultisigRedeemScript("public key is not a push opcode".to_string()));
                    }

                    let data = opcode.get_data();

                    if data.len() != 32 {
                        return Err(Error::InvalidMultisigRedeemScript(format!(
                            "schnorr public key length is {}, expected 32",
                            data.len()
                        )));
                    }

                    Ok(secp256k1::XOnlyPublicKey::from_slice(data)?)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();

            Ok(MultisigRedeemScriptContext::Schnorr { required, public_keys })
        }

        MultisigSignatureType::Ecdsa => {
            let public_keys = key_opcodes
                .iter()
                .map(|opcode| {
                    if !opcode.is_push_opcode() {
                        return Err(Error::InvalidMultisigRedeemScript("public key is not a push opcode".to_string()));
                    }

                    let data = opcode.get_data();

                    if data.len() != 33 {
                        return Err(Error::InvalidMultisigRedeemScript(format!(
                            "ecdsa public key length is {}, expected 33",
                            data.len()
                        )));
                    }

                    Ok(secp256k1::PublicKey::from_slice(data)?)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice();

            Ok(MultisigRedeemScriptContext::Ecdsa { required, public_keys })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineContext, TxScriptEngine, caches::Cache, opcodes::codes::OpData65, pay_to_script_hash_script};
    use core::str::FromStr;
    use kaspa_consensus_core::{
        hashing::{
            sighash::{SigHashReusedValuesUnsync, calc_ecdsa_signature_hash, calc_schnorr_signature_hash},
            sighash_type::SIG_HASH_ALL,
        },
        subnets::SubnetworkId,
        tx::*,
    };
    use kaspa_utils::hex::FromHex;
    use rand::thread_rng;
    use secp256k1::Keypair;
    use std::{iter, iter::empty};

    struct Input {
        kp: Keypair,
        required: bool,
        sign: bool,
    }

    fn kp() -> [Keypair; 3] {
        let kp1 = Keypair::from_seckey_slice(
            secp256k1::SECP256K1,
            Vec::from_hex("1d99c236b1f37b3b845336e6c568ba37e9ced4769d83b7a096eec446b940d160").unwrap().as_slice(),
        )
        .unwrap();
        let kp2 = Keypair::from_seckey_slice(
            secp256k1::SECP256K1,
            Vec::from_hex("349ca0c824948fed8c2c568ce205e9d9be4468ef099cad76e3e5ec918954aca4").unwrap().as_slice(),
        )
        .unwrap();
        let kp3 = Keypair::new(secp256k1::SECP256K1, &mut thread_rng());
        [kp1, kp2, kp3]
    }

    #[test]
    fn test_too_many_required_sigs() {
        let result = multisig_redeem_script(iter::once([0u8; 32]), 2);
        assert_eq!(result, Err(Error::ErrTooManyRequiredSigs));
        let result = multisig_redeem_script_ecdsa(iter::once(&[0u8; 33]), 2);
        assert_eq!(result, Err(Error::ErrTooManyRequiredSigs));
    }

    #[test]
    fn test_empty_keys() {
        let result = multisig_redeem_script(empty::<[u8; 32]>(), 0);
        assert_eq!(result, Err(Error::EmptyKeys));
    }

    #[test]
    fn test_too_many_public_keys_while_building_redeem_script() {
        let result = multisig_redeem_script(iter::repeat_n([0u8; 32], MAX_PUB_KEYS_PER_MUTLTISIG as usize + 1), 1);
        assert_eq!(result, Err(Error::ErrTooManyPublicKeys));

        let result = multisig_redeem_script_ecdsa(iter::repeat_n([0u8; 33], MAX_PUB_KEYS_PER_MUTLTISIG as usize + 1), 1);
        assert_eq!(result, Err(Error::ErrTooManyPublicKeys));
    }

    #[test]
    fn test_parse_schnorr_multisig_redeem_script() {
        let [kp1, kp2, kp3] = kp();
        let keys = [kp1, kp2, kp3];
        let x_only_keys = keys.iter().map(|kp| kp.x_only_public_key().0).collect::<Vec<_>>();
        let script = multisig_redeem_script(x_only_keys.iter().map(|key| key.serialize()), 2).unwrap();

        let context = parse_multisig_redeem_script(&script).unwrap();

        let MultisigRedeemScriptContext::Schnorr { required, public_keys } = context else {
            panic!("expected Schnorr multisig redeem script context");
        };

        assert_eq!(required, 2);
        assert_eq!(public_keys.as_ref(), x_only_keys.as_slice());
    }

    #[test]
    fn test_parse_ecdsa_multisig_redeem_script() {
        let [kp1, kp2, kp3] = kp();
        let keys = [kp1.public_key(), kp2.public_key(), kp3.public_key()];
        let script = multisig_redeem_script_ecdsa(keys.iter().map(|key| key.serialize()), 2).unwrap();

        let context = parse_multisig_redeem_script(&script).unwrap();

        let MultisigRedeemScriptContext::Ecdsa { required, public_keys } = context else {
            panic!("expected ECDSA multisig redeem script context");
        };

        assert_eq!(required, 2);
        assert_eq!(public_keys.as_ref(), keys.as_slice());
    }

    #[test]
    fn test_parse_rejects_malformed_multisig_redeem_script() {
        // empty pks
        assert!(parse_multisig_redeem_script(&[]).is_err());

        let [kp1, kp2, ..] = kp();
        let mut script = ScriptBuilder::new()
            // 2 out of 2 and missing checksig op
            .add_i64(2)
            .unwrap()
            .add_data(&kp1.x_only_public_key().0.serialize())
            .unwrap()
            .add_data(&kp2.x_only_public_key().0.serialize())
            .unwrap()
            .add_i64(2)
            .unwrap()
            .drain();
        assert!(parse_multisig_redeem_script(&script).is_err());

        script.push(OpCheckMultiSig);
        // 3 out of 2
        script[0] = crate::opcodes::codes::Op3;
        assert!(matches!(parse_multisig_redeem_script(&script), Err(Error::InvalidMultisigThreshold { required: 3, public_keys: 2 })));
    }

    fn check_multisig_scenario(inputs: Vec<Input>, required: usize, is_ok: bool, is_ecdsa: bool) {
        // Taken from: d839d29b549469d0f9a23e51febe68d4084967a6a477868b511a5a8d88c5ae06
        let prev_tx_id = TransactionId::from_str("63020db736215f8b1105a9281f7bcbb6473d965ecc45bb2fb5da59bd35e6ff84").unwrap();
        let filtered = inputs.iter().filter(|input| input.required);
        let script = if !is_ecdsa {
            let pks = filtered.map(|input| input.kp.x_only_public_key().0.serialize());
            multisig_redeem_script(pks, required).unwrap()
        } else {
            let pks = filtered.map(|input| input.kp.public_key().serialize());
            multisig_redeem_script_ecdsa(pks, required).unwrap()
        };

        let tx = Transaction::new(
            0,
            vec![TransactionInput {
                previous_outpoint: TransactionOutpoint { transaction_id: prev_tx_id, index: 0 },
                signature_script: vec![],
                sequence: 0,
                compute_commit: ComputeCommit::SigopCount(4.into()),
            }],
            vec![],
            0,
            SubnetworkId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            0,
            vec![],
        );

        let entries = vec![UtxoEntry {
            amount: 12793000000000,
            script_public_key: pay_to_script_hash_script(&script),
            block_daa_score: 36151168,
            is_coinbase: false,
            covenant_id: None,
        }];
        let mut tx = MutableTransaction::with_entries(tx, entries);

        let reused_values = SigHashReusedValuesUnsync::new();
        let sig_hash = if !is_ecdsa {
            calc_schnorr_signature_hash(&tx.as_verifiable(), 0, SIG_HASH_ALL, &reused_values)
        } else {
            calc_ecdsa_signature_hash(&tx.as_verifiable(), 0, SIG_HASH_ALL, &reused_values)
        };
        let msg = secp256k1::Message::from_digest_slice(sig_hash.as_bytes().as_slice()).unwrap();
        let signatures: Vec<_> = inputs
            .iter()
            .filter(|input| input.sign)
            .flat_map(|input| {
                if !is_ecdsa {
                    let sig = *input.kp.sign_schnorr(msg).as_ref();
                    iter::once(OpData65).chain(sig).chain([SIG_HASH_ALL.to_u8()])
                } else {
                    let sig = input.kp.secret_key().sign_ecdsa(msg).serialize_compact();
                    iter::once(OpData65).chain(sig).chain([SIG_HASH_ALL.to_u8()])
                }
            })
            .collect();

        {
            tx.tx.inputs[0].signature_script =
                signatures.into_iter().chain(ScriptBuilder::new().add_data(&script).unwrap().drain()).collect();
        }

        let tx = tx.as_verifiable();
        let (input, entry) = tx.populated_inputs().next().unwrap();

        let cache = Cache::new(10_000);
        let ctx = EngineContext::new(&cache).with_reused(&reused_values);
        let mut engine = TxScriptEngine::from_transaction_input(&tx, input, 0, entry, ctx, Default::default());
        assert_eq!(engine.execute().is_ok(), is_ok);
    }
    #[test]
    fn test_multisig_1_2() {
        let [kp1, kp2, ..] = kp();
        check_multisig_scenario(
            vec![Input { kp: kp1, required: true, sign: false }, Input { kp: kp2, required: true, sign: true }],
            1,
            true,
            false,
        );
        let [kp1, kp2, ..] = kp();
        check_multisig_scenario(
            vec![Input { kp: kp1, required: true, sign: true }, Input { kp: kp2, required: true, sign: false }],
            1,
            true,
            false,
        );

        // ecdsa
        check_multisig_scenario(
            vec![Input { kp: kp1, required: true, sign: false }, Input { kp: kp2, required: true, sign: true }],
            1,
            true,
            true,
        );
        let [kp1, kp2, ..] = kp();
        check_multisig_scenario(
            vec![Input { kp: kp1, required: true, sign: true }, Input { kp: kp2, required: true, sign: false }],
            1,
            true,
            true,
        );
    }

    #[test]
    fn test_multisig_2_2() {
        let [kp1, kp2, ..] = kp();
        check_multisig_scenario(
            vec![Input { kp: kp1, required: true, sign: true }, Input { kp: kp2, required: true, sign: true }],
            2,
            true,
            false,
        );

        // ecdsa
        let [kp1, kp2, ..] = kp();
        check_multisig_scenario(
            vec![Input { kp: kp1, required: true, sign: true }, Input { kp: kp2, required: true, sign: true }],
            2,
            true,
            true,
        );
    }

    #[test]
    fn test_multisig_wrong_signer() {
        let [kp1, kp2, kp3] = kp();
        check_multisig_scenario(
            vec![
                Input { kp: kp1, required: true, sign: false },
                Input { kp: kp2, required: true, sign: false },
                Input { kp: kp3, required: false, sign: true },
            ],
            1,
            false,
            false,
        );

        // ecdsa
        let [kp1, kp2, kp3] = kp();
        check_multisig_scenario(
            vec![
                Input { kp: kp1, required: true, sign: false },
                Input { kp: kp2, required: true, sign: false },
                Input { kp: kp3, required: false, sign: true },
            ],
            1,
            false,
            true,
        );
    }

    #[test]
    fn test_multisig_not_enough() {
        let [kp1, kp2, kp3] = kp();
        check_multisig_scenario(
            vec![
                Input { kp: kp1, required: true, sign: true },
                Input { kp: kp2, required: true, sign: true },
                Input { kp: kp3, required: true, sign: false },
            ],
            3,
            false,
            false,
        );

        let [kp1, kp2, kp3] = kp();
        check_multisig_scenario(
            vec![
                Input { kp: kp1, required: true, sign: true },
                Input { kp: kp2, required: true, sign: true },
                Input { kp: kp3, required: true, sign: false },
            ],
            3,
            false,
            true,
        );
    }
}
