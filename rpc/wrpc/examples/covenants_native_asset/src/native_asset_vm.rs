mod covenants;
mod errors;
mod payload_layout;
mod result;
mod scriptnum;

use covenants::{
    asset_id_for_outpoint, build_mint_tx, build_minter_covenant_script_knat20, build_sig_script_from_backtrace,
    build_token_covenant_script_knat20, try_spk_bytes, KnatBacktrace, NativeAssetOp, NativeAssetOutput, NativeAssetPayload,
    NativeAssetState,
};
use kaspa_addresses::Prefix;
use kaspa_consensus_core::constants::TX_VERSION;
use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
    VerifiableTransaction,
};
use kaspa_hashes::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::{extract_script_pub_key_address, pay_to_script_hash_script, EngineFlags, TxScriptEngine};
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
    state: NativeAssetState,
    parent_tx: Transaction,
    grandparent_tx: Transaction,
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

    let minter_covenant_script = build_minter_covenant_script_knat20(&authority_spk_bytes)?;
    let minter_spk = pay_to_script_hash_script(&minter_covenant_script);
    let minter_spk_bytes = try_spk_bytes(&minter_spk)?;

    let token_covenant_script = build_token_covenant_script_knat20(&minter_spk_bytes)?;
    let token_spk = pay_to_script_hash_script(&token_covenant_script);
    let token_spk_bytes = try_spk_bytes(&token_spk)?;

    let minter_address =
        extract_script_pub_key_address(&minter_spk, Prefix::Testnet).expect("Cannot get address from minter covenant spk");
    let token_address =
        extract_script_pub_key_address(&token_spk, Prefix::Testnet).expect("Cannot get address from token covenant spk");

    println!("minter covenant address: {}", minter_address);
    println!("token covenant address: {}", token_address);

    let genesis_value = try_kaspa_str_to_sompi(GENESIS_KAS.to_string()).expect("Cannot convert genesis amount").unwrap();
    let token_value = try_kaspa_str_to_sompi(TOKEN_KAS.to_string()).expect("Cannot convert token amount").unwrap();
    let auth_value = try_kaspa_str_to_sompi(AUTH_KAS.to_string()).expect("Cannot convert authority amount").unwrap();

    let authority_count = MINT_COUNT + EXTRA_AUTH_INPUTS;
    let funding_input = TransactionInput::new(TransactionOutpoint::new(Hash::from_u64_word(0), 0), vec![], 0, 0);
    let mut funding_outputs = vec![TransactionOutput::new(genesis_value, genesis_spk.clone())];
    for _ in 0..authority_count {
        funding_outputs.push(TransactionOutput::new(auth_value, authority_spk.clone()));
    }

    let mut funding_tx = Transaction::new(TX_VERSION, vec![funding_input], funding_outputs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    funding_tx.finalize();
    let funding_tx_id = funding_tx.id();

    let genesis_outpoint = TransactionOutpoint::new(funding_tx.id(), 0);
    let genesis_entry = UtxoEntry::new(funding_tx.outputs[0].value, genesis_spk.clone(), 0, false);

    let genesis_payload = NativeAssetPayload {
        asset_id: asset_id_for_outpoint(&genesis_outpoint),
        authority_spk_bytes,
        token_spk_bytes,
        remaining_supply: TOTAL_SUPPLY,
        op: NativeAssetOp::Mint,
        total_amount: TOKEN_AMOUNT,
        input_amounts: Vec::new(),
        outputs: vec![NativeAssetOutput { amount: TOKEN_AMOUNT, recipient_spk_bytes: owner_spk_bytes.clone() }],
    };
    let genesis_payload_bytes = genesis_payload.encode()?;

    let genesis_input = TransactionInput::new(genesis_outpoint, vec![], 0, 1);
    let temp_genesis_output = TransactionOutput::new(genesis_entry.amount, minter_spk.clone());
    let temp_genesis_tx = Transaction::new(
        TX_VERSION,
        vec![genesis_input.clone()],
        vec![temp_genesis_output],
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        genesis_payload_bytes.clone(),
    );
    let temp_genesis_tx = PopulatedTransaction::new(&temp_genesis_tx, vec![genesis_entry.clone()]);
    let genesis_mass = calc_mass(&mass_calculator, temp_genesis_tx);

    let minter_value = genesis_entry.amount.saturating_sub(genesis_mass);
    let minter_output = TransactionOutput::new(minter_value, minter_spk.clone());
    let mut minter_genesis_tx =
        Transaction::new(TX_VERSION, vec![genesis_input], vec![minter_output], 0, SUBNETWORK_ID_NATIVE, 0, genesis_payload_bytes);
    minter_genesis_tx.finalize();
    let minter_genesis_tx_id = minter_genesis_tx.id();

    let mut authority_utxos = Vec::with_capacity(authority_count);
    for i in 0..authority_count {
        let output_index = 1 + i as u32;
        let output = funding_tx.outputs[output_index as usize].clone();
        authority_utxos.push(SpendableUtxo {
            outpoint: TransactionOutpoint::new(funding_tx.id(), output_index),
            entry: UtxoEntry::new(output.value, output.script_public_key, 0, false),
        });
    }

    let sig_cache = Cache::new(10_000);
    let reused_values = SigHashReusedValuesUnsync::new();
    let flags = EngineFlags { covenants_enabled: true };

    let minter_entry = UtxoEntry::new(minter_genesis_tx.outputs[0].value, minter_spk.clone(), 0, false);
    let mut minter_state = NativeAssetState::from_tx_with_entry_and_grandparent(minter_genesis_tx.clone(), minter_entry, &funding_tx)?;
    let mut minter_parent_tx = minter_genesis_tx.clone();

    let mut mint_tx_ids = Vec::with_capacity(MINT_COUNT);
    let mut token_utxos = Vec::with_capacity(MINT_COUNT);

    for (idx, auth_utxo) in authority_utxos.iter().take(MINT_COUNT).enumerate() {
        let auth_input = TransactionInput::new(auth_utxo.outpoint, vec![], 0, 1);
        let auth_entry = auth_utxo.entry.clone();
        let next_payload = minter_state.payload.mint_next(TOKEN_AMOUNT, &owner_spk_bytes)?;

        let mint_tx = build_mint_tx(
            &minter_state,
            &next_payload,
            &minter_spk,
            &token_spk,
            token_value,
            auth_input,
            auth_entry.clone(),
            &minter_covenant_script,
            &mass_calculator,
        )?;

        run_covenant_vm(&mint_tx, 0, vec![minter_state.utxo_entry().clone(), auth_entry.clone()], &sig_cache, &reused_values, flags)?;
        let mint_tx_id = mint_tx.id();
        println!("mint {} tx executed: {mint_tx_id}", idx + 1);
        mint_tx_ids.push(mint_tx_id);

        let token_grandparent_tx = minter_parent_tx.clone();
        let token_entry = UtxoEntry::new(mint_tx.outputs[1].value, token_spk.clone(), 0, false);
        let token_state =
            NativeAssetState::from_tx_with_entry_and_grandparent_at_index(mint_tx.clone(), token_entry, &token_grandparent_tx, 1)?;
        token_utxos.push(TokenUtxo { state: token_state, parent_tx: mint_tx.clone(), grandparent_tx: token_grandparent_tx });

        let next_minter_entry = UtxoEntry::new(mint_tx.outputs[0].value, minter_spk.clone(), 0, false);
        minter_state = NativeAssetState::from_tx_with_entry_and_grandparent(mint_tx.clone(), next_minter_entry, &minter_parent_tx)?;
        minter_parent_tx = mint_tx;
    }

    println!("example: deploy + mints");
    println!("funding tx simulated: {funding_tx_id}");
    println!("deploy tx simulated: {minter_genesis_tx_id}");

    let split_source = token_utxos.last().expect("No token state available for split");
    let split_amount = token_amount_for_state(&split_source.state)?;
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
    let split_payload = split_source.state.payload.split_merge_next(&[split_amount], &split_outputs)?;
    let split_owner_utxo = &authority_utxos[MINT_COUNT];
    let split_owner_input = TransactionInput::new(split_owner_utxo.outpoint, vec![], 0, 1);
    let split_owner_entry = split_owner_utxo.entry.clone();

    let split_tx = build_token_split_merge_tx(
        std::slice::from_ref(split_source),
        &split_payload,
        &token_spk,
        split_owner_input,
        split_owner_entry.clone(),
        &token_covenant_script,
        &mass_calculator,
    )?;
    run_covenant_vm(
        &split_tx,
        0,
        vec![split_source.state.utxo_entry().clone(), split_owner_entry.clone()],
        &sig_cache,
        &reused_values,
        flags,
    )?;
    let split_tx_id = split_tx.id();
    println!("example: split");
    println!("split tx executed: {split_tx_id}");

    let split_grandparent_tx = split_source.parent_tx.clone();
    let split_output0 = &split_tx.outputs[0];
    let split_output1 = &split_tx.outputs[1];
    let split_output0_entry = UtxoEntry::new(split_output0.value, split_output0.script_public_key.clone(), 0, false);
    let split_output1_entry = UtxoEntry::new(split_output1.value, split_output1.script_public_key.clone(), 0, false);
    let split_token0_state =
        NativeAssetState::from_tx_with_entry_and_grandparent_at_index(split_tx.clone(), split_output0_entry, &split_grandparent_tx, 0)?;
    let split_token1_state =
        NativeAssetState::from_tx_with_entry_and_grandparent_at_index(split_tx.clone(), split_output1_entry, &split_grandparent_tx, 1)?;
    let split_token_utxos = vec![
        TokenUtxo { state: split_token0_state, parent_tx: split_tx.clone(), grandparent_tx: split_grandparent_tx.clone() },
        TokenUtxo { state: split_token1_state, parent_tx: split_tx.clone(), grandparent_tx: split_grandparent_tx },
    ];

    let merge_input_amounts: Vec<u64> =
        split_token_utxos.iter().map(|token| token_amount_for_state(&token.state)).collect::<Result<_, _>>()?;
    let merge_total_amount: u64 = merge_input_amounts.iter().sum();
    let merge_outputs = vec![NativeAssetOutput { amount: merge_total_amount, recipient_spk_bytes: owner_spk_bytes.clone() }];
    let merge_payload = split_token_utxos[0].state.payload.split_merge_next(&merge_input_amounts, &merge_outputs)?;

    let merge_owner_utxo = &authority_utxos[MINT_COUNT + 1];
    let merge_owner_input = TransactionInput::new(merge_owner_utxo.outpoint, vec![], 0, 1);
    let merge_owner_entry = merge_owner_utxo.entry.clone();

    let merge_tx = build_token_split_merge_tx(
        &split_token_utxos,
        &merge_payload,
        &token_spk,
        merge_owner_input,
        merge_owner_entry.clone(),
        &token_covenant_script,
        &mass_calculator,
    )?;
    let merge_entries = vec![
        split_token_utxos[0].state.utxo_entry().clone(),
        split_token_utxos[1].state.utxo_entry().clone(),
        merge_owner_entry.clone(),
    ];
    run_covenant_vm(&merge_tx, 0, merge_entries.clone(), &sig_cache, &reused_values, flags)?;
    run_covenant_vm(&merge_tx, 1, merge_entries, &sig_cache, &reused_values, flags)?;
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

fn run_covenant_vm(
    tx: &Transaction,
    input_index: usize,
    entries: Vec<UtxoEntry>,
    sig_cache: &Cache<kaspa_txscript::SigCacheKey, bool>,
    reused_values: &SigHashReusedValuesUnsync,
    flags: EngineFlags,
) -> Result<(), TxScriptError> {
    let populated = PopulatedTransaction::new(tx, entries);
    let entry = populated.utxo(input_index).ok_or_else(|| TxScriptError::InvalidInputIndex(input_index as i32, tx.inputs.len()))?;
    let mut vm = TxScriptEngine::from_transaction_input(
        &populated,
        &tx.inputs[input_index],
        input_index,
        entry,
        reused_values,
        sig_cache,
        flags,
    );
    vm.execute()
}

fn token_amount_for_state(state: &NativeAssetState) -> Result<u64, Box<dyn Error>> {
    match state.payload.op {
        NativeAssetOp::Mint => state
            .payload
            .outputs
            .get(0)
            .map(|output| output.amount)
            .ok_or_else(|| "Missing mint output amount".into()),
        NativeAssetOp::SplitMerge => {
            let output_index = state.utxo_outpoint().index as usize;
            state
                .payload
                .outputs
                .get(output_index)
                .map(|output| output.amount)
                .ok_or_else(|| "Missing split/merge output amount".into())
        }
    }
}

fn build_token_split_merge_tx(
    token_inputs: &[TokenUtxo],
    next_payload: &NativeAssetPayload,
    token_spk: &ScriptPublicKey,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    token_covenant_script: &[u8],
    mass_calculator: &MassCalculator,
) -> Result<Transaction, Box<dyn Error>> {
    if token_inputs.is_empty() {
        return Err("Split/merge tx requires at least one token input".into());
    }
    let output_count = next_payload.outputs.len();
    if output_count == 0 {
        return Err("Split/merge tx requires at least one output".into());
    }

    let payload = next_payload.encode()?;
    let mut inputs = Vec::with_capacity(token_inputs.len() + 1);
    let mut entries = Vec::with_capacity(token_inputs.len() + 1);

    for token in token_inputs {
        let backtrace = KnatBacktrace::from_parent_and_grandparent(&token.parent_tx, &token.grandparent_tx)?;
        let sig_script = build_sig_script_from_backtrace(&backtrace, token_covenant_script)?;
        inputs.push(TransactionInput::new(*token.state.utxo_outpoint(), sig_script, 0, 0));
        entries.push(token.state.utxo_entry().clone());
    }

    inputs.push(auth_input.clone());
    entries.push(auth_entry.clone());

    let total_input_value: u64 = token_inputs.iter().map(|token| token.state.utxo_entry().amount).sum();
    let temp_values = even_split_values(total_input_value, output_count)?;
    let temp_outputs: Vec<TransactionOutput> =
        temp_values.into_iter().map(|value| TransactionOutput::new(value, token_spk.clone())).collect();
    let temp_tx = Transaction::new(TX_VERSION, inputs.clone(), temp_outputs, 0, SUBNETWORK_ID_NATIVE, 0, payload.clone());
    let temp_tx = PopulatedTransaction::new(&temp_tx, entries.clone());
    let mass = calc_mass(mass_calculator, temp_tx);

    let available_value = total_input_value.saturating_sub(mass);
    let output_values = allocate_output_values_by_amount(available_value, &next_payload.outputs)?;
    let outputs: Vec<TransactionOutput> =
        output_values.into_iter().map(|value| TransactionOutput::new(value, token_spk.clone())).collect();

    let mut tx = Transaction::new(TX_VERSION, inputs, outputs, 0, SUBNETWORK_ID_NATIVE, 0, payload);
    tx.finalize();
    Ok(tx)
}

fn even_split_values(total: u64, parts: usize) -> Result<Vec<u64>, Box<dyn Error>> {
    if parts == 0 {
        return Err("Output count must be positive".into());
    }
    let base = total / parts as u64;
    let remainder = total - base * parts as u64;
    let mut values = vec![base; parts];
    if let Some(last) = values.last_mut() {
        *last = last.saturating_add(remainder);
    }
    if values.iter().any(|&value| value == 0) {
        return Err("Output values must be positive".into());
    }
    Ok(values)
}

fn allocate_output_values_by_amount(total: u64, outputs: &[NativeAssetOutput]) -> Result<Vec<u64>, Box<dyn Error>> {
    if outputs.is_empty() {
        return Err("Output count must be positive".into());
    }
    let total_amount: u64 = outputs.iter().map(|output| output.amount).sum();
    if total_amount == 0 {
        return Err("Total output amount must be positive".into());
    }

    let mut values = Vec::with_capacity(outputs.len());
    let mut allocated = 0u64;
    for (index, output) in outputs.iter().enumerate() {
        let value = if index + 1 == outputs.len() {
            total.saturating_sub(allocated)
        } else {
            let value = total.saturating_mul(output.amount) / total_amount;
            allocated = allocated.saturating_add(value);
            value
        };
        values.push(value);
    }
    if values.iter().any(|&value| value == 0) {
        return Err("Output values must be positive".into());
    }
    Ok(values)
}

fn calc_mass(calculator: &MassCalculator, tx: PopulatedTransaction<'_>) -> u64 {
    let storage_mass = calculator.calc_contextual_masses(&tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(tx.tx);

    println!("storage {}, transient {}", compute_mass, transient_mass);
    storage_mass.max(compute_mass).max(transient_mass) + 100
}
