use kaspa_consensus_core::constants::TX_VERSION_POST_COV_HF;
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    CovenantBinding, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput,
    UtxoEntry,
};
use kaspa_hashes::Hash;
use kaspa_txscript::opcodes::codes::{
    Op2Drop, OpAdd, OpBin2Num, OpCovOutputCount, OpCovOutputIdx, OpDrop, OpDup, OpEndIf, OpEqual, OpEqualVerify, OpGreaterThanOrEqual,
    OpIf, OpNumEqualVerify, OpPick, OpSize, OpSub, OpSubStr, OpSwap, OpTrue, OpTxInputCount, OpTxInputIndex, OpTxInputSpk,
    OpTxInputSpkLen, OpTxInputSpkSubstr, OpTxOutputCount, OpTxOutputSpkLen, OpTxOutputSpkSubstr, OpVerify, OpWithin,
};
use kaspa_txscript::script_builder::{ScriptBuilder, ScriptBuilderError};
use kaspa_txscript::SpkEncoding;

use crate::errors::CovenantError;
use crate::result::CovenantResult;
use crate::script_layout::{
    AMOUNT_LEN, COVENANT_ID_LEN, SPK_AMOUNT_OFFSET, SPK_BYTES_MAX, SPK_BYTES_MIN, SPK_COVENANT_ID_OFFSET, SPK_FIELD_LEN,
    SPK_FIELD_LEN_OFFSET, SPK_LOGIC_TAIL_OFFSET, SPK_SPK_FIELD_OFFSET,
};
use crate::scriptnum::{append_u64_le, decode_u64_le};

const MAX_SPLIT_MERGE_INPUTS_COUNT: usize = 3;
const MAX_SPLIT_MERGE_OUTPUTS_COUNT: usize = 3;
const TOKEN_AUTH_INPUT_INDEX: i64 = 0;
const TOKEN_LEADER_INPUT_INDEX: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
/// This is to be kept externally (but verified internally)
/// and is used to build a mint script
pub struct MinterState {
    /// genesis outpoint - this is the asset ID
    pub covenant_id: Hash,
    pub remaining_supply: u64,
    /// authorized public key (spk) to mint tokens
    /// note: it must contains version bytes (2), hint: use .to_bytes()
    pub authority_spk_bytes: Vec<u8>,
}

impl MinterState {
    pub fn mint_next(&self, amount: u64) -> CovenantResult<Self> {
        // following checks only lives at transaction build time (avoid usage mistakes ahead of time)
        // they don't enforce any rules, see covenant script for state transition validation instead
        if amount == 0 {
            return Err(CovenantError::InvalidField("amount"));
        }
        let remaining = self
            .remaining_supply
            .checked_sub(amount)
            .ok_or(CovenantError::AmountExceedsRemainingSupply { remaining: self.remaining_supply, amount })?;

        Ok(Self { covenant_id: self.covenant_id, remaining_supply: remaining, authority_spk_bytes: self.authority_spk_bytes.clone() })
    }
}

/// This is to be kept externally (but verified internally)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenState {
    /// genesis minter outpoint (inherited from the initial mint)
    pub covenant_id: Hash,
    /// current token amount
    pub amount: u64,
    /// authorized pubic key (spk) to transfer tokens (holder spk)
    /// note: it must contains version bytes (2), hint: use .to_bytes()
    pub owner_spk_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAssetOutput {
    pub amount: u64,
    pub recipient_spk_bytes: Vec<u8>,
}

// nothing, just naming clarification
pub fn covenant_id_for_outpoint(outpoint: &TransactionOutpoint) -> Hash {
    covenant_id(*outpoint)
}

// encode spk using internals (this adds spk version as a prefix of the returned bytes)
pub fn try_spk_bytes(spk: &ScriptPublicKey) -> CovenantResult<Vec<u8>> {
    let spk_bytes = spk.to_bytes();
    validate_spk_bytes(&spk_bytes, "spk_bytes")?;
    Ok(spk_bytes)
}

pub fn minter_state_from_spk(spk: &ScriptPublicKey) -> CovenantResult<MinterState> {
    let spk_bytes = spk.to_bytes();
    let covenant_id = decode_covenant_id(&spk_bytes)?;
    let remaining_supply = decode_amount(&spk_bytes, "remaining_supply")?;
    let authority_spk_bytes = decode_spk_field(&spk_bytes, SPK_SPK_FIELD_OFFSET, "authority_spk_bytes")?;
    Ok(MinterState { covenant_id, remaining_supply, authority_spk_bytes })
}

