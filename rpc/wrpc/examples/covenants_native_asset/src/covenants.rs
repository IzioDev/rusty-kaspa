use kaspa_consensus_core::hashing::tx::transaction_id_preimage;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, ScriptPublicKey, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_txscript::opcodes::codes::{
    Op1Sub, Op2Dup, OpBlake2bWithKey, OpCat, OpData1, OpDrop, OpDup, OpElse, OpEndIf, OpEqual, OpEqualVerify, OpGreaterThanOrEqual,
    OpIf, OpNumEqualVerify, OpOutpointTxId, OpPick, OpSize, OpSub, OpSubStr, OpSwap, OpTrue, OpTxInputCount, OpTxInputIndex,
    OpTxInputSpk, OpTxOutputCount, OpTxOutputSpk, OpTxPayloadLen, OpTxPayloadSubstr, OpVerify,
};
use kaspa_txscript::script_builder::{ScriptBuilder, ScriptBuilderError};
use kaspa_txscript::SpkEncoding;
use std::convert::TryInto;

pub use crate::errors::CovenantError;
use crate::result::CovenantResult;
use crate::scriptnum::{append_scriptnum_padded_u64, decode_scriptnum_padded_u64};

const PAYLOAD_MAGIC: &[u8; 6] = b"KNAT20";
const PAYLOAD_VERSION: u8 = 1;
pub const PAYLOAD_U64_SIZE: usize = 8;

// runtime validation purpose
const SPK_BYTES_MIN: usize = 36; // version (2) + script (34)
const ASSET_ID_SIZE: usize = 36; // txid (32) + index (4)
const SPK_BYTES_MAX: usize = 37; // version (2) + script (35)

// payload bytes layout
const OFF_MAGIC_START: usize = 0;
const OFF_MAGIC_END: usize = OFF_MAGIC_START + PAYLOAD_MAGIC.len();
const OFF_VERSION_START: usize = OFF_MAGIC_END;
const OFF_VERSION_END: usize = OFF_VERSION_START + 1;
const OFF_ASSET_ID_START: usize = OFF_VERSION_END;
const OFF_ASSET_ID_END: usize = OFF_ASSET_ID_START + ASSET_ID_SIZE;
const OFF_AUTHORITY_SPK_LEN_START: usize = OFF_ASSET_ID_END;
const OFF_AUTHORITY_SPK_LEN_END: usize = OFF_AUTHORITY_SPK_LEN_START + 1;
const OFF_AUTHORITY_SPK_START: usize = OFF_AUTHORITY_SPK_LEN_END;
const OFF_AUTHORITY_SPK_END: usize = OFF_AUTHORITY_SPK_START + SPK_BYTES_MAX;
const OFF_TOKEN_SPK_LEN_START: usize = OFF_AUTHORITY_SPK_END;
const OFF_TOKEN_SPK_LEN_END: usize = OFF_TOKEN_SPK_LEN_START + 1;
const OFF_TOKEN_SPK_START: usize = OFF_TOKEN_SPK_LEN_END;
const OFF_TOKEN_SPK_END: usize = OFF_TOKEN_SPK_START + SPK_BYTES_MAX;
const OFF_REMAINING_SUPPLY_START: usize = OFF_TOKEN_SPK_END;
const OFF_REMAINING_SUPPLY_END: usize = OFF_REMAINING_SUPPLY_START + PAYLOAD_U64_SIZE;
const OFF_OP_START: usize = OFF_REMAINING_SUPPLY_END;
const OFF_OP_END: usize = OFF_OP_START + 1;
const OFF_AMOUNT_START: usize = OFF_OP_END;
const OFF_AMOUNT_END: usize = OFF_AMOUNT_START + PAYLOAD_U64_SIZE;
const OFF_RECIPIENT_SPK_LEN_START: usize = OFF_AMOUNT_END;
const OFF_RECIPIENT_SPK_LEN_END: usize = OFF_RECIPIENT_SPK_LEN_START + 1;
const OFF_RECIPIENT_SPK_START: usize = OFF_RECIPIENT_SPK_LEN_END;
const OFF_RECIPIENT_SPK_END: usize = OFF_RECIPIENT_SPK_START + SPK_BYTES_MAX;
const OFF_REMAINING_SUPPLY_SN_LENP1_START: usize = OFF_RECIPIENT_SPK_END;
const OFF_REMAINING_SUPPLY_SN_LENP1_END: usize = OFF_REMAINING_SUPPLY_SN_LENP1_START + 1;
const OFF_REMAINING_SUPPLY_SN_START: usize = OFF_REMAINING_SUPPLY_SN_LENP1_END;
const OFF_REMAINING_SUPPLY_SN_END: usize = OFF_REMAINING_SUPPLY_SN_START + PAYLOAD_U64_SIZE;
const OFF_AMOUNT_SN_LENP1_START: usize = OFF_REMAINING_SUPPLY_SN_END;
const OFF_AMOUNT_SN_LENP1_END: usize = OFF_AMOUNT_SN_LENP1_START + 1;
const OFF_AMOUNT_SN_START: usize = OFF_AMOUNT_SN_LENP1_END;
const OFF_AMOUNT_SN_END: usize = OFF_AMOUNT_SN_START + PAYLOAD_U64_SIZE;
const PAYLOAD_LEN: usize = OFF_AMOUNT_SN_END;

// pre-image bytes layout
const TX_VERSION_SIZE: usize = 2;
const U64_SIZE: usize = 8;
const OUTPOINT_TXID_SIZE: usize = 32;
const OUTPOINT_INDEX_SIZE: usize = 4;
const INPUT_NO_SIG_SCRIPT_SIZE: usize = OUTPOINT_TXID_SIZE + OUTPOINT_INDEX_SIZE + U64_SIZE + U64_SIZE;
const OUTPUT_VALUE_SIZE: usize = 8;
const SPK_VERSION_SIZE: usize = 2;
const SPK_SCRIPT_LEN_SIZE: usize = U64_SIZE;

