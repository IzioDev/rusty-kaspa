use kaspa_consensus_core::config::params::TESTNET_PARAMS;
use kaspa_consensus_core::{
    Hash,
    hashing::sighash::{SigHashReusedValuesUnsync, calc_schnorr_signature_hash},
    tx::{TransactionId, TransactionOutpoint, UtxoEntry},
};
use kaspa_txscript::{
    MultisigRedeemScriptContext, multisig_redeem_script, opcodes::codes::OpData65, parse_multisig_redeem_script,
    pay_to_script_hash_script, script_builder::ScriptBuilder,
};
use kaspa_wallet_pskt::prelude::{
    Combiner, Creator, Extractor, Finalizer, Inner, Input, InputBuilder, PSKT, SignInputOk, Signature, SignatureScheme, Signer,
    Updater,
};
use secp256k1::{Keypair, rand::thread_rng};
use std::{iter, ops::Deref, str::FromStr};

fn main() {
    let kps = [Keypair::new(secp256k1::SECP256K1, &mut thread_rng()), Keypair::new(secp256k1::SECP256K1, &mut thread_rng())];
    let redeem_script = multisig_redeem_script(kps.iter().map(|pk| pk.x_only_public_key().0.serialize()), 2).unwrap();
    // Create the PSKT.
    let created = PSKT::<Creator>::default().inputs_modifiable().outputs_modifiable();
    let ser = serde_json::to_string_pretty(&created).expect("Failed to serialize after creation");
    println!("Serialized after creation: {}", ser);

    // The first constructor entity receives the PSKT and adds an input.
    let pskt: PSKT<Creator> = serde_json::from_str(&ser).expect("Failed to deserialize");
    // let in_0 = dummy_out_point();
    let input_0 = InputBuilder::default()
        .utxo_entry(UtxoEntry {
            amount: 12793000000000,
            script_public_key: pay_to_script_hash_script(&redeem_script),
            block_daa_score: 36151168,
            is_coinbase: false,
            covenant_id: Default::default(),
        })
        .previous_outpoint(TransactionOutpoint {
            transaction_id: TransactionId::from_str("63020db736215f8b1105a9281f7bcbb6473d965ecc45bb2fb5da59bd35e6ff84").unwrap(),
            index: 0,
        })
        .sig_op_count(2)
        .redeem_script(redeem_script)
        .build()
        .unwrap();
    let pskt_in0 = pskt.constructor().input(input_0);
    let ser_in_0 = serde_json::to_string_pretty(&pskt_in0).expect("Failed to serialize after adding first input");
    println!("Serialized after adding first input: {}", ser_in_0);

    let combiner_pskt: PSKT<Combiner> = serde_json::from_str(&ser).expect("Failed to deserialize");
    let combined_pskt = combiner_pskt.combine(pskt_in0).unwrap();
    let ser_combined = serde_json::to_string_pretty(&combined_pskt).expect("Failed to serialize after adding output");
    println!("Serialized after combining: {}", ser_combined);

    // The PSKT is now ready for handling with the updater role.
    let updater_pskt: PSKT<Updater> = serde_json::from_str(&ser_combined).expect("Failed to deserialize");
    let updater_pskt = updater_pskt.set_sequence(u64::MAX, 0).expect("Failed to set sequence");
    let ser_updated = serde_json::to_string_pretty(&updater_pskt).expect("Failed to serialize after setting sequence");
    println!("Serialized after setting sequence: {}", ser_updated);

    let signer_pskt: PSKT<Signer> = serde_json::from_str(&ser_updated).expect("Failed to deserialize");
    let reused_values = SigHashReusedValuesUnsync::new();
    let sign = |signer_pskt: PSKT<Signer>, kp: &Keypair| {
        signer_pskt
            .pass_signature_sync(|tx, sighash| -> Result<Vec<SignInputOk>, String> {
                tx.tx
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(idx, _input)| {
                        let hash = calc_schnorr_signature_hash(&tx.as_verifiable(), idx, sighash[idx], &reused_values);
                        let msg = secp256k1::Message::from_digest_slice(hash.as_bytes().as_slice()).unwrap();
                        Ok(SignInputOk {
                            signature: Signature::Schnorr(kp.sign_schnorr(msg)),
                            pub_key: kp.public_key(),
                            key_source: None,
                        })
                    })
                    .collect()
            })
            .unwrap()
    };
    let signed_0 = sign(signer_pskt.clone(), &kps[0]);
    let signed_1 = sign(signer_pskt, &kps[1]);
    let combiner_pskt: PSKT<Combiner> = serde_json::from_str(&ser_updated).expect("Failed to deserialize");
    let combined_signed = combiner_pskt.combine(signed_0).and_then(|combined| combined.combine(signed_1)).unwrap();
    let ser_combined_signed = serde_json::to_string_pretty(&combined_signed).expect("Failed to serialize after combining signed");
    println!("Combined Signed: {}", ser_combined_signed);
    let pskt_finalizer: PSKT<Finalizer> = serde_json::from_str(&ser_combined_signed).expect("Failed to deserialize");
    let pskt_finalizer = finalize_p2sh_multisig_schnorr(pskt_finalizer).unwrap();
    let ser_finalized = serde_json::to_string_pretty(&pskt_finalizer).expect("Failed to serialize after finalizing");
    println!("Finalized: {}", ser_finalized);

    let extractor_pskt: PSKT<Extractor> = serde_json::from_str(&ser_finalized).expect("Failed to deserialize");
    let params = TESTNET_PARAMS;
    let tx = extractor_pskt.extract_tx(&params).unwrap().tx;
    let ser_tx = serde_json::to_string_pretty(&tx).unwrap();
    println!("Tx: {}", ser_tx);
}

