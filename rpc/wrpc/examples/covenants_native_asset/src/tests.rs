use super::*;
use crate::covenants::{
    MinterState, NativeAssetOutput, TokenInputRef, build_mint_tx, build_minter_covenant_script_knat20, build_token_split_merge_tx,
    minter_state_from_spk, token_state_from_spk, try_spk_bytes,
};
use crate::script_layout::{AMOUNT_LEN, SPK_AMOUNT_OFFSET};
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::mass::MassCalculator;
use kaspa_consensus_core::tx::{
    SCRIPT_VECTOR_SIZE, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_txscript::EngineFlags;
use kaspa_txscript::caches::Cache;
use kaspa_txscript_errors::TxScriptError;
use kaspa_wrpc_client::prelude::NetworkType;
use rand::{RngCore, SeedableRng, rngs::StdRng};

const TEST_TOTAL_SUPPLY: u64 = 20;

struct Harness {
    mass_calculator: MassCalculator,
    sig_cache: Cache<kaspa_txscript::SigCacheKey, bool>,
    reused_values: SigHashReusedValuesUnsync,
    flags: EngineFlags,
    covenant_id: Hash,
    minter_state: MinterState,
    minter_outpoint: TransactionOutpoint,
    minter_entry: UtxoEntry,
    authority_spk: ScriptPublicKey,
    authority_spk_bytes: Vec<u8>,
    next_auth_index: usize,
    rng: StdRng,
}

impl Harness {
    fn new() -> Self {
        let mut rng = StdRng::seed_from_u64(110_165_000);
        let authority_spk = random_spk(&mut rng);
        let authority_spk_bytes = try_spk_bytes(&authority_spk).expect("authority spk bytes");

        let seed_outpoint = TransactionOutpoint::new(Hash::from_u64_word(10), 0);

        let covenant_id = covenant_id(seed_outpoint, std::iter::empty());

        let minter_state = MinterState { remaining_supply: TEST_TOTAL_SUPPLY, authority_spk_bytes: authority_spk_bytes.clone() };

        let minter_script = build_minter_covenant_script_knat20(&minter_state).expect("minter script");
        let minter_spk = ScriptPublicKey::from_vec(0, minter_script);
        let minter_entry = UtxoEntry::new(10_000_000_000, minter_spk, 0, false, Some(covenant_id));

        Self {
            mass_calculator: MassCalculator::new_with_consensus_params(&NetworkType::Testnet.into()),
            sig_cache: Cache::new(10_000),
            reused_values: SigHashReusedValuesUnsync::new(),
            flags: EngineFlags { covenants_enabled: true },
            covenant_id,
            minter_state,
            minter_outpoint: seed_outpoint,
            minter_entry,
            authority_spk,
            authority_spk_bytes,
            next_auth_index: 0,
            rng,
        }
    }

    fn get_auth_utxo(&mut self) -> SpendableUtxo {
        self.next_auth_index += 1;
        SpendableUtxo {
            outpoint: TransactionOutpoint::new(Hash::from_u64_word(100 * self.next_auth_index as u64), 0),
            entry: UtxoEntry::new(1_000_000, self.authority_spk.clone(), 0, false, None),
        }
    }

    fn random_spk(&mut self) -> ScriptPublicKey {
        let mut script = [0u8; 34];
        self.rng.fill_bytes(&mut script);
        ScriptPublicKey::from_vec(0, script.to_vec())
    }

    fn mint_to(&mut self, amount: u64, recipient_spk: &ScriptPublicKey) -> TokenUtxo {
        let auth_utxo = self.get_auth_utxo();
        let auth_input = TransactionInput::new(auth_utxo.outpoint, vec![], 0, 1);
        let auth_entry = auth_utxo.entry.clone();

        let mint_tx = build_mint_tx(
            &self.minter_state,
            &self.covenant_id,
            &self.minter_outpoint,
            &self.minter_entry,
            amount,
            recipient_spk,
            100_000_000,
            auth_input,
            auth_entry.clone(),
            &self.mass_calculator,
        )
        .expect("mint tx");

        run_covenant_vm(
            &mint_tx,
            0,
            vec![self.minter_entry.clone(), auth_entry.clone()],
            &self.reused_values,
            &self.sig_cache,
            self.flags,
        )
        .expect("mint covenant");

        let token_outpoint = TransactionOutpoint::new(mint_tx.id(), 1);
        let token_entry =
            UtxoEntry::new(mint_tx.outputs[1].value, mint_tx.outputs[1].script_public_key.clone(), 0, false, Some(self.covenant_id));
        let token_state = token_state_from_spk(&mint_tx.outputs[1].script_public_key).expect("token state");

        self.minter_outpoint = TransactionOutpoint::new(mint_tx.id(), 0);
        self.minter_state = minter_state_from_spk(&mint_tx.outputs[0].script_public_key).expect("minter state");
        self.minter_entry =
            UtxoEntry::new(mint_tx.outputs[0].value, mint_tx.outputs[0].script_public_key.clone(), 0, false, Some(self.covenant_id));

        TokenUtxo { state: token_state, outpoint: token_outpoint, entry: token_entry }
    }

    fn build_split_merge_tx(&self, tokens: &[TokenUtxo], outputs: &[NativeAssetOutput], auth: &SpendableUtxo) -> Transaction {
        let token_refs: Vec<TokenInputRef<'_>> =
            tokens.iter().map(|token| TokenInputRef { state: &token.state, outpoint: &token.outpoint, entry: &token.entry }).collect();

        let auth_input = TransactionInput::new(auth.outpoint, vec![], 0, 1);
        build_token_split_merge_tx(&token_refs, &self.covenant_id, outputs, auth_input, auth.entry.clone(), &self.mass_calculator)
            .expect("split/merge tx")
    }

    fn run_token_inputs(&self, tx: &Transaction, auth: &SpendableUtxo, tokens: &[TokenUtxo]) -> Result<(), TxScriptError> {
        let mut entries = Vec::with_capacity(tokens.len() + 1);
        entries.push(auth.entry.clone());
        for token in tokens {
            entries.push(token.entry.clone());
        }

        for idx in 0..tokens.len() {
            run_covenant_vm(tx, idx + 1, entries.clone(), &self.reused_values, &self.sig_cache, self.flags)?;
        }
        Ok(())
    }
}

fn outputs_for_amounts(amounts: &[u64], owner_spk_bytes: &[u8]) -> Vec<NativeAssetOutput> {
    amounts.iter().map(|&amount| NativeAssetOutput { amount, recipient_spk_bytes: owner_spk_bytes.to_vec() }).collect()
}

fn token_utxo_from_tx(covenant_id: Hash, tx: &Transaction, output_index: u32) -> TokenUtxo {
    let output = &tx.outputs[output_index as usize];
    let state = token_state_from_spk(&output.script_public_key).expect("token state");
    let entry = UtxoEntry::new(output.value, output.script_public_key.clone(), 0, false, Some(covenant_id));
    let outpoint = TransactionOutpoint::new(tx.id(), output_index);
    TokenUtxo { state, outpoint, entry }
}

#[test]
fn split_rejects_missing_covenant_info() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();
    let tokens = vec![h.mint_to(5, &authority_spk)];
    let auth = h.get_auth_utxo();
    let outputs = outputs_for_amounts(&[tokens[0].state.amount], &authority_spk_bytes);
    let mut tx = h.build_split_merge_tx(&tokens, &outputs, &auth);

    for output in &mut tx.outputs {
        output.covenant = None;
    }
    tx.finalize();

    assert!(h.run_token_inputs(&tx, &auth, &tokens).is_err());
}