// Offsets into transaction_id_preimage() for parent input0 prevout fields.
// Layout is: tx_version (2) + input_count (u64) + input0.prevout(txid + index) + ...
// Used by the covenant to extract the parent input0 prevout for asset_id derivation and GP binding.
const PARENT_INPUT0_PREVOUT_TXID_START: usize = TX_VERSION_SIZE + U64_SIZE;
const PARENT_INPUT0_PREVOUT_TXID_END: usize = PARENT_INPUT0_PREVOUT_TXID_START + OUTPOINT_TXID_SIZE;
const PARENT_INPUT0_PREVOUT_INDEX_START: usize = PARENT_INPUT0_PREVOUT_TXID_END;
const PARENT_INPUT0_PREVOUT_INDEX_END: usize = PARENT_INPUT0_PREVOUT_INDEX_START + OUTPOINT_INDEX_SIZE;

const TOKEN_OP_MINT: u8 = 0;
const TOKEN_OP_TRANSFER: u8 = 2;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAssetOp {
    Mint = TOKEN_OP_MINT,
    TokenTransfer = TOKEN_OP_TRANSFER,
}

impl NativeAssetOp {
    fn from_byte(value: u8) -> Option<Self> {
        match value {
            TOKEN_OP_MINT => Some(Self::Mint),
            TOKEN_OP_TRANSFER => Some(Self::TokenTransfer),
            _ => None,
        }
    }
}

/// KNAT20 payload encoded as fixed offsets with little-endian u64 fields plus ScriptNum copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAssetPayload {
    /// outpoint_txid || outpoint_index_le of the mint's input 0.
    pub asset_id: [u8; ASSET_ID_SIZE],
    /// spk.to_bytes() (version + script)
    pub authority_spk_bytes: Vec<u8>,
    /// spk.to_bytes() (version + script)
    pub token_spk_bytes: Vec<u8>,
    pub remaining_supply: u64,
    pub op: NativeAssetOp,
    pub amount: u64,
    /// spk.to_bytes() (version + script)
    pub recipient_spk_bytes: Vec<u8>,
}

impl NativeAssetPayload {
    /// Returns the new payload after minting.
    /// Errors if the amount exceeds the remaining supply.
    pub fn mint_next(&self, amount: u64, recipient_spk_bytes: &[u8]) -> Result<Self, CovenantError> {
        validate_spk_bytes(recipient_spk_bytes, "recipient_spk_bytes")?;
        let remaining = self
            .remaining_supply
            .checked_sub(amount)
            .ok_or(CovenantError::AmountExceedsRemainingSupply { remaining: self.remaining_supply, amount })?;

        Ok(Self {
            asset_id: self.asset_id,
            authority_spk_bytes: self.authority_spk_bytes.clone(),
            token_spk_bytes: self.token_spk_bytes.clone(),
            remaining_supply: remaining,
            op: NativeAssetOp::Mint,
            amount,
            recipient_spk_bytes: recipient_spk_bytes.to_vec(),
        })
    }

