mod covenants;

use blake2b_simd::Params;
use covenants::{
    build_mint_tx, build_minter_covenant_script_knat20, build_sig_script_from_backtrace, build_token_covenant_script_knat20,
    build_token_transfer_tx, NativeAssetOp, NativeAssetPayload, NativeAssetState,
};
use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::opcodes::codes::OpTrue;
use kaspa_txscript::script_builder::ScriptBuilderResult;
use kaspa_txscript::{pay_to_script_hash_script, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;

fn main() -> ScriptBuilderResult<()> {
    mint_success_scenario()?;
    mint_bad_authority_scenario()?;
    mint_bad_token_output_scenario()?;
    mint_bad_payload_magic_scenario()?;
    mint_bad_parent_witness_scenario()?;
    mint_bad_grandparent_witness_scenario()?;
    token_transfer_success_scenario()?;
    token_transfer_wrong_owner_scenario()?;
    token_transfer_wrong_amount_scenario()?;
    token_transfer_wrong_token_id_scenario()?;
    token_transfer_wrong_output_scenario()?;
    Ok(())
}

fn mint_success_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Mint success");
    let setup = setup_minter_env()?;
    let next_payload = setup.state.payload.mint_next(1, setup.recipient_hash);
    let tx = build_mint_tx(
        &setup.state,
        &next_payload,
        &setup.minter_spk,
        &setup.token_spk,
        1_000,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.minter_script,
    );
    run_vm(&tx, vec![setup.minter_entry, setup.auth_entry]).expect("[KNAT20] Mint scenario failed");
    println!("[KNAT20] Mint success scenario complete");
    Ok(())
}

fn mint_bad_authority_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Mint fails with wrong authority input");
    let setup = setup_minter_env()?;
    let next_payload = setup.state.payload.mint_next(1, setup.recipient_hash);
    let wrong_spk = pay_to_script_hash_script(b"wrong-authority");
    let (wrong_input, wrong_entry) = build_auth_input(99, wrong_spk, 10_000);
    let tx = build_mint_tx(
        &setup.state,
        &next_payload,
        &setup.minter_spk,
        &setup.token_spk,
        1_000,
        wrong_input,
        wrong_entry.clone(),
        &setup.minter_script,
    );
    let err = run_vm(&tx, vec![setup.minter_entry, wrong_entry]).expect_err("authority mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn mint_bad_token_output_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Mint fails with wrong token output");
    let setup = setup_minter_env()?;
    let next_payload = setup.state.payload.mint_next(1, setup.recipient_hash);
    let mut tx = build_mint_tx(
        &setup.state,
        &next_payload,
        &setup.minter_spk,
        &setup.token_spk,
        1_000,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.minter_script,
    );
    tx.outputs[1].script_public_key = pay_to_script_hash_script(b"not-token");
    let err = run_vm(&tx, vec![setup.minter_entry, setup.auth_entry]).expect_err("token spk mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn mint_bad_payload_magic_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Mint fails with wrong payload magic");
    let setup = setup_minter_env()?;
    let next_payload = setup.state.payload.mint_next(1, setup.recipient_hash);
    let mut tx = build_mint_tx(
        &setup.state,
        &next_payload,
        &setup.minter_spk,
        &setup.token_spk,
        1_000,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.minter_script,
    );
    tx.payload[0] = b'B';
    let err = run_vm(&tx, vec![setup.minter_entry, setup.auth_entry]).expect_err("payload magic mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn mint_bad_parent_witness_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Mint fails with wrong parent witness");
    let setup = setup_minter_env()?;
    let next_payload = setup.state.payload.mint_next(1, setup.recipient_hash);
    let mut tx = build_mint_tx(
        &setup.state,
        &next_payload,
        &setup.minter_spk,
        &setup.token_spk,
        1_000,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.minter_script,
    );
    let mut bad_backtrace = setup.state.backtrace().clone();
    if let Some(byte) = bad_backtrace.parent_payload.first_mut() {
        *byte ^= 0x01;
    }
    let bad_sig_script = build_sig_script_from_backtrace(&bad_backtrace, &setup.minter_script);
    tx.inputs[0].signature_script = bad_sig_script;
    let err = run_vm(&tx, vec![setup.minter_entry, setup.auth_entry]).expect_err("bad parent witness should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn mint_bad_grandparent_witness_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Mint fails with wrong grandparent witness");
    let setup = setup_minter_env()?;
    let next_payload = setup.state.payload.mint_next(1, setup.recipient_hash);
    let mut tx = build_mint_tx(
        &setup.state,
        &next_payload,
        &setup.minter_spk,
        &setup.token_spk,
        1_000,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.minter_script,
    );
    let mut bad_backtrace = setup.state.backtrace().clone();
    if let Some(byte) = bad_backtrace.gp_prefix.first_mut() {
        *byte ^= 0x01;
    }
    let bad_sig_script = build_sig_script_from_backtrace(&bad_backtrace, &setup.minter_script);
    tx.inputs[0].signature_script = bad_sig_script;
    let err = run_vm(&tx, vec![setup.minter_entry, setup.auth_entry]).expect_err("bad grandparent witness should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn token_transfer_success_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Token transfer success");
    let setup = setup_token_env()?;
    let next_payload = setup.state.payload.token_transfer_next(setup.new_owner_hash);
    let tx = build_token_transfer_tx(
        &setup.state,
        &next_payload,
        &setup.token_spk,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.token_script,
    );
    run_vm(&tx, vec![setup.token_entry, setup.auth_entry]).expect("[KNAT20] Token transfer scenario failed");
    println!("[KNAT20] Token transfer success scenario complete");
    Ok(())
}