pub fn token_state_from_spk(spk: &ScriptPublicKey) -> CovenantResult<TokenState> {
    let spk_bytes = spk.to_bytes();
    let covenant_id = decode_covenant_id(&spk_bytes)?;
    let amount = decode_amount(&spk_bytes, "amount")?;
    let owner_spk_bytes = decode_spk_field(&spk_bytes, SPK_SPK_FIELD_OFFSET, "owner_spk_bytes")?;
    Ok(TokenState { covenant_id, amount, owner_spk_bytes })
}

pub fn build_minter_covenant_script_knat20(state: &MinterState) -> CovenantResult<Vec<u8>> {
    validate_spk_bytes(&state.authority_spk_bytes, "authority_spk_bytes")?;

    // we could impl shared trait on both minter&token state instead, but they are the same as of now
    let encoded_state = encode_script_state(state.covenant_id, state.remaining_supply, &state.authority_spk_bytes)?;
    let token_logic = build_token_covenant_logic()?;
    let minter_logic = build_minter_covenant_logic(&token_logic)?;

    let mut script = encoded_state;
    script.extend_from_slice(&minter_logic);
    Ok(script)
}

pub fn build_token_covenant_script_knat20(state: &TokenState) -> CovenantResult<Vec<u8>> {
    validate_spk_bytes(&state.owner_spk_bytes, "owner_spk_bytes")?;

    let encoded_state = encode_script_state(state.covenant_id, state.amount, &state.owner_spk_bytes)?;
    let token_logic = build_token_covenant_logic()?;

    let mut script = encoded_state;
    script.extend_from_slice(&token_logic);
    Ok(script)
}

pub fn build_mint_tx(
    state: &MinterState,
    utxo_outpoint: &TransactionOutpoint,
    utxo_entry: &UtxoEntry,
    mint_amount: u64,
    recipient_spk: &ScriptPublicKey,
    kaspa_amount: u64,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    mass_calculator: &MassCalculator,
) -> CovenantResult<Transaction> {
    let recipient_spk_bytes = try_spk_bytes(recipient_spk)?;
    let next_state = state.mint_next(mint_amount)?;

    let minter_script = build_minter_covenant_script_knat20(&next_state)?;
    let token_state = TokenState { covenant_id: state.covenant_id, amount: mint_amount, owner_spk_bytes: recipient_spk_bytes };
    let token_script = build_token_covenant_script_knat20(&token_state)?;

    let minter_spk = ScriptPublicKey::from_vec(0, minter_script);
    let token_spk = ScriptPublicKey::from_vec(0, token_script);

    let minter_input = TransactionInput::new(*utxo_outpoint, vec![], 0, 0);

    let mut temp_outputs =
        vec![TransactionOutput::new(utxo_entry.amount, minter_spk.clone()), TransactionOutput::new(kaspa_amount, token_spk.clone())];
    apply_covenant_info(&mut temp_outputs, state.covenant_id, 0);

    let temp_tx = Transaction::new(
        TX_VERSION_POST_COV_HF,
        vec![minter_input.clone(), auth_input.clone()],
        temp_outputs,
        0,
        SubnetworkId::default(),
        0,
        vec![],
    );
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![utxo_entry.clone(), auth_entry.clone()]);

    let mass = estimate_mass(mass_calculator, &temp_tx);
    let required = kaspa_amount.checked_add(mass).ok_or(CovenantError::InvalidField("fee"))?;
    let minter_value = checked_sub_or_err(utxo_entry.amount, required)?;

    let mut outputs = vec![TransactionOutput::new(minter_value, minter_spk), TransactionOutput::new(kaspa_amount, token_spk)];
    apply_covenant_info(&mut outputs, state.covenant_id, 0);

    let mut tx =
        Transaction::new(TX_VERSION_POST_COV_HF, vec![minter_input, auth_input], outputs, 0, SubnetworkId::default(), 0, vec![]);
    tx.finalize();
    Ok(tx)
}

/// Sugar syntaxe function to operate a transfer 1:1 of one UTXO containing a token
pub fn build_token_transfer_tx(
    state: &TokenState,
    utxo_outpoint: &TransactionOutpoint,
    utxo_entry: &UtxoEntry,
    new_owner_spk: &ScriptPublicKey,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    mass_calculator: &MassCalculator,
) -> CovenantResult<Transaction> {
    let output = NativeAssetOutput { amount: state.amount, recipient_spk_bytes: try_spk_bytes(new_owner_spk)? };
    let token_input = TokenInputRef { state, outpoint: utxo_outpoint, entry: utxo_entry };
    build_token_split_merge_tx(&[token_input], &[output], auth_input, auth_entry, mass_calculator)
}

