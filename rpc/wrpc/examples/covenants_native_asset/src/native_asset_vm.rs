mod covenants;
mod errors;
mod result;
mod script_layout;
mod scriptnum;

use covenants::{
    build_mint_tx, build_minter_covenant_script_knat20, build_token_split_merge_tx, covenant_id_for_outpoint, minter_state_from_spk,
    token_state_from_spk, try_spk_bytes, MinterState, NativeAssetOutput, TokenInputRef, TokenState,
};
use kaspa_consensus_core::constants::TX_VERSION_POST_COV_HF;
use kaspa_consensus_core::hashing::sighash::{SigHashReusedValues, SigHashReusedValuesUnsync};
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
    VerifiableTransaction,
};
use kaspa_hashes::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::{EngineContext, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use kaspa_wallet_core::utils::try_kaspa_str_to_sompi;
use kaspa_wrpc_client::prelude::NetworkType;
use rand::{rngs::StdRng, RngCore, SeedableRng};
use std::error::Error;
use std::process::ExitCode;

const MINT_COUNT: usize = 3;
const EXTRA_AUTH_INPUTS: usize = 2;
const TOKEN_AMOUNT: u64 = 2;
const TOTAL_SUPPLY: u64 = 10;
const GENESIS_KAS: &str = "5";
const TOKEN_KAS: &str = "0.5";
const AUTH_KAS: &str = "1";

struct SpendableUtxo {
    outpoint: TransactionOutpoint,
    entry: UtxoEntry,
}

struct TokenUtxo {
    state: TokenState,
    outpoint: TransactionOutpoint,
    entry: UtxoEntry,
}

/// cargo run -p kaspa-wrpc-covenants-native-asset --bin kaspa-wrpc-covenants-native-asset-vm
fn main() -> ExitCode {
    match create_native_asset_vm_flow() {
        Ok(_) => {
            println!("Well done! You successfully ran deploy, mint, split, and merge examples locally.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("An error occurred: {error}");
            ExitCode::FAILURE
        }
    }
}

fn create_native_asset_vm_flow() -> Result<(), Box<dyn Error>> {
    let mass_calculator = MassCalculator::new_with_consensus_params(&NetworkType::Testnet.into());

    let mut rng = StdRng::from_entropy();
    let genesis_spk = random_spk(&mut rng);
    let authority_spk = random_spk(&mut rng);

    let authority_spk_bytes = try_spk_bytes(&authority_spk)?;
    let owner_spk_bytes = authority_spk_bytes.clone();

    let total_minted = TOKEN_AMOUNT.checked_mul(MINT_COUNT as u64).expect("Mint count exceeds token amount range");
    assert!(total_minted <= TOTAL_SUPPLY, "Minted supply exceeds total supply");

    println!("note: covenant scripts embed state, so script pubkeys change every spend");

    let genesis_value = try_kaspa_str_to_sompi(GENESIS_KAS.to_string()).expect("Cannot convert genesis amount").unwrap();
    let token_value = try_kaspa_str_to_sompi(TOKEN_KAS.to_string()).expect("Cannot convert token amount").unwrap();
    let auth_value = try_kaspa_str_to_sompi(AUTH_KAS.to_string()).expect("Cannot convert authority amount").unwrap();

    let authority_count = MINT_COUNT + EXTRA_AUTH_INPUTS;
    let funding_input = TransactionInput::new(TransactionOutpoint::new(Hash::from_u64_word(0), 0), vec![], 0, 0);
    let mut funding_outputs = vec![TransactionOutput::new(genesis_value, genesis_spk.clone())];
    for _ in 0..authority_count {
        funding_outputs.push(TransactionOutput::new(auth_value, authority_spk.clone()));
    }

    let mut funding_tx =
        Transaction::new(TX_VERSION_POST_COV_HF, vec![funding_input], funding_outputs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    funding_tx.finalize();
    let funding_tx_id = funding_tx.id();

    let genesis_outpoint = TransactionOutpoint::new(funding_tx.id(), 0);
    let genesis_entry = UtxoEntry::new(funding_tx.outputs[0].value, genesis_spk.clone(), 0, false, None);
    let covenant_id = covenant_id_for_outpoint(&genesis_outpoint);
    println!("covenant id: {}", covenant_id);

    let genesis_state = MinterState { covenant_id, remaining_supply: TOTAL_SUPPLY, authority_spk_bytes: authority_spk_bytes.clone() };
    let genesis_script = build_minter_covenant_script_knat20(&genesis_state)?;
    let minter_spk = ScriptPublicKey::from_vec(0, genesis_script);

    let genesis_input = TransactionInput::new(genesis_outpoint, vec![], 0, 1);
    let mut temp_genesis_output = TransactionOutput::new(genesis_entry.amount, minter_spk.clone());
    temp_genesis_output.cov_out_info = Some(covenant_info(covenant_id, 0));
    let temp_genesis_tx = Transaction::new(
        TX_VERSION_POST_COV_HF,
        vec![genesis_input.clone()],
        vec![temp_genesis_output],
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    let temp_genesis_tx = PopulatedTransaction::new(&temp_genesis_tx, vec![genesis_entry.clone()]);
    let genesis_mass = calc_mass(&mass_calculator, temp_genesis_tx);

    let minter_value = genesis_entry.amount.saturating_sub(genesis_mass);
    let mut minter_output = TransactionOutput::new(minter_value, minter_spk.clone());
    minter_output.cov_out_info = Some(covenant_info(covenant_id, 0));
    let mut minter_genesis_tx =
        Transaction::new(TX_VERSION_POST_COV_HF, vec![genesis_input], vec![minter_output], 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    minter_genesis_tx.finalize();
    let minter_genesis_tx_id = minter_genesis_tx.id();

    let mut authority_utxos = Vec::with_capacity(authority_count);
    for i in 0..authority_count {
        let output_index = 1 + i as u32;
        let output = funding_tx.outputs[output_index as usize].clone();
        authority_utxos.push(SpendableUtxo {
            outpoint: TransactionOutpoint::new(funding_tx.id(), output_index),
            entry: UtxoEntry::new(output.value, output.script_public_key, 0, false, None),
        });
    }

    let sig_cache = Cache::new(10_000);
    let reused_values = SigHashReusedValuesUnsync::new();
    let flags = EngineFlags { covenants_enabled: true, trace: true, trace_on_error: true };

    let mut minter_entry = UtxoEntry::new(minter_genesis_tx.outputs[0].value, minter_spk.clone(), 0, false, Some(covenant_id));
    let mut minter_state = minter_state_from_spk(&minter_genesis_tx.outputs[0].script_public_key)?;
    let mut minter_outpoint = TransactionOutpoint::new(minter_genesis_tx.id(), 0);

    let mut mint_tx_ids = Vec::with_capacity(MINT_COUNT);
    let mut token_utxos = Vec::with_capacity(MINT_COUNT);

    for (idx, auth_utxo) in authority_utxos.iter().take(MINT_COUNT).enumerate() {
        let auth_input = TransactionInput::new(auth_utxo.outpoint, vec![], 0, 1);
        let auth_entry = auth_utxo.entry.clone();
        let mint_tx = build_mint_tx(
            &minter_state,
            &minter_outpoint,
            &minter_entry,
            TOKEN_AMOUNT,
            &authority_spk,
            token_value,
            auth_input,
            auth_entry.clone(),
            &mass_calculator,
        )?;

        run_covenant_vm(&mint_tx, 0, vec![minter_entry.clone(), auth_entry.clone()], &reused_values, &sig_cache, flags)?;
        let mint_tx_id = mint_tx.id();
        println!("mint {} tx executed: {mint_tx_id}", idx + 1);
        mint_tx_ids.push(mint_tx_id);

        let token_outpoint = TransactionOutpoint::new(mint_tx.id(), 1);
        let token_entry =
            UtxoEntry::new(mint_tx.outputs[1].value, mint_tx.outputs[1].script_public_key.clone(), 0, false, Some(covenant_id));
        let token_state = token_state_from_spk(&mint_tx.outputs[1].script_public_key)?;
        token_utxos.push(TokenUtxo { state: token_state, outpoint: token_outpoint, entry: token_entry });

        minter_outpoint = TransactionOutpoint::new(mint_tx.id(), 0);
        minter_state = minter_state_from_spk(&mint_tx.outputs[0].script_public_key)?;
        minter_entry =
            UtxoEntry::new(mint_tx.outputs[0].value, mint_tx.outputs[0].script_public_key.clone(), 0, false, Some(covenant_id));
    }

    println!("example: deploy + mints");
    println!("funding tx simulated: {funding_tx_id}");
    println!("deploy tx simulated: {minter_genesis_tx_id}");

    let split_source = token_utxos.last().expect("No token state available for split");
    let split_amount = split_source.state.amount;
    let split_left = split_amount / 2;
    let split_right = split_amount - split_left;
    if split_left == 0 || split_right == 0 {
        return Err("Split amounts must be positive".into());
    }
    // Keep recipients identical so a single auth input can authorize the merge later.
    let split_outputs = vec![
        NativeAssetOutput { amount: split_left, recipient_spk_bytes: owner_spk_bytes.clone() },
        NativeAssetOutput { amount: split_right, recipient_spk_bytes: owner_spk_bytes.clone() },
    ];
    let split_owner_utxo = &authority_utxos[MINT_COUNT];
    let split_owner_input = TransactionInput::new(split_owner_utxo.outpoint, vec![], 0, 1);
    let split_owner_entry = split_owner_utxo.entry.clone();

    let split_tx = build_token_split_merge_tx(
        &[TokenInputRef { state: &split_source.state, outpoint: &split_source.outpoint, entry: &split_source.entry }],
        &split_outputs,
        split_owner_input,
        split_owner_entry.clone(),
        &mass_calculator,
    )?;
    run_covenant_vm(&split_tx, 0, vec![split_source.entry.clone(), split_owner_entry.clone()], &reused_values, &sig_cache, flags)?;
    let split_tx_id = split_tx.id();
    println!("example: split");
    println!("split tx executed: {split_tx_id}");

    let split_output0 = &split_tx.outputs[0];
    let split_output1 = &split_tx.outputs[1];
    let split_output0_entry =
        UtxoEntry::new(split_output0.value, split_output0.script_public_key.clone(), 0, false, Some(covenant_id));
    let split_output1_entry =
        UtxoEntry::new(split_output1.value, split_output1.script_public_key.clone(), 0, false, Some(covenant_id));
    let split_token0_state = token_state_from_spk(&split_output0.script_public_key)?;
    let split_token1_state = token_state_from_spk(&split_output1.script_public_key)?;
    let split_token_utxos = vec![
        TokenUtxo { state: split_token0_state, outpoint: TransactionOutpoint::new(split_tx.id(), 0), entry: split_output0_entry },
        TokenUtxo { state: split_token1_state, outpoint: TransactionOutpoint::new(split_tx.id(), 1), entry: split_output1_entry },
    ];

    let merge_input_amounts: Vec<u64> = split_token_utxos.iter().map(|token| token.state.amount).collect();
    let merge_total_amount: u64 = merge_input_amounts.iter().sum();
    let merge_outputs = vec![NativeAssetOutput { amount: merge_total_amount, recipient_spk_bytes: owner_spk_bytes.clone() }];

    let merge_owner_utxo = &authority_utxos[MINT_COUNT + 1];
    let merge_owner_input = TransactionInput::new(merge_owner_utxo.outpoint, vec![], 0, 1);
    let merge_owner_entry = merge_owner_utxo.entry.clone();

    let merge_tx = build_token_split_merge_tx(
        &[
            TokenInputRef {
                state: &split_token_utxos[0].state,
                outpoint: &split_token_utxos[0].outpoint,
                entry: &split_token_utxos[0].entry,
            },
            TokenInputRef {
                state: &split_token_utxos[1].state,
                outpoint: &split_token_utxos[1].outpoint,
                entry: &split_token_utxos[1].entry,
            },
        ],
        &merge_outputs,
        merge_owner_input,
        merge_owner_entry.clone(),
        &mass_calculator,
    )?;
    let merge_entries = vec![split_token_utxos[0].entry.clone(), split_token_utxos[1].entry.clone(), merge_owner_entry.clone()];
    run_covenant_vm(&merge_tx, 0, merge_entries.clone(), &reused_values, &sig_cache, flags)?;
    run_covenant_vm(&merge_tx, 1, merge_entries, &reused_values, &sig_cache, flags)?;
    let merge_tx_id = merge_tx.id();
    println!("example: merge");
    println!("merge tx executed: {merge_tx_id}");

    println!("funding tx simulated: {funding_tx_id}");
    println!("minter genesis tx simulated: {minter_genesis_tx_id}");
    println!("related transaction ids:");
    println!("funding: {funding_tx_id}");
    println!("deploy: {minter_genesis_tx_id}");
    for (index, id) in mint_tx_ids.iter().enumerate() {
        println!("mint-{}: {id}", index + 1);
    }
    println!("split: {split_tx_id}");
    println!("merge: {merge_tx_id}");

    Ok(())
}

fn random_spk(rng: &mut StdRng) -> ScriptPublicKey {
    let mut script = [0u8; 34];
    rng.fill_bytes(&mut script);
    ScriptPublicKey::from_vec(0, script.to_vec())
}

fn run_covenant_vm<Reused: SigHashReusedValues>(
    tx: &Transaction,
    input_index: usize,
    entries: Vec<UtxoEntry>,
    reused_values: &Reused,
    sig_cache: &Cache<kaspa_txscript::SigCacheKey, bool>,
    flags: EngineFlags,
) -> Result<(), TxScriptError> {
    let populated = PopulatedTransaction::new(tx, entries.clone());
    let entry = populated.utxo(input_index).ok_or_else(|| TxScriptError::InvalidInputIndex(input_index as i32, tx.inputs.len()))?;
    let covenants_ctx = build_covenants_ctx(tx, &entries);
    let engine_ctx = EngineContext::with_covenants_ctx(reused_values, sig_cache, &covenants_ctx);
    let mut vm = TxScriptEngine::from_transaction_input(&populated, &tx.inputs[input_index], input_index, entry, engine_ctx, flags);
    vm.execute()
}

fn calc_mass(calculator: &MassCalculator, tx: PopulatedTransaction<'_>) -> u64 {
    let storage_mass = calculator.calc_contextual_masses(&tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(tx.tx);

    println!("storage {}, transient {}", compute_mass, transient_mass);
    storage_mass.max(compute_mass).max(transient_mass) + 100
}

fn covenant_info(covenant_id: Hash, authorizing_input: u16) -> kaspa_consensus_core::tx::CovOutInfo {
    kaspa_consensus_core::tx::CovOutInfo { authorizing_input, covenant_id }
}

fn build_covenants_ctx(tx: &Transaction, entries: &[UtxoEntry]) -> kaspa_txscript::covenants::CovenantsContext {
    use kaspa_txscript::covenants::{CovenantGlobalContext, CovenantLocalContext, CovenantsContext};
    use std::collections::hash_map::Entry;

    let mut ctx = CovenantsContext::default();

    for (i, entry) in entries.iter().enumerate() {
        if let Some(covenant_id) = entry.covenant_id {
            match ctx.covenant_ctxs.entry(covenant_id) {
                Entry::Occupied(mut e) => e.get_mut().input_indices.push(i),
                Entry::Vacant(e) => {
                    e.insert(CovenantGlobalContext { input_indices: vec![i], output_indices: Default::default() });
                }
            }
        }
    }

    for (i, output) in tx.outputs.iter().enumerate() {
        if let Some(cov_out_info) = &output.cov_out_info {
            let auth_input = cov_out_info.authorizing_input as usize;
            let utxo_entry = entries.get(auth_input).expect("missing auth input entry");
            if let Some(covenant_id) = utxo_entry.covenant_id {
                assert_eq!(covenant_id, cov_out_info.covenant_id);
            }

            match ctx.local_ctxs.entry(auth_input) {
                Entry::Occupied(mut e) => e.get_mut().auth_outputs.push(i),
                Entry::Vacant(e) => {
                    e.insert(CovenantLocalContext { covenant_id: cov_out_info.covenant_id, auth_outputs: vec![i] });
                }
            }

            match ctx.covenant_ctxs.entry(cov_out_info.covenant_id) {
                Entry::Occupied(mut e) => e.get_mut().output_indices.push(i),
                Entry::Vacant(e) => {
                    e.insert(CovenantGlobalContext { input_indices: Default::default(), output_indices: vec![i] });
                }
            }
        }
    }

    ctx
}