fn token_transfer_wrong_output_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Token transfer fails with wrong output");
    let setup = setup_token_env()?;
    let next_payload = setup.state.payload.token_transfer_next(setup.new_owner_hash);
    let mut tx = build_token_transfer_tx(
        &setup.state,
        &next_payload,
        &setup.token_spk,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.token_script,
    );
    tx.outputs[0].script_public_key = pay_to_script_hash_script(b"wrong-token-output");
    let err = run_vm(&tx, vec![setup.token_entry, setup.auth_entry]).expect_err("output spk mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn token_transfer_wrong_owner_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Token transfer fails with wrong owner input");
    let setup = setup_token_env()?;
    let next_payload = setup.state.payload.token_transfer_next(setup.new_owner_hash);
    let wrong_spk = pay_to_script_hash_script(b"wrong-owner");
    let (wrong_input, wrong_entry) = build_auth_input(88, wrong_spk, 5_000);
    let tx =
        build_token_transfer_tx(&setup.state, &next_payload, &setup.token_spk, wrong_input, wrong_entry.clone(), &setup.token_script);
    let err = run_vm(&tx, vec![setup.token_entry, wrong_entry]).expect_err("owner mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn token_transfer_wrong_amount_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Token transfer fails with wrong amount");
    let setup = setup_token_env()?;
    let mut next_payload = setup.state.payload.token_transfer_next(setup.new_owner_hash);
    next_payload.amount = next_payload.amount.saturating_add(1);
    let tx = build_token_transfer_tx(
        &setup.state,
        &next_payload,
        &setup.token_spk,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.token_script,
    );
    let err = run_vm(&tx, vec![setup.token_entry, setup.auth_entry]).expect_err("amount mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

fn token_transfer_wrong_token_id_scenario() -> ScriptBuilderResult<()> {
    println!("[KNAT20] Token transfer fails with wrong token id");
    let setup = setup_token_env()?;
    let mut next_payload = setup.state.payload.token_transfer_next(setup.new_owner_hash);
    next_payload.asset_id[0] ^= 0x01;
    let tx = build_token_transfer_tx(
        &setup.state,
        &next_payload,
        &setup.token_spk,
        setup.auth_input.clone(),
        setup.auth_entry.clone(),
        &setup.token_script,
    );
    let err = run_vm(&tx, vec![setup.token_entry, setup.auth_entry]).expect_err("token id mismatch should fail");
    println!("[KNAT20] Expected failure: {err:?}");
    Ok(())
}

struct MinterEnv {
    state: NativeAssetState,
    minter_entry: UtxoEntry,
    minter_spk: ScriptPublicKey,
    minter_script: Vec<u8>,
    token_spk: ScriptPublicKey,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    recipient_hash: [u8; 32],
}

fn setup_minter_env() -> ScriptBuilderResult<MinterEnv> {
    let token_script = vec![OpTrue];
    let token_spk = pay_to_script_hash_script(&token_script);
    let token_spk_hash = spk_script_hash(&token_spk);

    let auth_spk = pay_to_script_hash_script(b"minter-auth");
    let (auth_input, auth_entry) = build_auth_input(42, auth_spk.clone(), 10_000);
    let authority_hash = spk_script_hash(&auth_spk);
    let recipient_hash = authority_hash;
    let authority_spk_bytes = spk_to_bytes(&auth_spk);

    let minter_script = build_minter_covenant_script_knat20(token_spk_hash, &authority_spk_bytes)?;
    let minter_spk = pay_to_script_hash_script(&minter_script);

    let (state, minter_entry, _parent, _grandparent) =
        build_minter_genesis_state(&minter_spk, &token_spk, 50_000, authority_hash, recipient_hash, 10);

    Ok(MinterEnv { state, minter_entry, minter_spk, minter_script, token_spk, auth_input, auth_entry, recipient_hash })
}

struct TokenEnv {
    state: NativeAssetState,
    token_entry: UtxoEntry,
    token_spk: ScriptPublicKey,
    token_script: Vec<u8>,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    new_owner_hash: [u8; 32],
}

fn setup_token_env() -> ScriptBuilderResult<TokenEnv> {
    let token_script = build_token_covenant_script_knat20([0u8; 32])?;
    let token_spk = pay_to_script_hash_script(&token_script);

    let auth_spk = pay_to_script_hash_script(b"token-owner");
    let (auth_input, auth_entry) = build_auth_input(77, auth_spk.clone(), 5_000);
    let owner_hash = spk_script_hash(&auth_spk);
    let new_owner_hash = spk_script_hash(&pay_to_script_hash_script(b"new-owner"));

    let (state, token_entry, _parent, _grandparent) = build_token_chain(&token_spk, 25_000, owner_hash, 3);

    Ok(TokenEnv { state, token_entry, token_spk, token_script, auth_input, auth_entry, new_owner_hash })
}

fn build_minter_genesis_state(
    minter_spk: &ScriptPublicKey,
    grandparent_spk: &ScriptPublicKey,
    amount: u64,
    authority_hash: [u8; 32],
    recipient_hash: [u8; 32],
    remaining_supply: u8,
) -> (NativeAssetState, UtxoEntry, Transaction, Transaction) {
    let grandparent_tx = build_simple_tx(grandparent_spk.clone(), amount, 1);
    let outpoint = TransactionOutpoint::new(grandparent_tx.id(), 0);
    let asset_id = asset_id_for_outpoint(&outpoint);
    let payload =
        NativeAssetPayload { asset_id, authority_hash, remaining_supply, seq: 0, op: NativeAssetOp::Mint, amount: 1, recipient_hash };
    let parent_tx = build_spend_tx(&grandparent_tx, minter_spk.clone(), amount, payload.encode());
    let minter_entry = UtxoEntry::new(amount, minter_spk.clone(), 0, false);
    let state = NativeAssetState::from_tx_with_entry_and_grandparent(parent_tx.clone(), minter_entry.clone(), &grandparent_tx);
    (state, minter_entry, parent_tx, grandparent_tx)
}

fn build_token_chain(
    token_spk: &ScriptPublicKey,
    amount: u64,
    owner_hash: [u8; 32],
    token_amount: u8,
) -> (NativeAssetState, UtxoEntry, Transaction, Transaction) {
    let grandparent_tx = build_simple_tx(token_spk.clone(), amount, 2);
    let outpoint = TransactionOutpoint::new(grandparent_tx.id(), 0);
    let asset_id = asset_id_for_outpoint(&outpoint);
    let payload = NativeAssetPayload {
        asset_id,
        authority_hash: [2u8; 32],
        remaining_supply: 10,
        seq: 0,
        op: NativeAssetOp::TokenTransfer,
        amount: token_amount,
        recipient_hash: owner_hash,
    };
    let parent_tx = build_spend_tx(&grandparent_tx, token_spk.clone(), amount, payload.encode());
    let token_entry = UtxoEntry::new(amount, token_spk.clone(), 0, false);
    let state = NativeAssetState::from_tx_with_entry_and_grandparent(parent_tx.clone(), token_entry.clone(), &grandparent_tx);
    (state, token_entry, parent_tx, grandparent_tx)
}

fn build_simple_tx(spk: ScriptPublicKey, amount: u64, seed: u64) -> Transaction {
    let input = TransactionInput::new(TransactionOutpoint::new(Hash::from_u64_word(seed), 0), vec![], 0, 0);
    let output = TransactionOutput::new(amount, spk);
    let mut tx = Transaction::new(0, vec![input], vec![output], 0, SubnetworkId::default(), 0, vec![]);
    tx.finalize();
    tx
}

fn build_spend_tx(parent_tx: &Transaction, spk: ScriptPublicKey, amount: u64, payload: Vec<u8>) -> Transaction {
    let input = TransactionInput::new(TransactionOutpoint::new(parent_tx.id(), 0), vec![], 0, 0);
    let output = TransactionOutput::new(amount, spk);
    let mut tx = Transaction::new(0, vec![input], vec![output], 0, SubnetworkId::default(), 0, payload);
    tx.finalize();
    tx
}

fn build_auth_input(seed: u64, spk: ScriptPublicKey, amount: u64) -> (TransactionInput, UtxoEntry) {
    let input = TransactionInput::new(TransactionOutpoint::new(Hash::from_u64_word(seed), 0), vec![], 0, 0);
    let entry = UtxoEntry::new(amount, spk, 0, false);
    (input, entry)
}

fn asset_id_for_outpoint(outpoint: &TransactionOutpoint) -> [u8; 32] {
    let mut data = Vec::with_capacity(36);
    data.extend_from_slice(outpoint.transaction_id.as_ref());
    data.extend_from_slice(&outpoint.index.to_le_bytes());
    blake2b_32(&data)
}

fn spk_script_hash(spk: &ScriptPublicKey) -> [u8; 32] {
    blake2b_32(spk.script())
}

fn spk_to_bytes(spk: &ScriptPublicKey) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + spk.script().len());
    bytes.extend_from_slice(&spk.version().to_be_bytes());
    bytes.extend_from_slice(spk.script());
    bytes
}

fn blake2b_32(data: &[u8]) -> [u8; 32] {
    let hash = Params::new().hash_length(32).to_state().update(data).finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    out
}

fn run_vm(tx: &Transaction, entries: Vec<UtxoEntry>) -> Result<(), TxScriptError> {
    let populated = PopulatedTransaction::new(tx, entries);
    let entry = populated.entries[0].clone();
    let sig_cache = Cache::new(10_000);
    let reused_values = SigHashReusedValuesUnsync::new();
    let mut vm = TxScriptEngine::from_transaction_input(
        &populated,
        &tx.inputs[0],
        0,
        &entry,
        &reused_values,
        &sig_cache,
        EngineFlags { covenants_enabled: true },
    );
    vm.execute()
}