pub struct TokenInputRef<'a> {
    pub state: &'a TokenState,
    pub outpoint: &'a TransactionOutpoint,
    pub entry: &'a UtxoEntry,
}

pub fn build_token_split_merge_tx(
    token_inputs: &[TokenInputRef<'_>],
    outputs: &[NativeAssetOutput],
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    mass_calculator: &MassCalculator,
) -> CovenantResult<Transaction> {
    if token_inputs.is_empty() {
        return Err(CovenantError::InvalidField("input_count"));
    }
    if outputs.is_empty() {
        return Err(CovenantError::InvalidField("output_count"));
    }
    if token_inputs.len() > MAX_SPLIT_MERGE_INPUTS_COUNT {
        return Err(CovenantError::InvalidField("input_count"));
    }
    if outputs.len() > MAX_SPLIT_MERGE_OUTPUTS_COUNT {
        return Err(CovenantError::InvalidField("output_count"));
    }
    for output in outputs {
        if output.amount == 0 {
            return Err(CovenantError::InvalidField("output_amount"));
        }
    }

    let covenant_id = token_inputs[0].state.covenant_id;
    for token in token_inputs.iter().skip(1) {
        if token.state.covenant_id != covenant_id {
            return Err(CovenantError::InvalidField("covenant_id"));
        }
    }

    let mut inputs = Vec::with_capacity(token_inputs.len() + 1);
    let mut entries = Vec::with_capacity(token_inputs.len() + 1);

    inputs.push(auth_input.clone());
    entries.push(auth_entry.clone());

    for token in token_inputs {
        let token_script = build_token_covenant_script_knat20(token.state)?;
        let token_spk = ScriptPublicKey::from_vec(0, token_script);
        if token.entry.script_public_key != token_spk {
            return Err(CovenantError::InvalidField("token_spk"));
        }
        inputs.push(TransactionInput::new(*token.outpoint, vec![], 0, 0));
        entries.push(token.entry.clone());
    }

    let total_input_value: u64 = token_inputs.iter().map(|token| token.entry.amount).sum();
    let temp_values = even_split_values(total_input_value, outputs.len())?;

    let mut temp_outputs = Vec::with_capacity(outputs.len());
    for output in outputs {
        validate_spk_bytes(&output.recipient_spk_bytes, "recipient_spk_bytes")?;
        let token_state = TokenState { covenant_id, amount: output.amount, owner_spk_bytes: output.recipient_spk_bytes.clone() };
        let token_script = build_token_covenant_script_knat20(&token_state)?;
        let token_spk = ScriptPublicKey::from_vec(0, token_script);
        // 1 Kaspa allocated to the output
        temp_outputs.push(TransactionOutput::new(1, token_spk));
    }

    for (output, value) in temp_outputs.iter_mut().zip(temp_values) {
        output.value = value;
    }
    apply_covenant_info(&mut temp_outputs, covenant_id, TOKEN_LEADER_INPUT_INDEX as u16);

    let temp_tx =
        Transaction::new(TX_VERSION_POST_COV_HF, inputs.clone(), temp_outputs.clone(), 0, SubnetworkId::default(), 0, vec![]);
    let temp_tx = PopulatedTransaction::new(&temp_tx, entries.clone());
    let mass = estimate_mass(mass_calculator, &temp_tx);

    let available_value = total_input_value.saturating_sub(mass);
    let output_values = allocate_output_values_by_amount(available_value, outputs)?;

    let mut final_outputs = Vec::with_capacity(outputs.len());
    for (output, value) in outputs.iter().zip(output_values) {
        let token_state = TokenState { covenant_id, amount: output.amount, owner_spk_bytes: output.recipient_spk_bytes.clone() };
        let token_script = build_token_covenant_script_knat20(&token_state)?;
        let token_spk = ScriptPublicKey::from_vec(0, token_script);
        final_outputs.push(TransactionOutput::new(value, token_spk));
    }
    apply_covenant_info(&mut final_outputs, covenant_id, TOKEN_LEADER_INPUT_INDEX as u16);

    let mut tx = Transaction::new(TX_VERSION_POST_COV_HF, inputs, final_outputs, 0, SubnetworkId::default(), 0, vec![]);
    tx.finalize();
    Ok(tx)
}

/// takes the state and return the encoded form
fn encode_script_state(covenant_id: Hash, amount: u64, spk_bytes: &[u8]) -> CovenantResult<Vec<u8>> {
    let spk_field = encode_spk_field(spk_bytes)?;
    let mut amount_bytes = Vec::with_capacity(AMOUNT_LEN);
    append_u64_le(&mut amount_bytes, amount, "amount")?;

    let mut sb = ScriptBuilder::new();
    sb.add_data(&covenant_id.as_bytes())?.add_data(&amount_bytes)?.add_data(&spk_field)?;
    Ok(sb.drain())
}

/// param: token logic to enforce on second output
/// in this example this is enforced to be a naive token implementation
/// expect at least two utxo inputs: [minter_script, authorithy_signed_input]
fn build_minter_covenant_logic(token_logic: &[u8]) -> CovenantResult<Vec<u8>> {
    let mut sb = ScriptBuilder::new();

    // start with a clean stack
    sb.add_op(Op2Drop)?.add_op(OpDrop)?;

    // input 0 must be the minter input (ourself)
    sb.add_op(OpTxInputIndex)?.add_i64(0)?.add_op(OpEqualVerify)?;

    // must have at least minter + (expected, need validation later) authority inputs
    // protocol fails if lower than 2
    sb.add_op(OpTxInputCount)?.add_i64(2)?.add_op(OpGreaterThanOrEqual)?.add_op(OpVerify)?;

    // authority input spk must match the authority spk field.
    sb.add_i64(1)?.add_op(OpTxInputSpk)?;
    push_current_spk_field(&mut sb)?;
    sb.add_op(OpSwap)?;
    verify_spk_field_matches_stack(&mut sb)?;

    // require exactly two covenant outputs for input0
    // possible enhancement: allow extra output (give back to the authorithy) in a 2:2+ scheme
    // all that matters is enforcing at least 2 outputs
    sb.add_i64(0)?.add_op(OpCovOutputCount)?.add_i64(2)?.add_op(OpEqualVerify)?;

    // output0 must be the minter covenant and reference input0 as its authorithy
    verify_authorized_output_at_index_script_matches_input0_script(&mut sb, 0)?;

    // output0 authority field must match current authority field (authorithy doesn't change)
    push_current_spk_field(&mut sb)?;
    push_auth_output_spk_field(&mut sb, 0)?;
    sb.add_op(OpEqualVerify)?;

    // output1 must be a token covenant and reference input0 as its authorithy
    verify_authorized_output_at_index_script_matches_script(&mut sb, 1, token_logic)?;

    // output1 owner spk length must be within bounds
    verify_auth_output_spk_field_len(&mut sb, 1)?;

    // minted amount from token output.
    push_auth_output_amount(&mut sb, 1)?;
    assert_positive_amount(&mut sb)?;

    // ensure remaining supply >= minted amount.
    sb.add_op(OpDup)?;
    push_current_amount(&mut sb)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // remaining_supply_next = remaining_supply - minted_amount.
    push_current_amount(&mut sb)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpSub)?;

    // output0 remaining_supply must equal remaining_supply_next.
    push_auth_output_amount(&mut sb, 0)?;
    sb.add_op(OpNumEqualVerify)?;

    sb.add_op(OpTrue)?;
    Ok(sb.drain())
}