#[test]
fn split_rejects_wrong_authorizing_input() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();
    let tokens = vec![h.mint_to(5, &authority_spk)];
    let auth = h.get_auth_utxo();
    let outputs = outputs_for_amounts(&[tokens[0].state.amount], &authority_spk_bytes);
    let mut tx = h.build_split_merge_tx(&tokens, &outputs, &auth);

    for output in &mut tx.outputs {
        if let Some(binding) = &mut output.covenant {
            binding.authorizing_input = 0;
        }
    }
    tx.finalize();

    assert!(h.run_token_inputs(&tx, &auth, &tokens).is_err());
}

#[test]
fn split_rejects_extra_output() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();
    let tokens = vec![h.mint_to(5, &authority_spk)];
    let auth = h.get_auth_utxo();
    let outputs = outputs_for_amounts(&[tokens[0].state.amount], &authority_spk_bytes);
    let mut tx = h.build_split_merge_tx(&tokens, &outputs, &auth);

    tx.outputs.push(TransactionOutput::new(1, authority_spk));
    tx.finalize();

    assert!(h.run_token_inputs(&tx, &auth, &tokens).is_err());
}

#[test]
fn split_rejects_output_script_mismatch() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();
    let tokens = vec![h.mint_to(5, &authority_spk)];
    let auth = h.get_auth_utxo();
    let outputs = outputs_for_amounts(&[tokens[0].state.amount], &authority_spk_bytes);
    let mut tx = h.build_split_merge_tx(&tokens, &outputs, &auth);

    // hack the first output script
    let mut script = tx.outputs[0].script_public_key.script().to_vec();
    let flip_index = script.len() - 1;
    script[flip_index] ^= 0x01;
    let version = tx.outputs[0].script_public_key.version();
    tx.outputs[0].script_public_key = ScriptPublicKey::from_vec(version, script);
    tx.finalize();

    assert!(h.run_token_inputs(&tx, &auth, &tokens).is_err());
}

