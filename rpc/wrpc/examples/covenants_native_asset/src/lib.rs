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
use kaspa_txscript::{EngineCtx, EngineFlags, TxScriptEngine};
#[cfg(test)]
use kaspa_txscript_errors::TxScriptError;
#[cfg(test)]
use rand::{rngs::StdRng, RngCore};

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
fn build_covenants_ctx(tx: &Transaction, entries: &[UtxoEntry]) -> kaspa_txscript::covenants::CovenantsContext {
    use kaspa_txscript::covenants::{CovenantInputContext, CovenantSharedContext, CovenantsContext};
    use std::collections::hash_map::Entry;

    let mut ctx = CovenantsContext::default();

    for (i, entry) in entries.iter().enumerate() {
        if let Some(covenant_id) = entry.covenant_id {
            match ctx.shared_ctxs.entry(covenant_id) {
                Entry::Occupied(mut e) => e.get_mut().input_indices.push(i),
                Entry::Vacant(e) => {
                    e.insert(CovenantSharedContext { input_indices: vec![i], output_indices: Default::default() });
                }
            }
        }
    }

    for (i, output) in tx.outputs.iter().enumerate() {
        if let Some(covenant_binding) = &output.covenant {
            let auth_input = covenant_binding.authorizing_input as usize;
            let utxo_entry = entries.get(auth_input).expect("missing auth input entry");
            if let Some(covenant_id) = utxo_entry.covenant_id {
                assert_eq!(covenant_id, covenant_binding.covenant_id);
            }

            match ctx.input_ctxs.entry(auth_input) {
                Entry::Occupied(mut e) => e.get_mut().auth_outputs.push(i),
                Entry::Vacant(e) => {
                    e.insert(CovenantInputContext { covenant_id: covenant_binding.covenant_id, auth_outputs: vec![i] });
                }
            }

            match ctx.shared_ctxs.entry(covenant_binding.covenant_id) {
                Entry::Occupied(mut e) => e.get_mut().output_indices.push(i),
                Entry::Vacant(e) => {
                    e.insert(CovenantSharedContext { input_indices: Default::default(), output_indices: vec![i] });
                }
            }
        }
    }

    ctx
}