// TODO(enhancement): verify output[n] spk indeed is the output[n].recipient
fn build_token_covenant_logic() -> CovenantResult<Vec<u8>> {
    let mut sb = ScriptBuilder::new();

    // start with a clean stack (drop the state)
    sb.add_op(Op2Drop)?.add_op(OpDrop)?;

    // input count must be 2..=(MAX_SPLIT_MERGE_INPUTS_COUNT + 1) (auth + token inputs)
    sb.add_op(OpTxInputCount)?.add_i64(2)?.add_i64((MAX_SPLIT_MERGE_INPUTS_COUNT + 2) as i64)?.add_op(OpWithin)?.add_op(OpVerify)?;

    // ensure current input index is a token input (auth input is index 0)
    sb.add_op(OpTxInputCount)?;
    sb.add_op(OpTxInputIndex)?;
    sb.add_op(OpSwap)?;
    sb.add_i64(1)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpWithin)?;
    sb.add_op(OpVerify)?;

    // leader input (index 1) must match the current token logic tail
    verify_input_script_at_index_matches_current_script(&mut sb, TOKEN_LEADER_INPUT_INDEX)?;

    // all outputs must reference input1 as authority
    sb.add_op(OpTxOutputCount)?;
    sb.add_i64(TOKEN_LEADER_INPUT_INDEX)?.add_op(OpCovOutputCount)?;
    sb.add_op(OpNumEqualVerify)?;

    // only input1 does actual checks, others return true from here
    sb.add_op(OpTxInputIndex)?;
    sb.add_i64(TOKEN_LEADER_INPUT_INDEX)?;
    sb.add_op(OpEqual)?;
    sb.add_op(OpIf)?;

    // ensure all token inputs share the same owner spk field as the leader
    push_input_spk_field(&mut sb, TOKEN_LEADER_INPUT_INDEX)?;
    for idx in 2..=MAX_SPLIT_MERGE_INPUTS_COUNT {
        sb.add_op(OpTxInputCount)?.add_i64((idx + 1) as i64)?.add_op(OpGreaterThanOrEqual)?;
        sb.add_op(OpIf)?;
        sb.add_op(OpDup)?;
        push_input_spk_field(&mut sb, idx as i64)?;
        sb.add_op(OpEqualVerify)?;
        sb.add_op(OpEndIf)?;
    }

    // authorization input spk must match the owner spk field (leader input)
    sb.add_i64(TOKEN_AUTH_INPUT_INDEX)?.add_op(OpTxInputSpk)?;
    verify_spk_field_matches_stack(&mut sb)?;

    // extra token inputs must match script and covenant_id
    for idx in 2..=MAX_SPLIT_MERGE_INPUTS_COUNT {
        sb.add_op(OpTxInputCount)?.add_i64((idx + 1) as i64)?.add_op(OpGreaterThanOrEqual)?;
        sb.add_op(OpIf)?;
        verify_input_script_at_index_matches_current_script(&mut sb, idx as i64)?;
        push_input_state_covenant_id(&mut sb, TOKEN_LEADER_INPUT_INDEX)?;
        push_input_state_covenant_id(&mut sb, idx as i64)?;
        sb.add_op(OpEqualVerify)?;
        sb.add_op(OpEndIf)?;
    }

    // total input amount validation. sum(inputs[n].amount) == sum(outputs[n].amount)
    push_input_amount(&mut sb, TOKEN_LEADER_INPUT_INDEX)?;
    assert_positive_amount(&mut sb)?;
    for idx in 2..=MAX_SPLIT_MERGE_INPUTS_COUNT {
        sb.add_op(OpTxInputCount)?.add_i64((idx + 1) as i64)?.add_op(OpGreaterThanOrEqual)?;
        sb.add_op(OpIf)?;
        push_input_amount(&mut sb, idx as i64)?;
        assert_positive_amount(&mut sb)?;
        sb.add_op(OpAdd)?;
        sb.add_op(OpEndIf)?;
    }

    // outputs count for input1 must be 1..=MAX_SPLIT_MERGE_OUTPUTS_COUNT.
    sb.add_i64(TOKEN_LEADER_INPUT_INDEX)?.add_op(OpCovOutputCount)?;
    sb.add_i64(1)?.add_i64((MAX_SPLIT_MERGE_OUTPUTS_COUNT + 1) as i64)?.add_op(OpWithin)?.add_op(OpVerify)?;

    // total output amount
    verify_authorized_output_at_index_script_matches_authority_script(&mut sb, TOKEN_LEADER_INPUT_INDEX, 0)?;
    verify_authorized_output_spk_field_len(&mut sb, TOKEN_LEADER_INPUT_INDEX, 0)?;
    push_authorized_output_amount(&mut sb, TOKEN_LEADER_INPUT_INDEX, 0)?;
    assert_positive_amount(&mut sb)?;
    for idx in 1..MAX_SPLIT_MERGE_OUTPUTS_COUNT {
        sb.add_i64(TOKEN_LEADER_INPUT_INDEX)?.add_op(OpCovOutputCount)?.add_i64((idx + 1) as i64)?.add_op(OpGreaterThanOrEqual)?;
        sb.add_op(OpIf)?;
        verify_authorized_output_at_index_script_matches_authority_script(&mut sb, TOKEN_LEADER_INPUT_INDEX, idx as i64)?;
        verify_authorized_output_spk_field_len(&mut sb, TOKEN_LEADER_INPUT_INDEX, idx as i64)?;
        push_authorized_output_amount(&mut sb, TOKEN_LEADER_INPUT_INDEX, idx as i64)?;
        assert_positive_amount(&mut sb)?;
        sb.add_op(OpAdd)?;
        sb.add_op(OpEndIf)?;
    }

    // compare total inputs and outputs.
    sb.add_op(OpNumEqualVerify)?;

    sb.add_op(OpEndIf)?;

    sb.add_op(OpTrue)?;
    Ok(sb.drain())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [amount]