#[test]
fn split_rejects_output_amount_mismatch() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();
    let tokens = vec![h.mint_to(5, &authority_spk)];
    let auth = h.get_auth_utxo();
    let outputs = outputs_for_amounts(&[tokens[0].state.amount], &authority_spk_bytes);
    let mut tx = h.build_split_merge_tx(&tokens, &outputs, &auth);

    let mut script = tx.outputs[0].script_public_key.script().to_vec();
    let start = SPK_AMOUNT_OFFSET;
    let end = SPK_AMOUNT_OFFSET + AMOUNT_LEN;
    let mut amount_bytes = [0u8; AMOUNT_LEN];
    amount_bytes.copy_from_slice(&script[start..end]);
    let mut amount = u64::from_le_bytes(amount_bytes);
    amount += 1;
    script[start..end].copy_from_slice(&amount.to_le_bytes());
    let version = tx.outputs[0].script_public_key.version();
    tx.outputs[0].script_public_key = ScriptPublicKey::from_vec(version, script);
    tx.finalize();

    assert!(h.run_token_inputs(&tx, &auth, &tokens).is_err());
}

#[test]
fn merge_rejects_mismatched_owner() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();
    let token_a = h.mint_to(4, &authority_spk);
    let other_spk = h.random_spk();
    let token_b = h.mint_to(4, &other_spk);

    let tokens = vec![token_a, token_b];
    let auth = h.get_auth_utxo();
    let total_amount = tokens.iter().map(|token| token.state.amount).sum::<u64>();
    let outputs = outputs_for_amounts(&[total_amount], &authority_spk_bytes);
    let tx = h.build_split_merge_tx(&tokens, &outputs, &auth);

    assert!(h.run_token_inputs(&tx, &auth, &tokens).is_err());
}

#[test]
fn full_flow_passes() {
    let mut h = Harness::new();
    let authority_spk = h.authority_spk.clone();
    let authority_spk_bytes = h.authority_spk_bytes.clone();

    let _mint1 = h.mint_to(2, &authority_spk);
    let _mint2 = h.mint_to(2, &authority_spk);
    let mint3 = h.mint_to(2, &authority_spk);

    let split_auth = h.get_auth_utxo();
    let split_outputs = outputs_for_amounts(&[1, 1], &authority_spk_bytes);
    let split_tx = h.build_split_merge_tx(&[mint3.clone()], &split_outputs, &split_auth);
    h.run_token_inputs(&split_tx, &split_auth, &[mint3]).expect("split");

    let split_token0 = token_utxo_from_tx(h.covenant_id, &split_tx, 0);
    let split_token1 = token_utxo_from_tx(h.covenant_id, &split_tx, 1);

    let merge_auth = h.get_auth_utxo();
    let merge_amount = split_token0.state.amount + split_token1.state.amount;
    let merge_outputs = outputs_for_amounts(&[merge_amount], &authority_spk_bytes);
    let merge_tx = h.build_split_merge_tx(&[split_token0.clone(), split_token1.clone()], &merge_outputs, &merge_auth);
    h.run_token_inputs(&merge_tx, &merge_auth, &[split_token0, split_token1]).expect("merge");
}
