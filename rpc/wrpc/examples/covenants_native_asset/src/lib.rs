mod covenants;
mod errors;
mod result;
mod script_layout;
mod scriptnum;
#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::covenants::TokenState;
#[cfg(test)]
use kaspa_consensus_core::hashing::sighash::SigHashReusedValues;
#[cfg(test)]
use kaspa_consensus_core::tx::{
    PopulatedTransaction, ScriptPublicKey, Transaction, TransactionOutpoint, UtxoEntry, VerifiableTransaction,
};
#[cfg(test)]
use kaspa_txscript::caches::Cache;
#[cfg(test)]
use kaspa_txscript::covenants::CovenantsContext;
#[cfg(test)]
use kaspa_txscript::{EngineCtx, EngineFlags, TxScriptEngine};
#[cfg(test)]
use kaspa_txscript_errors::TxScriptError;
#[cfg(test)]
use rand::{RngCore, rngs::StdRng};

#[cfg(test)]
#[derive(Clone)]
struct SpendableUtxo {
    outpoint: TransactionOutpoint,
    entry: UtxoEntry,
}

#[cfg(test)]
#[derive(Clone)]
struct TokenUtxo {
    state: TokenState,
    outpoint: TransactionOutpoint,
    entry: UtxoEntry,
}

#[cfg(test)]
fn random_spk(rng: &mut StdRng) -> ScriptPublicKey {
    let mut script = [0u8; 34];
    rng.fill_bytes(&mut script);
    ScriptPublicKey::from_vec(0, script.to_vec())
}

#[cfg(test)]
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
    let engine_ctx = EngineCtx::new(sig_cache).with_reused(reused_values).with_covenants_ctx(&covenants_ctx);
    let mut vm = TxScriptEngine::from_transaction_input(&populated, &tx.inputs[input_index], input_index, entry, engine_ctx, flags);
    vm.execute()
}

#[cfg(test)]
fn build_covenants_ctx(tx: &Transaction, entries: &[UtxoEntry]) -> CovenantsContext {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let ctx = CovenantsContext::from_tx(&populated).unwrap();

    ctx
}