fn finalize_p2sh_multisig_schnorr(pskt: PSKT<Finalizer>) -> Result<PSKT<Finalizer>, String> {
    let signature_hashes =
        PSKT::<Signer>::from(pskt.deref().clone()).signature_hashes(SignatureScheme::Schnorr).map_err(|error| error.to_string())?;

    pskt.finalize_sync(|inner| final_scripts_for_p2sh_multisig_schnorr(inner, &signature_hashes)).map_err(|error| error.to_string())
}

fn final_scripts_for_p2sh_multisig_schnorr(inner: &Inner, signature_hashes: &[Hash]) -> Result<Vec<Vec<u8>>, String> {
    if signature_hashes.len() != inner.inputs.len() {
        return Err(format!("Signature hash count mismatch: expected {}, actual {}", inner.inputs.len(), signature_hashes.len()));
    }

    inner
        .inputs
        .iter()
        .zip(signature_hashes)
        .map(|(input, signature_hash)| final_script_for_p2sh_multisig_schnorr(input, signature_hash))
        .collect()
}

fn final_script_for_p2sh_multisig_schnorr(input: &Input, signature_hash: &Hash) -> Result<Vec<u8>, String> {
    let redeem_script = input.redeem_script.as_ref().ok_or_else(|| "Missing redeem script".to_string())?;
    let context = parse_multisig_redeem_script(redeem_script).map_err(|error| error.to_string())?;

    let MultisigRedeemScriptContext::Schnorr { required, public_keys } = context else {
        return Err("Unsupported multisig signature type".to_string());
    };

    let message = secp256k1::Message::from_digest_slice(signature_hash.as_bytes().as_slice()).map_err(|error| error.to_string())?;

    for (signature_public_key, signature) in input.partial_sigs.iter() {
        if !public_keys.iter().any(|public_key| *public_key == signature_public_key.x_only_public_key().0) {
            return Err(format!("Partial signature public key {signature_public_key} is not a member"));
        }
        if !matches!(signature, Signature::Schnorr(_)) {
            return Err("Unsupported partial signature type".to_string());
        }
        signature
            .verify(&message, signature_public_key)
            .map_err(|_| format!("Invalid partial signature for public key {signature_public_key}"))?;
    }

    let mut selected = Vec::with_capacity(required);
    for public_key in public_keys.iter() {
        if let Some((_, signature)) =
            input.partial_sigs.iter().find(|(signature_public_key, _)| signature_public_key.x_only_public_key().0 == *public_key)
        {
            selected.push(*signature);
        }
        if selected.len() == required {
            break;
        }
    }

    if selected.len() < required {
        return Err(format!("Insufficient signatures: required {}, present {}", required, selected.len()));
    }

    let mut script = Vec::new();
    for signature in selected {
        script.extend(iter::once(OpData65).chain(signature.into_bytes()).chain([input.sighash_type.to_u8()]));
    }
    script.extend(ScriptBuilder::new().add_data(redeem_script).map_err(|error| error.to_string())?.drain());
    Ok(script)
}
