use kaspa_consensus_core::hashing::tx::transaction_id_preimage;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_txscript::opcodes::codes::{
    Op1Add, OpBlake2bWithKey, OpCat, OpDup, OpEqual, OpEqualVerify, OpOutpointTxId, OpRot, OpTxInputIndex, OpTxInputSpk,
    OpTxOutputCount, OpTxOutputSpk, OpTxPayloadLen, OpTxPayloadSubstr,
};
use kaspa_txscript::script_builder::{ScriptBuilder, ScriptBuilderResult};
use kaspa_wrpc_client::prelude::NetworkType;

/// Holds the current covenant UTXO state.
#[derive(Clone)]
pub struct CovenantState {
    prev_tx_rest: Vec<u8>,
    prev_payload: Vec<u8>,
    utxo_outpoint: TransactionOutpoint,
    utxo_entry: UtxoEntry,
    pub counter: u8,
}

impl CovenantState {
    pub fn from_tx_with_entry(tx: Transaction, utxo_entry: UtxoEntry, counter: u8) -> Self {
        let preimage = transaction_id_preimage(&tx);
        let payload_len = tx.payload.len();
        let (rest, payload) = preimage.split_at(preimage.len() - payload_len);
        let outpoint = TransactionOutpoint::new(tx.id(), 0);
        Self { prev_tx_rest: rest.to_vec(), prev_payload: payload.to_vec(), utxo_outpoint: outpoint, utxo_entry, counter }
    }
}

/// Build the covenant script described in the docs.
pub fn build_covenant_script() -> ScriptBuilderResult<Vec<u8>> {
    Ok(ScriptBuilder::new()
			// Hash(prev_tx_rest || prev_tx_payload) with domain "TransactionID" and verify matches input outpoint txid
			.add_op(OpDup)?
			.add_op(OpRot)?
			.add_op(OpRot)?
			.add_op(OpCat)?
			.add_data(b"TransactionID")?
			.add_op(OpBlake2bWithKey)?
			.add_op(OpTxInputIndex)?
			.add_op(OpOutpointTxId)?
			.add_op(OpEqualVerify)?
			// Enforce payload increment: payload_of_tx == prev_payload + 1
			.add_op(Op1Add)?
			.add_i64(0)?
			.add_op(OpTxPayloadLen)?
			.add_op(OpTxPayloadSubstr)?
			.add_op(OpEqualVerify)?
			// Enforce same script pub key and single-output spend
			.add_op(OpTxInputIndex)?
			.add_op(OpTxInputSpk)?
			.add_i64(0)?
			.add_op(OpTxOutputSpk)?
			.add_op(OpEqualVerify)?
			.add_op(OpTxOutputCount)?
			.add_i64(1)?
			.add_op(OpEqual)?
			.drain())
}

/// Build the spend transaction for the next counter value.
pub fn build_spend_tx(
    state: &CovenantState,
    next_counter: u8,
    spk: &kaspa_consensus_core::tx::ScriptPublicKey,
    covenant_script: &[u8],
) -> Transaction {
    let payload = encode_counter(next_counter);
    let sig_script = ScriptBuilder::new()
		.add_data(&state.prev_tx_rest)
		.unwrap()
		.add_data(&state.prev_payload)
		.unwrap()
		// For P2SH the redeem script must be the last stack item in the signature script
		.add_data(covenant_script)
		.unwrap()
		.drain();

    let input = TransactionInput::new(state.utxo_outpoint, sig_script, 0, 0);

    // temp tx without fees
    let temp_output = TransactionOutput::new(state.utxo_entry.amount, spk.clone());
    let temp_tx = Transaction::new(0, vec![input.clone()], vec![temp_output], 0, SubnetworkId::default(), 0, payload.clone());
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![state.utxo_entry.clone()]);

    // calculate mass/fees
    let calculator = MassCalculator::new_with_consensus_params(&NetworkType::Devnet.into());
    let storage_mass = calculator.calc_contextual_masses(&temp_tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(temp_tx.tx);
    let mass = storage_mass.max(compute_mass).max(transient_mass);

    // build real transaction
    let amount_minus_fees = state.utxo_entry.amount.saturating_sub(mass);
    let output = TransactionOutput::new(amount_minus_fees, spk.clone());

    let mut tx = Transaction::new(0, vec![input], vec![output], 0, SubnetworkId::default(), 0, payload);
    tx.finalize();
    tx
}

pub fn encode_counter(counter: u8) -> Vec<u8> {
    if counter == 0 {
        vec![]
    } else {
        vec![counter]
    }
}