fn push_current_amount(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpTxInputIndex)?
        .add_i64(SPK_AMOUNT_OFFSET as i64)?
        .add_i64((SPK_AMOUNT_OFFSET + AMOUNT_LEN) as i64)?
        .add_op(OpTxInputSpkSubstr)?
        .add_op(OpBin2Num)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [amount]
fn push_input_amount(sb: &mut ScriptBuilder, idx: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(idx)?
        .add_i64(SPK_AMOUNT_OFFSET as i64)?
        .add_i64((SPK_AMOUNT_OFFSET + AMOUNT_LEN) as i64)?
        .add_op(OpTxInputSpkSubstr)?
        .add_op(OpBin2Num)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// post-stack: [spk_field]
/// push the current spk field on the stack, reminder token = holder; minter = mint authorithy
fn push_current_spk_field(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpTxInputIndex)?
        .add_i64(SPK_SPK_FIELD_OFFSET as i64)?
        .add_i64((SPK_SPK_FIELD_OFFSET + SPK_FIELD_LEN) as i64)?
        .add_op(OpTxInputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [covenant_id]
fn push_input_state_covenant_id(sb: &mut ScriptBuilder, idx: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(idx)?
        .add_i64(SPK_COVENANT_ID_OFFSET as i64)?
        .add_i64((SPK_COVENANT_ID_OFFSET + COVENANT_ID_LEN) as i64)?
        .add_op(OpTxInputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// post-stack: [spk_field]
fn push_input_spk_field(sb: &mut ScriptBuilder, idx: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(idx)?
        .add_i64(SPK_SPK_FIELD_OFFSET as i64)?
        .add_i64((SPK_SPK_FIELD_OFFSET + SPK_FIELD_LEN) as i64)?
        .add_op(OpTxInputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [logic_tail]
fn push_input_script_from_index(sb: &mut ScriptBuilder, idx: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(idx)?
        .add_op(OpDup)?
        .add_op(OpTxInputSpkLen)?
        .add_i64(SPK_LOGIC_TAIL_OFFSET as i64)?
        .add_op(OpSwap)?
        .add_op(OpTxInputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [amount]
fn push_authorized_output_amount(sb: &mut ScriptBuilder, authorizing_input: i64, k: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(authorizing_input)?.add_i64(k)?.add_op(OpCovOutputIdx)?;
    push_output_amount_from_index(sb)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [spk_field]
fn push_auth_output_amount(sb: &mut ScriptBuilder, k: i64) -> Result<(), ScriptBuilderError> {
    push_authorized_output_amount(sb, TOKEN_AUTH_INPUT_INDEX, k)
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [spk_field]
fn push_authorized_output_spk_field(sb: &mut ScriptBuilder, authorizing_input: i64, k: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(authorizing_input)?.add_i64(k)?.add_op(OpCovOutputIdx)?;
    push_output_spk_field_from_index(sb)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [spk_field]
fn push_auth_output_spk_field(sb: &mut ScriptBuilder, k: i64) -> Result<(), ScriptBuilderError> {
    push_authorized_output_spk_field(sb, TOKEN_AUTH_INPUT_INDEX, k)
}

// assumed pre-stack: [output_index]
// assumed post-stack: [amount]
fn push_output_amount_from_index(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_i64(SPK_AMOUNT_OFFSET as i64)?
        .add_i64((SPK_AMOUNT_OFFSET + AMOUNT_LEN) as i64)?
        .add_op(OpTxOutputSpkSubstr)?
        .add_op(OpBin2Num)?;
    Ok(())
}

// assumed pre-stack: [output_index]
// assumed post-stack: [spk_field]
fn push_output_spk_field_from_index(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_i64(SPK_SPK_FIELD_OFFSET as i64)?.add_i64((SPK_SPK_FIELD_OFFSET + SPK_FIELD_LEN) as i64)?.add_op(OpTxOutputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: [output_index]
// assumed post-stack: [logic_tail]
fn push_output_script_from_index(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpDup)?;
    sb.add_op(OpTxOutputSpkLen)?;
    sb.add_i64(SPK_LOGIC_TAIL_OFFSET as i64)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpTxOutputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: [logic_tail]
fn push_current_script(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpTxInputIndex)?;
    sb.add_op(OpDup)?;
    sb.add_op(OpTxInputSpkLen)?;
    sb.add_i64(SPK_LOGIC_TAIL_OFFSET as i64)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpTxInputSpkSubstr)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// post-stack: []
/// Authorized output here means the k-th output that referenced the input authorithy.
/// Verify that this k-th output references `authorizing_input` as authorithy AND
/// that tail (covenant_script) matches, effectively checking if output_script matches current_script (script continuation).
fn verify_authorized_output_at_index_script_matches_authority_script(
    sb: &mut ScriptBuilder,
    authorizing_input: i64,
    k: i64,
) -> Result<(), ScriptBuilderError> {
    // input index that is expected to authorize
    sb.add_i64(authorizing_input)?;
    // get the absolute output index that has been authorized by authorizing_input
    sb.add_i64(k)?.add_op(OpCovOutputIdx)?;
    push_output_script_from_index(sb)?;
    push_current_script(sb)?;
    sb.add_op(OpEqualVerify)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// post-stack: []
fn verify_authorized_output_at_index_script_matches_input0_script(sb: &mut ScriptBuilder, k: i64) -> Result<(), ScriptBuilderError> {
    verify_authorized_output_at_index_script_matches_authority_script(sb, TOKEN_AUTH_INPUT_INDEX, k)
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: []
fn verify_input_script_at_index_matches_current_script(sb: &mut ScriptBuilder, idx: i64) -> Result<(), ScriptBuilderError> {
    push_input_script_from_index(sb, idx)?;
    push_current_script(sb)?;
    sb.add_op(OpEqualVerify)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// assumed post-stack: []
fn verify_authorized_output_at_index_script_matches_script(
    sb: &mut ScriptBuilder,
    k: i64,
    token_logic: &[u8],
) -> Result<(), ScriptBuilderError> {
    sb.add_i64(0)?.add_i64(k)?.add_op(OpCovOutputIdx)?;
    push_output_script_from_index(sb)?;
    sb.add_data(token_logic)?;
    sb.add_op(OpEqualVerify)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// post-stack: []
fn verify_authorized_output_spk_field_len(sb: &mut ScriptBuilder, authorizing_input: i64, k: i64) -> Result<(), ScriptBuilderError> {
    sb.add_i64(authorizing_input)?.add_i64(k)?.add_op(OpCovOutputIdx)?;
    sb.add_i64(SPK_FIELD_LEN_OFFSET as i64)?
        .add_i64((SPK_FIELD_LEN_OFFSET + 1) as i64)?
        .add_op(OpTxOutputSpkSubstr)?
        .add_i64(SPK_BYTES_MIN as i64)?
        .add_i64((SPK_BYTES_MAX + 1) as i64)?
        .add_op(OpWithin)?
        .add_op(OpVerify)?;
    Ok(())
}

// assumed pre-stack: no pre-stack assumptions
// post-stack: []
fn verify_auth_output_spk_field_len(sb: &mut ScriptBuilder, k: i64) -> Result<(), ScriptBuilderError> {
    verify_authorized_output_spk_field_len(sb, TOKEN_AUTH_INPUT_INDEX, k)
}

// assumed pre-stack: [amount]
// assumed post-stack: [amount]
fn assert_positive_amount(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpDup)?;
    sb.add_i64(1)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;
    Ok(())
}

// assumed pre-stack: [spk_field(len+padding), spk_bytes]
// post-stack: []
// behavior: throws if doesn't match
// TODO(enhancement): requires mental gymnastic, see possibility of simplication
fn verify_spk_field_matches_stack(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    // 1. compare length byte in spk_field with size(spk_bytes).
    sb.add_op(OpDup)?;
    sb.add_op(OpSize)?;

    // pick spk_field
    sb.add_i64(3)?;
    sb.add_op(OpPick)?;

    sb.add_i64(0)?;
    sb.add_i64(1)?;
    // return first byte = len(spk_field)
    sb.add_op(OpSubStr)?;
    sb.add_op(OpEqualVerify)?;

    // 2. compare spk_field (starting at byte 1: without length) with spk_bytes
    // at this stage, stack = [spk_field(len+padding), spk_bytes]
    sb.add_op(OpSize)?;
    sb.add_i64(3)?;
    sb.add_op(OpPick)?;
    sb.add_i64(1)?;
    sb.add_i64(2)?;
    sb.add_op(OpPick)?;

    // offset from 1 to skip length
    sb.add_i64(1)?;
    sb.add_op(OpAdd)?;
    sb.add_op(OpSubStr)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpEqualVerify)?;

    // Drop spk_field and spk_bytes.
    sb.add_op(Op2Drop)?;
    Ok(())
}

fn apply_covenant_info(outputs: &mut [TransactionOutput], covenant_id: Hash, authorizing_input: u16) {
    for output in outputs {
        output.covenant = Some(CovenantBinding { authorizing_input, covenant_id });
    }
}

fn estimate_mass(calculator: &MassCalculator, tx: &PopulatedTransaction<'_>) -> u64 {
    let storage_mass = calculator.calc_contextual_masses(tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(tx.tx);
    storage_mass.max(compute_mass).max(transient_mass)
}

fn checked_sub_or_err(available: u64, required: u64) -> CovenantResult<u64> {
    available.checked_sub(required).ok_or(CovenantError::InsufficientFunds { available, required })
}

fn even_split_values(total: u64, parts: usize) -> CovenantResult<Vec<u64>> {
    if parts == 0 {
        return Err(CovenantError::InvalidField("output_count"));
    }
    let base = total / parts as u64;
    let remainder = total - base * parts as u64;
    let mut values = vec![base; parts];
    if let Some(last) = values.last_mut() {
        *last = last.saturating_add(remainder);
    }
    if values.iter().any(|&value| value == 0) {
        return Err(CovenantError::InvalidField("output_value"));
    }
    Ok(values)
}

fn allocate_output_values_by_amount(total: u64, outputs: &[NativeAssetOutput]) -> CovenantResult<Vec<u64>> {
    if outputs.is_empty() {
        return Err(CovenantError::InvalidField("output_count"));
    }
    let total_amount: u64 = outputs.iter().map(|output| output.amount).sum();
    if total_amount == 0 {
        return Err(CovenantError::InvalidField("total_amount"));
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
        return Err(CovenantError::InvalidField("output_value"));
    }
    Ok(values)
}

fn validate_spk_bytes(spk_bytes: &[u8], field: &'static str) -> Result<(), CovenantError> {
    let len = spk_bytes.len();
    if len < SPK_BYTES_MIN || len > SPK_BYTES_MAX {
        return Err(CovenantError::SpkBytesLengthOutOfRange { field, min: SPK_BYTES_MIN, max: SPK_BYTES_MAX, actual: len });
    }
    Ok(())
}

fn encode_spk_field(spk_bytes: &[u8]) -> CovenantResult<Vec<u8>> {
    validate_spk_bytes(spk_bytes, "spk_bytes")?;
    let len = spk_bytes.len();
    let mut field = vec![0u8; SPK_FIELD_LEN];
    field[0] = len as u8;
    field[1..1 + len].copy_from_slice(spk_bytes);
    Ok(field)
}

fn decode_spk_field(spk_bytes: &[u8], field_start: usize, field: &'static str) -> CovenantResult<Vec<u8>> {
    let field_end = field_start + SPK_FIELD_LEN;
    let field_bytes = spk_bytes.get(field_start..field_end).ok_or(CovenantError::InvalidField(field))?;
    let len = field_bytes[0] as usize;
    if len < SPK_BYTES_MIN || len > SPK_BYTES_MAX {
        return Err(CovenantError::SpkBytesLengthOutOfRange { field, min: SPK_BYTES_MIN, max: SPK_BYTES_MAX, actual: len });
    }
    if field_bytes[1 + len..].iter().any(|&b| b != 0) {
        return Err(CovenantError::InvalidField(field));
    }
    Ok(field_bytes[1..1 + len].to_vec())
}

fn decode_amount(spk_bytes: &[u8], field: &'static str) -> CovenantResult<u64> {
    let start = SPK_AMOUNT_OFFSET;
    let end = start + AMOUNT_LEN;
    let bytes = spk_bytes.get(start..end).ok_or(CovenantError::InvalidField(field))?;
    Ok(decode_u64_le(bytes, field)?)
}

fn decode_covenant_id(spk_bytes: &[u8]) -> CovenantResult<Hash> {
    let start = SPK_COVENANT_ID_OFFSET;
    let end = start + COVENANT_ID_LEN;
    let bytes = spk_bytes.get(start..end).ok_or(CovenantError::InvalidField("covenant_id"))?;
    Ok(Hash::from_slice(bytes))
}