    /// Returns the new payload after transferring.
    pub fn token_transfer_next(&self, new_recipient_spk_bytes: &[u8]) -> Result<Self, CovenantError> {
        validate_spk_bytes(new_recipient_spk_bytes, "recipient_spk_bytes")?;
        Ok(Self {
            asset_id: self.asset_id,
            authority_spk_bytes: self.authority_spk_bytes.clone(),
            token_spk_bytes: self.token_spk_bytes.clone(),
            remaining_supply: self.remaining_supply,
            op: NativeAssetOp::TokenTransfer,
            amount: self.amount,
            recipient_spk_bytes: new_recipient_spk_bytes.to_vec(),
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, CovenantError> {
        let mut payload = Vec::with_capacity(PAYLOAD_LEN);
        payload.extend_from_slice(PAYLOAD_MAGIC);
        payload.push(PAYLOAD_VERSION);
        payload.extend_from_slice(&self.asset_id);
        append_spk_bytes(&mut payload, &self.authority_spk_bytes, "authority_spk_bytes")?;
        append_spk_bytes(&mut payload, &self.token_spk_bytes, "token_spk_bytes")?;
        payload.extend_from_slice(&self.remaining_supply.to_le_bytes());
        payload.push(self.op as u8);
        payload.extend_from_slice(&self.amount.to_le_bytes());
        append_spk_bytes(&mut payload, &self.recipient_spk_bytes, "recipient_spk_bytes")?;

        append_scriptnum_padded_u64(&mut payload, self.remaining_supply, "remaining_supply")?;
        append_scriptnum_padded_u64(&mut payload, self.amount, "amount")?;

        Ok(payload)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, CovenantError> {
        if payload.len() != PAYLOAD_LEN {
            return Err(CovenantError::InvalidPayloadLength { expected: PAYLOAD_LEN, actual: payload.len() });
        }
        if &payload[OFF_MAGIC_START..OFF_MAGIC_END] != PAYLOAD_MAGIC {
            return Err(CovenantError::InvalidPayloadMagic);
        }
        let version = payload[OFF_VERSION_START];
        if version != PAYLOAD_VERSION {
            return Err(CovenantError::InvalidPayloadVersion { expected: PAYLOAD_VERSION, actual: version });
        }

        let asset_id =
            payload[OFF_ASSET_ID_START..OFF_ASSET_ID_END].try_into().map_err(|_| CovenantError::InvalidField("asset_id"))?;
        let authority_spk_bytes = decode_spk_bytes(
            payload,
            OFF_AUTHORITY_SPK_LEN_START,
            OFF_AUTHORITY_SPK_START,
            OFF_AUTHORITY_SPK_END,
            "authority_spk_bytes",
        )?;
        let token_spk_bytes =
            decode_spk_bytes(payload, OFF_TOKEN_SPK_LEN_START, OFF_TOKEN_SPK_START, OFF_TOKEN_SPK_END, "token_spk_bytes")?;
        let remaining_supply = u64::from_le_bytes(
            payload[OFF_REMAINING_SUPPLY_START..OFF_REMAINING_SUPPLY_END]
                .try_into()
                .map_err(|_| CovenantError::InvalidField("remaining_supply"))?,
        );
        let op_byte = payload[OFF_OP_START];
        let op = NativeAssetOp::from_byte(op_byte).ok_or(CovenantError::InvalidPayloadOp { value: op_byte })?;
        let amount = u64::from_le_bytes(
            payload[OFF_AMOUNT_START..OFF_AMOUNT_END].try_into().map_err(|_| CovenantError::InvalidField("amount"))?,
        );
        let recipient_spk_bytes = decode_spk_bytes(
            payload,
            OFF_RECIPIENT_SPK_LEN_START,
            OFF_RECIPIENT_SPK_START,
            OFF_RECIPIENT_SPK_END,
            "recipient_spk_bytes",
        )?;

        let remaining_supply_sn = decode_scriptnum_padded_u64(
            payload[OFF_REMAINING_SUPPLY_SN_LENP1_START],
            &payload[OFF_REMAINING_SUPPLY_SN_START..OFF_REMAINING_SUPPLY_SN_END],
            "remaining_supply",
        )?;
        if remaining_supply_sn != remaining_supply {
            return Err(CovenantError::ScriptNumMismatch {
                field: "remaining_supply",
                encoded: remaining_supply_sn,
                expected: remaining_supply,
            });
        }

        let amount_sn = decode_scriptnum_padded_u64(
            payload[OFF_AMOUNT_SN_LENP1_START],
            &payload[OFF_AMOUNT_SN_START..OFF_AMOUNT_SN_END],
            "amount",
        )?;
        if amount_sn != amount {
            return Err(CovenantError::ScriptNumMismatch { field: "amount", encoded: amount_sn, expected: amount });
        }

        Ok(Self { asset_id, authority_spk_bytes, token_spk_bytes, remaining_supply, op, amount, recipient_spk_bytes })
    }
}

/// spk bytes length is 36 or 37
fn validate_spk_len(len: usize, field: &'static str) -> Result<(), CovenantError> {
    if !(SPK_BYTES_MIN..=SPK_BYTES_MAX).contains(&len) {
        return Err(CovenantError::SpkBytesLengthOutOfRange { field, min: SPK_BYTES_MIN, max: SPK_BYTES_MAX, actual: len });
    }
    Ok(())
}

fn validate_spk_bytes(spk_bytes: &[u8], field: &'static str) -> Result<(), CovenantError> {
    validate_spk_len(spk_bytes.len(), field)
}

fn append_spk_bytes(payload: &mut Vec<u8>, spk_bytes: &[u8], field: &'static str) -> Result<(), CovenantError> {
    validate_spk_len(spk_bytes.len(), field)?;
    payload.push(spk_bytes.len() as u8);
    payload.extend_from_slice(spk_bytes);
    // fill with 0 if not = SPK_BYTES_MAX (static layout)
    payload.resize(payload.len() + (SPK_BYTES_MAX - spk_bytes.len()), 0);
    Ok(())
}

fn decode_spk_bytes(
    payload: &[u8],
    len_start: usize,
    bytes_start: usize,
    bytes_end: usize,
    field: &'static str,
) -> Result<Vec<u8>, CovenantError> {
    let len = *payload.get(len_start).ok_or(CovenantError::InvalidField(field))? as usize;
    validate_spk_len(len, field)?;
    let bytes = payload.get(bytes_start..bytes_end).ok_or(CovenantError::InvalidField(field))?;

    // non 0 detected after supposedly finished spk bytes
    if bytes[len..].iter().any(|&b| b != 0) {
        return Err(CovenantError::InvalidField(field));
    }
    Ok(bytes[..len].to_vec())
}

/// Holds the current covenant UTXO state.
#[derive(Clone)]
pub struct NativeAssetState {
    knat_backtrace: KnatBacktrace,
    utxo_outpoint: TransactionOutpoint,
    utxo_entry: UtxoEntry,
    pub payload: NativeAssetPayload,
}

impl NativeAssetState {
    pub fn from_tx_with_entry_at_index(
        tx: Transaction,
        utxo_entry: UtxoEntry,
        knat_backtrace: KnatBacktrace,
        output_index: u32,
    ) -> CovenantResult<Self> {
        let payload = NativeAssetPayload::decode(&tx.payload)?;
        let outpoint = TransactionOutpoint::new(tx.id(), output_index);
        Ok(Self { knat_backtrace, utxo_outpoint: outpoint, utxo_entry, payload })
    }

    pub fn from_tx_with_entry_and_grandparent(
        tx: Transaction,
        utxo_entry: UtxoEntry,
        grandparent_tx: &Transaction,
    ) -> CovenantResult<Self> {
        Self::from_tx_with_entry_and_grandparent_at_index(tx, utxo_entry, grandparent_tx, 0)
    }

    pub fn from_tx_with_entry_and_grandparent_at_index(
        tx: Transaction,
        utxo_entry: UtxoEntry,
        grandparent_tx: &Transaction,
        output_index: u32,
    ) -> CovenantResult<Self> {
        let knat_backtrace = KnatBacktrace::from_parent_and_grandparent(&tx, grandparent_tx)?;
        Self::from_tx_with_entry_at_index(tx, utxo_entry, knat_backtrace, output_index)
    }

    pub fn utxo_entry(&self) -> &UtxoEntry {
        &self.utxo_entry
    }

    pub fn utxo_outpoint(&self) -> &TransactionOutpoint {
        &self.utxo_outpoint
    }

    fn build_sig_script(&self, covenant_script: &[u8]) -> CovenantResult<Vec<u8>> {
        build_sig_script_from_backtrace(&self.knat_backtrace, covenant_script)
    }
}

/// Holds witness fragments for KNAT parent/grandparent verification.
#[derive(Clone)]
pub struct KnatBacktrace {
    /// Grandparent preimage without payload.
    pub gp_preimage: Vec<u8>,
    /// Raw spk bytes (version + script) of grandparent output0 (kept separate for KNAT gating).
    pub gp_output0_script: Vec<u8>,
    /// Grandparent payload bytes.
    pub gp_payload: Vec<u8>,
    /// Parent preimage without payload (header + inputs + outputs).
    pub parent_preimage: Vec<u8>,
    /// Parent payload bytes.
    pub parent_payload: Vec<u8>,
}

impl KnatBacktrace {
    pub fn from_parent_and_grandparent(parent: &Transaction, grandparent: &Transaction) -> CovenantResult<Self> {
        let (parent_preimage, parent_payload) = split_preimage_payload(parent)?;
        let gp_parts = split_grandparent_preimage_for_knat(grandparent)?;
        Ok(Self {
            gp_preimage: gp_parts.preimage,
            gp_output0_script: gp_parts.output0_script,
            gp_payload: gp_parts.payload,
            parent_preimage,
            parent_payload,
        })
    }
}

fn split_preimage_payload(tx: &Transaction) -> CovenantResult<(Vec<u8>, Vec<u8>)> {
    // transaction_id_preimage returns payload at the end; split to reuse the prefix/payload separately.
    let preimage = transaction_id_preimage(tx);
    let payload_len = tx.payload.len();
    let split_at = preimage
        .len()
        .checked_sub(payload_len)
        // shouldn't happen
        .ok_or(CovenantError::PayloadLargerThanPreimage { payload_len, preimage_len: preimage.len() })?;
    let (preimage_prefix, payload_bytes) = preimage.split_at(split_at);
    Ok((preimage_prefix.to_vec(), payload_bytes.to_vec()))
}

struct GrandparentPreimageParts {
    preimage: Vec<u8>,
    output0_script: Vec<u8>,
    payload: Vec<u8>,
}

fn split_grandparent_preimage_for_knat(tx: &Transaction) -> CovenantResult<GrandparentPreimageParts> {
    let (preimage_without_payload, payload_bytes) = split_preimage_payload(tx)?;
    let output0 = tx.outputs.get(0).ok_or(CovenantError::MissingGrandparentOutput0)?;
    let out0_spk = output0.script_public_key.to_bytes();
    let out0_script = output0.script_public_key.script();

    // Output0 script is embedded in the preimage; validate its length.
    let prefix_len = TX_VERSION_SIZE
        + U64_SIZE
        + tx.inputs.len() * INPUT_NO_SIG_SCRIPT_SIZE
        + U64_SIZE
        + OUTPUT_VALUE_SIZE
        + SPK_VERSION_SIZE
        + SPK_SCRIPT_LEN_SIZE;
    let script_start = prefix_len;
    let script_end = script_start + out0_script.len();
    if preimage_without_payload.len() < script_end {
        return Err(CovenantError::GrandparentPreimageLengthMismatch {
            expected_len: script_end,
            actual_len: preimage_without_payload.len(),
        });
    }

    let script_len_slice = preimage_without_payload.get(prefix_len - SPK_SCRIPT_LEN_SIZE..prefix_len).ok_or(
        CovenantError::GrandparentPreimageLengthMismatch { expected_len: prefix_len, actual_len: preimage_without_payload.len() },
    )?;
    let script_len =
        u64::from_le_bytes(script_len_slice.try_into().map_err(|_| CovenantError::InvalidField("gp_output0_script_len"))?);
    let expected_len = out0_script.len() as u64;
    if script_len != expected_len {
        return Err(CovenantError::GrandparentOutputScriptLenMismatch { expected_len, actual_len: script_len });
    }

    Ok(GrandparentPreimageParts { preimage: preimage_without_payload, output0_script: out0_spk, payload: payload_bytes })
}

/// Build a signature script from KNAT backtrace fragments.
pub fn build_sig_script_from_backtrace(backtrace: &KnatBacktrace, covenant_script: &[u8]) -> CovenantResult<Vec<u8>> {
    // Stack order (top -> bottom) inside the covenant script after redeem: parent_payload, parent_preimage, gp_payload,
    // gp_output0_script, gp_preimage.
    let mut sb = ScriptBuilder::new();
    sb.add_data(&backtrace.gp_preimage)?
        .add_data(&backtrace.gp_output0_script)?
        .add_data(&backtrace.gp_payload)?
        .add_data(&backtrace.parent_preimage)?
        .add_data(&backtrace.parent_payload)?
        .add_data(covenant_script)?;
    Ok(sb.drain())
}

/// Verify parent/grandparent binding and leave a boolean for "continuation vs genesis".
pub fn knat_verify_parent_and_grandparent(sb: &mut ScriptBuilder) -> Result<(), ScriptBuilderError> {
    // Entry stack (top -> bottom), produced by build_sig_script_from_backtrace:
    // parent_payload: raw payload bytes
    // parent_preimage: transaction_id_preimage(parent) without payload
    // gp_payload: raw grandparent payload bytes
    // gp_output0_script: raw spk bytes (version + script) of grandparent output0
    // gp_preimage: grandparent preimage without payload
    //
    // Stack depths below are tied to this shape; 0 = top.
    const DEPTH_PARENT_PREIMAGE: i64 = 1;
    const DEPTH_PARENT_PREIMAGE_WITH_PREVOUT: i64 = 3;
    const DEPTH_GP_PREIMAGE_WITH_PARENT_PREVOUT: i64 = 7;
    const DEPTH_GP_PAYLOAD_WITH_PARENT_PREVOUT: i64 = 5;

    const DEPTH_GP_OUT0_SCRIPT_AFTER_KNAT_CHECK: i64 = 5;

    // --- 1) Bind the parent txid to the spending outpoint ---
    // Compute parent_txid = blake2b("TransactionID", parent_preimage || parent_payload),
    // then compare it to the txid referenced by the current input's outpoint.
    sb.add_op(Op2Dup)?
        .add_op(OpCat)?
        .add_data(b"TransactionID")?
        .add_op(OpBlake2bWithKey)?
        .add_op(OpTxInputIndex)?
        .add_op(OpOutpointTxId)?
        .add_op(OpEqualVerify)?;

    // --- 2) Extract parent input0 prevout (txid + index) from parent_preimage ---
    // Leaves prevout_txid and prevout_index on stack for:
    // - binding grandparent txid, and
    // - genesis asset_id derivation (prevout_txid || prevout_index).
    sb.add_i64(DEPTH_PARENT_PREIMAGE)?
        .add_op(OpPick)?
        .add_i64(PARENT_INPUT0_PREVOUT_TXID_START as i64)?
        .add_i64(PARENT_INPUT0_PREVOUT_TXID_END as i64)?
        .add_op(OpSubStr)?;
    sb.add_op(OpDup)?;

    sb.add_i64(DEPTH_PARENT_PREIMAGE_WITH_PREVOUT)?
        .add_op(OpPick)?
        .add_i64(PARENT_INPUT0_PREVOUT_INDEX_START as i64)?
        .add_i64(PARENT_INPUT0_PREVOUT_INDEX_END as i64)?
        .add_op(OpSubStr)?;
    sb.add_op(OpDup)?;

    // Enforce input0 index == 0 to make asset_id derivation deterministic.
    sb.add_data(&[0u8, 0u8, 0u8, 0u8])?;
    sb.add_op(OpEqualVerify)?;

    // Arrange stack so prevout_txid sits above prevout_index for the gp binding below.
    sb.add_op(OpSwap)?;

    // --- 3) Bind grandparent txid to parent_prev_txid ---
    // Compute gp_txid = blake2b("TransactionID", gp_preimage || gp_payload)
    // and compare it to parent_prev_txid left on the stack.
    sb.add_i64(DEPTH_GP_PREIMAGE_WITH_PARENT_PREVOUT)?.add_op(OpPick)?;
    // After the previous pick, gp_payload is one item deeper.
    sb.add_i64(DEPTH_GP_PAYLOAD_WITH_PARENT_PREVOUT + 1)?.add_op(OpPick)?;
    sb.add_op(OpCat)?.add_data(b"TransactionID")?.add_op(OpBlake2bWithKey)?.add_op(OpEqualVerify)?;

    // --- 4) gate: compare gp_output0_script with current covenant spk bytes ---
    // Leaves a boolean on the stack for callers to branch on:
    // true = continuation (gp_output0_script matches this covenant), false = genesis.
    sb.add_i64(DEPTH_GP_OUT0_SCRIPT_AFTER_KNAT_CHECK)?.add_op(OpPick)?;
    sb.add_op(OpTxInputIndex)?.add_op(OpTxInputSpk)?.add_op(OpEqual)?;

    // Stack after this function (top -> bottom):
    // is_continuation, prevout_index, prevout_txid, parent_payload, parent_preimage,
    // gp_payload, gp_output0_script, gp_preimage.
    Ok(())
}

// Verifies that the provided spk bytes match the current payload field (len byte + bytes prefix).
// Stack in: spk_bytes, ...
// Stack out: ... (consumes spk_bytes)
fn verify_spk_matches_current_payload(
    sb: &mut ScriptBuilder,
    len_start: usize,
    len_end: usize,
    bytes_start: usize,
    bytes_end: usize,
) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpDup)?
        .add_op(OpSize)?
        .add_i64(len_start as i64)?
        .add_i64(len_end as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpSwap)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDrop)?;
    sb.add_i64(len_start as i64)?.add_i64(len_end as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_i64(bytes_start as i64)?.add_i64(bytes_end as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_op(OpSwap)?;
    sb.add_i64(0)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpSubStr)?;
    sb.add_op(OpEqualVerify)?;
    Ok(())
}

// Verifies that the provided spk bytes match the parent payload field (len byte + bytes prefix).
// Stack in: spk_bytes, parent_payload, ...
// Stack out: parent_payload, ... (consumes spk_bytes)
fn verify_spk_matches_parent_payload(
    sb: &mut ScriptBuilder,
    len_start: usize,
    len_end: usize,
    bytes_start: usize,
    bytes_end: usize,
) -> Result<(), ScriptBuilderError> {
    sb.add_op(OpDup)?
        .add_op(OpSize)?
        .add_i64(3)?
        .add_op(OpPick)?
        .add_i64(len_start as i64)?
        .add_i64(len_end as i64)?
        .add_op(OpSubStr)?
        .add_op(OpSwap)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDrop)?;
    sb.add_i64(1)?.add_op(OpPick)?.add_i64(bytes_start as i64)?.add_i64(bytes_end as i64)?.add_op(OpSubStr)?;
    sb.add_i64(2)?.add_op(OpPick)?.add_i64(len_start as i64)?.add_i64(len_end as i64)?.add_op(OpSubStr)?;
    sb.add_i64(0)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpSubStr)?;
    sb.add_op(OpEqualVerify)?;
    Ok(())
}

/// minter covenant enforcing mint payloads and covenant output checks.
/// Output1 is bound to the token covenant spk bytes carried in the payload (in the payload: to avoid circular deps minter<-->token covenants).
pub fn build_minter_covenant_script_knat20(authority_spk: &[u8]) -> Result<Vec<u8>, CovenantError> {
    let mut sb = ScriptBuilder::new();
    validate_spk_bytes(authority_spk, "authority_spk_bytes")?;

    knat_verify_parent_and_grandparent(&mut sb)?;

    // Stack after knat_verify (top -> bottom):
    // is_continuation, prevout_index, prevout_txid, parent_payload, parent_preimage, gp_payload, gp_output0_script, gp_preimage.
    //
    // KNAT gate result drives asset_id logic:
    // - continuation: drop prevout data and keep the asset_id from payload,
    // - genesis: recompute asset_id = prevout_txid || prevout_index and compare to payload.
    sb.add_op(OpIf)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpElse)?;

    sb.add_op(OpCat)?;
    sb.add_i64(1)?
        .add_op(OpPick)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpSubStr)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpEndIf)?;

    // --- Payload header: validate length, magic, version ---
    sb.add_op(OpTxPayloadLen)?;
    sb.add_i64(PAYLOAD_LEN as i64)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_MAGIC_START as i64)?.add_i64(OFF_MAGIC_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_data(PAYLOAD_MAGIC)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_VERSION_START as i64)?.add_i64(OFF_VERSION_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_i64(PAYLOAD_VERSION as i64)?;
    sb.add_op(OpEqualVerify)?;

    // --- Current payload op = mint ---
    sb.add_i64(OFF_OP_START as i64)?.add_i64(OFF_OP_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_ops(&[OpData1, TOKEN_OP_MINT])?;
    sb.add_op(OpEqualVerify)?;

    // --- Parent payload op = mint (mint chain can only follow mint) ---
    // note: on the first mint, this works because we ensure the genesis tx contains OP_MINT
    // an alternative would be to make the following check conditional based on GATE path (genesis vs continuation)
    // for the sake of simplicity and readability, i suggest we keep it as is
    sb.add_op(OpDup)?;
    sb.add_i64(OFF_OP_START as i64)?.add_i64(OFF_OP_END as i64)?.add_op(OpSubStr)?;
    sb.add_ops(&[OpData1, TOKEN_OP_MINT])?;
    sb.add_op(OpEqualVerify)?;

    // --- Fields that must be inherited from parent payload ---
    sb.add_op(OpDup)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDup)?
        .add_i64(OFF_AUTHORITY_SPK_LEN_START as i64)?
        .add_i64(OFF_AUTHORITY_SPK_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_AUTHORITY_SPK_LEN_START as i64)?
        .add_i64(OFF_AUTHORITY_SPK_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDup)?
        .add_i64(OFF_TOKEN_SPK_LEN_START as i64)?
        .add_i64(OFF_TOKEN_SPK_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_TOKEN_SPK_LEN_START as i64)?
        .add_i64(OFF_TOKEN_SPK_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;

    // --- State transition verification: remaining_supply = parent_remaining_supply - amount ---
    // ScriptNum values are stored as len+1 plus 8-byte padded bytes to keep zero-length encodings minimal.

    // parent remaining supply (scriptnum) from parent payload.
    sb.add_op(OpDup)?
        .add_i64(OFF_REMAINING_SUPPLY_SN_LENP1_START as i64)?
        .add_i64(OFF_REMAINING_SUPPLY_SN_LENP1_END as i64)?
        .add_op(OpSubStr)?
        .add_op(Op1Sub)?;
    sb.add_i64(1)?
        .add_op(OpPick)?
        .add_i64(OFF_REMAINING_SUPPLY_SN_START as i64)?
        .add_i64(OFF_REMAINING_SUPPLY_SN_END as i64)?
        .add_op(OpSubStr)?;
    sb.add_op(OpSwap)?;
    sb.add_i64(0)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpSubStr)?;

    // current amount (scriptnum) from current payload.
    sb.add_i64(OFF_AMOUNT_SN_LENP1_START as i64)?
        .add_i64(OFF_AMOUNT_SN_LENP1_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(Op1Sub)?;
    sb.add_i64(OFF_AMOUNT_SN_START as i64)?.add_i64(OFF_AMOUNT_SN_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_op(OpSwap)?;
    sb.add_i64(0)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpSubStr)?;

    // current remaining supply (scriptnum) from current payload.
    sb.add_i64(OFF_REMAINING_SUPPLY_SN_LENP1_START as i64)?
        .add_i64(OFF_REMAINING_SUPPLY_SN_LENP1_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(Op1Sub)?;
    sb.add_i64(OFF_REMAINING_SUPPLY_SN_START as i64)?.add_i64(OFF_REMAINING_SUPPLY_SN_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_op(OpSwap)?;
    sb.add_i64(0)?;
    sb.add_op(OpSwap)?;
    sb.add_op(OpSubStr)?;

    // Stack now has (top to bottom):
    // current_remaining_supply, current_amount, parent_remaining_supply, parent_payload

    // Verify that parent_remaining_supply >= current_amount (amount doesn't exceed supply).
    sb.add_i64(2)?.add_op(OpPick)?; // parent_remaining_supply
    sb.add_i64(2)?.add_op(OpPick)?; // current_amount
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // Verify that current_remaining_supply == parent_remaining_supply - current_amount.
    sb.add_i64(2)?.add_op(OpPick)?; // parent_remaining_supply
    sb.add_i64(2)?.add_op(OpPick)?; // current_amount
    sb.add_op(OpSub)?;
    sb.add_i64(1)?.add_op(OpPick)?; // current_remaining_supply
    sb.add_op(OpNumEqualVerify)?;

    // Clean up stack - drop the supply check verification values.
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;

    // Authority spk bytes must match provided authority spk.
    // note: authorithy isn't transferable as of now, it could be the role of a future covenant OP
    sb.add_data(authority_spk)?;
    verify_spk_matches_current_payload(
        &mut sb,
        OFF_AUTHORITY_SPK_LEN_START,
        OFF_AUTHORITY_SPK_LEN_END,
        OFF_AUTHORITY_SPK_START,
        OFF_AUTHORITY_SPK_END,
    )?;

    // Authorization input and covenant outputs:
    // - input[1] must spend the authority_spk,
    // - output[0] must loop back to this covenant,
    // - output[1] must be a token covenant whose script bytes match payload.token_spk_bytes.
    sb.add_op(OpTxInputCount)?;
    sb.add_i64(2)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    sb.add_i64(1)?;
    sb.add_op(OpTxInputSpk)?;
    sb.add_data(authority_spk)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_op(OpTxInputIndex)?.add_op(OpTxInputSpk)?.add_i64(0)?.add_op(OpTxOutputSpk)?.add_op(OpEqualVerify)?;

    sb.add_i64(1)?.add_op(OpTxOutputSpk)?;
    verify_spk_matches_current_payload(
        &mut sb,
        OFF_TOKEN_SPK_LEN_START,
        OFF_TOKEN_SPK_LEN_END,
        OFF_TOKEN_SPK_START,
        OFF_TOKEN_SPK_END,
    )?;

    sb.add_op(OpTxOutputCount)?.add_i64(2)?.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // Drop KNAT backtrace items (parent_payload, parent_preimage, gp_payload, gp_output0_script, gp_preimage) and leave true.
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpTrue)?;

    Ok(sb.drain())
}

/// Token covenant script. `minter_covenant_spk` binds mint-origin to the minter covenant.
pub fn build_token_covenant_script_knat20(minter_covenant_spk: &[u8]) -> Result<Vec<u8>, CovenantError> {
    let mut sb = ScriptBuilder::new();
    validate_spk_bytes(minter_covenant_spk, "minter_covenant_spk")?;

    knat_verify_parent_and_grandparent(&mut sb)?;

    // Stack after knat_verify (top -> bottom):
    // is_continuation, prevout_index, prevout_txid, parent_payload, parent_preimage, gp_payload, gp_output0_script, gp_preimage.
    //
    // Depths below assume this stack shape; 0 = top.
    const DEPTH_PARENT_PAYLOAD_WITH_PREVOUT: i64 = 2;
    const DEPTH_GP_OUT0_SCRIPT_WITH_PREVOUT: i64 = 5;

    // Genesis vs continuation:
    // - continuation: parent op must be token-transfer,
    // - genesis: parent op must be mint and gp_output0_script must match the minter covenant spk bytes.
    sb.add_op(OpIf)?;

    // verify its op is a token transfer
    sb.add_i64(DEPTH_PARENT_PAYLOAD_WITH_PREVOUT)?
        .add_op(OpPick)?
        .add_i64(OFF_OP_START as i64)?
        .add_i64(OFF_OP_END as i64)?
        .add_op(OpSubStr)?;
    sb.add_data(&[TOKEN_OP_TRANSFER])?;
    sb.add_op(OpEqualVerify)?;

    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;

    sb.add_op(OpElse)?;

    // verify its op is a mint
    sb.add_i64(DEPTH_PARENT_PAYLOAD_WITH_PREVOUT)?
        .add_op(OpPick)?
        .add_i64(OFF_OP_START as i64)?
        .add_i64(OFF_OP_END as i64)?
        .add_op(OpSubStr)?;
    sb.add_ops(&[OpData1, TOKEN_OP_MINT])?;
    sb.add_op(OpEqualVerify)?;

    // verify gp out script is the minter covenant tied to this token covenant
    sb.add_i64(DEPTH_GP_OUT0_SCRIPT_WITH_PREVOUT)?.add_op(OpPick)?.add_data(minter_covenant_spk)?.add_op(OpEqualVerify)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpEndIf)?;

    // Stack after branching (top -> bottom):
    // parent_payload, parent_preimage, gp_payload, gp_output0_script, gp_preimage.

    // --- Payload header: validate length, magic, version ---
    sb.add_op(OpTxPayloadLen)?;
    sb.add_i64(PAYLOAD_LEN as i64)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_MAGIC_START as i64)?.add_i64(OFF_MAGIC_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_data(PAYLOAD_MAGIC)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_VERSION_START as i64)?.add_i64(OFF_VERSION_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_i64(PAYLOAD_VERSION as i64)?;
    sb.add_op(OpEqualVerify)?;

    // --- Current payload op = token-transfer ---
    sb.add_i64(OFF_OP_START as i64)?.add_i64(OFF_OP_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_data(&[TOKEN_OP_TRANSFER])?;
    sb.add_op(OpEqualVerify)?;

    // --- Fields that must be inherited from parent payload ---
    sb.add_op(OpDup)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDup)?
        .add_i64(OFF_AUTHORITY_SPK_LEN_START as i64)?
        .add_i64(OFF_AUTHORITY_SPK_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_AUTHORITY_SPK_LEN_START as i64)?
        .add_i64(OFF_AUTHORITY_SPK_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDup)?
        .add_i64(OFF_TOKEN_SPK_LEN_START as i64)?
        .add_i64(OFF_TOKEN_SPK_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_TOKEN_SPK_LEN_START as i64)?
        .add_i64(OFF_TOKEN_SPK_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;
    sb.add_op(OpDup)?
        .add_i64(OFF_REMAINING_SUPPLY_START as i64)?
        .add_i64(OFF_REMAINING_SUPPLY_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_REMAINING_SUPPLY_START as i64)?
        .add_i64(OFF_REMAINING_SUPPLY_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;
    // for now, only allow full transfer (no split or merge)
    sb.add_op(OpDup)?
        .add_i64(OFF_AMOUNT_START as i64)?
        .add_i64(OFF_AMOUNT_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_AMOUNT_START as i64)?
        .add_i64(OFF_AMOUNT_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;

    // Current input spk bytes must match payload token_spk_bytes.
    sb.add_op(OpTxInputIndex)?.add_op(OpTxInputSpk)?;
    verify_spk_matches_current_payload(
        &mut sb,
        OFF_TOKEN_SPK_LEN_START,
        OFF_TOKEN_SPK_LEN_END,
        OFF_TOKEN_SPK_START,
        OFF_TOKEN_SPK_END,
    )?;

    // number of input >= 2
    sb.add_op(OpTxInputCount)?;
    sb.add_i64(2)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // Authorization input spk bytes must match parent recipient_spk_bytes
    sb.add_i64(1)?.add_op(OpTxInputSpk)?;
    verify_spk_matches_parent_payload(
        &mut sb,
        OFF_RECIPIENT_SPK_LEN_START,
        OFF_RECIPIENT_SPK_LEN_END,
        OFF_RECIPIENT_SPK_START,
        OFF_RECIPIENT_SPK_END,
    )?;

    // this input[n] == output[n]
    sb.add_op(OpTxInputIndex)?.add_op(OpTxInputSpk)?.add_op(OpTxInputIndex)?.add_op(OpTxOutputSpk)?.add_op(OpEqualVerify)?;

    // output number >= 1
    sb.add_op(OpTxOutputCount)?;
    sb.add_i64(1)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // Drop KNAT backtrace items (parent_payload, parent_preimage, gp_payload, gp_output0_script, gp_preimage) and leave true.
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;
    sb.add_op(OpTrue)?;

    Ok(sb.drain())
}

/// Build a mint transaction with an authorization input at index 1.
/// Uses the provided mass calculator to estimate fees.
pub fn build_mint_tx(
    state: &NativeAssetState,
    next_payload: &NativeAssetPayload,
    minter_spk: &ScriptPublicKey,
    token_spk: &ScriptPublicKey,
    token_value: u64,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    minter_covenant_script: &[u8],
    mass_calculator: &MassCalculator,
) -> CovenantResult<Transaction> {
    let payload = next_payload.encode()?;
    let minter_sig_script = state.build_sig_script(minter_covenant_script)?;

    let minter_input = TransactionInput::new(*state.utxo_outpoint(), minter_sig_script, 0, 0);

    let temp_minter_value = checked_sub_or_err(state.utxo_entry.amount, token_value)?;
    let temp_outputs =
        vec![TransactionOutput::new(temp_minter_value, minter_spk.clone()), TransactionOutput::new(token_value, token_spk.clone())];
    let temp_tx = Transaction::new(
        0,
        vec![minter_input.clone(), auth_input.clone()],
        temp_outputs,
        0,
        SubnetworkId::default(),
        0,
        payload.clone(),
    );
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![state.utxo_entry.clone(), auth_entry.clone()]);

    let mass = estimate_mass(mass_calculator, &temp_tx);

    let minter_value = checked_sub_or_err(temp_minter_value, mass)?;
    let outputs =
        vec![TransactionOutput::new(minter_value, minter_spk.clone()), TransactionOutput::new(token_value, token_spk.clone())];

    let mut tx = Transaction::new(0, vec![minter_input, auth_input], outputs, 0, SubnetworkId::default(), 0, payload);
    tx.finalize();
    Ok(tx)
}

/// Build a token transfer transaction with an authorization input at index 1.
/// Uses the provided mass calculator to estimate fees.
pub fn build_token_transfer_tx(
    state: &NativeAssetState,
    next_payload: &NativeAssetPayload,
    token_spk: &ScriptPublicKey,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    token_covenant_script: &[u8],
    mass_calculator: &MassCalculator,
) -> CovenantResult<Transaction> {
    let payload = next_payload.encode()?;
    let token_sig_script = state.build_sig_script(token_covenant_script)?;

    let token_input = TransactionInput::new(*state.utxo_outpoint(), token_sig_script, 0, 0);

    let temp_token_output = TransactionOutput::new(state.utxo_entry.amount, token_spk.clone());
    let temp_tx = Transaction::new(
        0,
        vec![token_input.clone(), auth_input.clone()],
        vec![temp_token_output],
        0,
        SubnetworkId::default(),
        0,
        payload.clone(),
    );
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![state.utxo_entry.clone(), auth_entry.clone()]);

    let mass = estimate_mass(mass_calculator, &temp_tx);

    let token_value = checked_sub_or_err(state.utxo_entry.amount, mass)?;
    let output = TransactionOutput::new(token_value, token_spk.clone());

    let mut tx = Transaction::new(0, vec![token_input, auth_input], vec![output], 0, SubnetworkId::default(), 0, payload);
    tx.finalize();
    Ok(tx)
}

fn estimate_mass(calculator: &MassCalculator, tx: &PopulatedTransaction<'_>) -> u64 {
    let storage_mass = calculator.calc_contextual_masses(tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(tx.tx);
    storage_mass.max(compute_mass).max(transient_mass)
}

fn checked_sub_or_err(available: u64, required: u64) -> CovenantResult<u64> {
    available.checked_sub(required).ok_or(CovenantError::InsufficientFunds { available, required })
}

/// outpoint_txid || outpoint_index_le
/// returns the asset_id for the outpoint
pub fn asset_id_for_outpoint(outpoint: &TransactionOutpoint) -> [u8; ASSET_ID_SIZE] {
    let mut out = [0u8; ASSET_ID_SIZE];
    out[..OUTPOINT_TXID_SIZE].copy_from_slice(outpoint.transaction_id.as_ref());
    out[OUTPOINT_TXID_SIZE..].copy_from_slice(&outpoint.index.to_le_bytes());
    out
}

/// spk.to_bytes() (version + script)
pub fn try_spk_bytes(spk: &ScriptPublicKey) -> CovenantResult<Vec<u8>> {
    let spk_bytes = spk.to_bytes();
    validate_spk_bytes(&spk_bytes, "spk_bytes")?;
    Ok(spk_bytes)
}
