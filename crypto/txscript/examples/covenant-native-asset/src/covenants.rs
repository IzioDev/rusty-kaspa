use kaspa_consensus_core::hashing::tx::transaction_id_preimage;
use kaspa_consensus_core::mass::{MassCalculator, NonContextualMasses};
use kaspa_consensus_core::network::NetworkType;
use kaspa_consensus_core::subnets::SubnetworkId;
use kaspa_consensus_core::tx::{
    PopulatedTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_txscript::opcodes::codes::{
    Op2Dup, OpBlake2b, OpBlake2bWithKey, OpCat, OpData1, OpDrop, OpDup, OpElse, OpEndIf, OpEqual, OpEqualVerify, OpGreaterThanOrEqual,
    OpIf, OpOutpointTxId, OpPick, OpSubStr, OpSwap, OpTrue, OpTxInputCount, OpTxInputIndex, OpTxInputSpk, OpTxInputSpkLen,
    OpTxOutputCount, OpTxOutputSpk, OpTxOutputSpkLen, OpTxPayloadLen, OpTxPayloadSubstr, OpVerify,
};
use kaspa_txscript::script_builder::ScriptBuilder;
use std::convert::TryInto;

const PAYLOAD_MAGIC: &[u8; 6] = b"KNAT20";
const PAYLOAD_VERSION: u8 = 1;
const MAX_SMALL_INT: u8 = 127;

const OFF_MAGIC_START: usize = 0;
const OFF_MAGIC_END: usize = OFF_MAGIC_START + PAYLOAD_MAGIC.len();
const OFF_VERSION_START: usize = OFF_MAGIC_END;
const OFF_VERSION_END: usize = OFF_VERSION_START + 1;
const OFF_ASSET_ID_START: usize = OFF_VERSION_END;
const OFF_ASSET_ID_END: usize = OFF_ASSET_ID_START + 32;
const OFF_AUTHORITY_HASH_START: usize = OFF_ASSET_ID_END;
const OFF_AUTHORITY_HASH_END: usize = OFF_AUTHORITY_HASH_START + 32;
const OFF_REMAINING_SUPPLY_START: usize = OFF_AUTHORITY_HASH_END;
const OFF_REMAINING_SUPPLY_END: usize = OFF_REMAINING_SUPPLY_START + 1;
const OFF_SEQ_START: usize = OFF_REMAINING_SUPPLY_END;
const OFF_SEQ_END: usize = OFF_SEQ_START + 1;
const OFF_OP_START: usize = OFF_SEQ_END;
const OFF_OP_END: usize = OFF_OP_START + 1;
const OFF_AMOUNT_START: usize = OFF_OP_END;
const OFF_AMOUNT_END: usize = OFF_AMOUNT_START + 1;
const OFF_RECIPIENT_HASH_START: usize = OFF_AMOUNT_END;
const OFF_RECIPIENT_HASH_END: usize = OFF_RECIPIENT_HASH_START + 32;
const PAYLOAD_LEN: usize = OFF_RECIPIENT_HASH_END;

const TX_VERSION_SIZE: usize = 2;
const U64_SIZE: usize = 8;
const OUTPOINT_TXID_SIZE: usize = 32;
const OUTPOINT_INDEX_SIZE: usize = 4;
const INPUT_NO_SIG_SCRIPT_SIZE: usize = OUTPOINT_TXID_SIZE + OUTPOINT_INDEX_SIZE + U64_SIZE + U64_SIZE;
const OUTPUT_VALUE_SIZE: usize = 8;
const SPK_VERSION_SIZE: usize = 2;
const SPK_SCRIPT_LEN_SIZE: usize = U64_SIZE;

const PARENT_OUTPOINT_TXID_START: usize = TX_VERSION_SIZE + U64_SIZE;
const PARENT_OUTPOINT_TXID_END: usize = PARENT_OUTPOINT_TXID_START + OUTPOINT_TXID_SIZE;
const PARENT_OUTPOINT_INDEX_START: usize = PARENT_OUTPOINT_TXID_END;
const PARENT_OUTPOINT_INDEX_END: usize = PARENT_OUTPOINT_INDEX_START + OUTPOINT_INDEX_SIZE;

const OP_MINT: u8 = 0;
const OP_TOKEN_TRANSFER: u8 = 2;

const ENCODED_GENESIS_SEQ: u8 = 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAssetOp {
    Mint = OP_MINT,
    TokenTransfer = OP_TOKEN_TRANSFER,
}

impl NativeAssetOp {
    fn from_byte(value: u8) -> Option<Self> {
        match value {
            OP_MINT => Some(Self::Mint),
            OP_TOKEN_TRANSFER => Some(Self::TokenTransfer),
            _ => None,
        }
    }
}

/// KNAT20 payload encoded as fixed offsets with small script-number fields.
/// Numeric fields are restricted to 1..=127 to keep minimal encoding in 1 byte.
/// `remaining_supply` and `seq` are encoded as value + 1 to avoid zero.
/// `asset_id` is expected to be blake2b(outpoint_txid || outpoint_index_le)
/// of the mint's input 0 (the genesis UTXO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAssetPayload {
    pub asset_id: [u8; 32],
    pub authority_hash: [u8; 32],
    pub remaining_supply: u8,
    pub seq: u8,
    pub op: NativeAssetOp,
    pub amount: u8,
    pub recipient_hash: [u8; 32],
}

impl NativeAssetPayload {
    pub fn mint_next(&self, amount: u8, recipient_hash: [u8; 32]) -> Self {
        let remaining = self.remaining_supply.checked_sub(amount).expect("amount exceeds remaining supply");
        let seq = self.seq.checked_add(1).expect("seq overflow");
        Self {
            asset_id: self.asset_id,
            authority_hash: self.authority_hash,
            remaining_supply: remaining,
            seq,
            op: NativeAssetOp::Mint,
            amount,
            recipient_hash,
        }
    }

    pub fn token_transfer_next(&self, new_recipient_hash: [u8; 32]) -> Self {
        let seq = self.seq.checked_add(1).expect("seq overflow");
        Self {
            asset_id: self.asset_id,
            authority_hash: self.authority_hash,
            remaining_supply: self.remaining_supply,
            seq,
            op: NativeAssetOp::TokenTransfer,
            amount: self.amount,
            recipient_hash: new_recipient_hash,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(PAYLOAD_LEN);
        payload.extend_from_slice(PAYLOAD_MAGIC);
        payload.push(PAYLOAD_VERSION);
        payload.extend_from_slice(&self.asset_id);
        payload.extend_from_slice(&self.authority_hash);
        payload.push(encode_plus_one(self.remaining_supply, "remaining_supply"));
        payload.push(encode_plus_one(self.seq, "seq"));
        payload.push(self.op as u8);
        payload.push(encode_small_non_zero(self.amount, "amount"));
        payload.extend_from_slice(&self.recipient_hash);
        payload
    }

    pub fn decode(payload: &[u8]) -> Option<Self> {
        if payload.len() != PAYLOAD_LEN {
            return None;
        }
        if &payload[OFF_MAGIC_START..OFF_MAGIC_END] != PAYLOAD_MAGIC {
            return None;
        }
        if payload[OFF_VERSION_START] != PAYLOAD_VERSION {
            return None;
        }

        let asset_id = payload[OFF_ASSET_ID_START..OFF_ASSET_ID_END].try_into().ok()?;
        let authority_hash = payload[OFF_AUTHORITY_HASH_START..OFF_AUTHORITY_HASH_END].try_into().ok()?;
        let remaining_supply = decode_plus_one(payload[OFF_REMAINING_SUPPLY_START])?;
        let seq = decode_plus_one(payload[OFF_SEQ_START])?;
        let op = NativeAssetOp::from_byte(payload[OFF_OP_START])?;
        let amount = decode_small_non_zero(payload[OFF_AMOUNT_START])?;
        let recipient_hash = payload[OFF_RECIPIENT_HASH_START..OFF_RECIPIENT_HASH_END].try_into().ok()?;

        Some(Self { asset_id, authority_hash, remaining_supply, seq, op, amount, recipient_hash })
    }
}

/// Holds the current covenant UTXO state.
#[derive(Clone)]
pub struct NativeAssetState {
    backtrace: CatBacktrace,
    utxo_outpoint: TransactionOutpoint,
    utxo_entry: UtxoEntry,
    pub payload: NativeAssetPayload,
}

impl NativeAssetState {
    pub fn from_tx_with_entry(tx: Transaction, utxo_entry: UtxoEntry, backtrace: CatBacktrace) -> Self {
        let payload = NativeAssetPayload::decode(&tx.payload).expect("Invalid native asset payload");
        let outpoint = TransactionOutpoint::new(tx.id(), 0);
        Self { backtrace, utxo_outpoint: outpoint, utxo_entry, payload }
    }

    pub fn from_tx_with_entry_and_grandparent(tx: Transaction, utxo_entry: UtxoEntry, grandparent_tx: &Transaction) -> Self {
        let backtrace = CatBacktrace::from_parent_and_grandparent(&tx, grandparent_tx);
        Self::from_tx_with_entry(tx, utxo_entry, backtrace)
    }

    pub fn backtrace(&self) -> &CatBacktrace {
        &self.backtrace
    }

    fn build_sig_script(&self, covenant_script: &[u8]) -> Vec<u8> {
        build_sig_script_from_backtrace(&self.backtrace, covenant_script)
    }
}

/// Holds witness fragments for CAT parent/grandparent verification.
#[derive(Clone)]
pub struct CatBacktrace {
    pub gp_prefix: Vec<u8>,
    pub gp_out0_script: Vec<u8>,
    pub gp_suffix: Vec<u8>,
    pub gp_payload: Vec<u8>,
    pub parent_rest: Vec<u8>,
    pub parent_payload: Vec<u8>,
}

impl CatBacktrace {
    pub fn from_parent_and_grandparent(parent: &Transaction, grandparent: &Transaction) -> Self {
        let (parent_rest, parent_payload) = split_preimage_at_payload(parent);
        let (gp_prefix, gp_out0_script, gp_suffix, gp_payload) = split_grandparent_preimage(grandparent);
        Self { gp_prefix, gp_out0_script, gp_suffix, gp_payload, parent_rest, parent_payload }
    }
}

fn split_preimage_at_payload(tx: &Transaction) -> (Vec<u8>, Vec<u8>) {
    let preimage = transaction_id_preimage(tx);
    let payload_len = tx.payload.len();
    let split_at = preimage.len().checked_sub(payload_len).expect("payload larger than preimage");
    let (rest, payload) = preimage.split_at(split_at);
    (rest.to_vec(), payload.to_vec())
}

fn split_grandparent_preimage(tx: &Transaction) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (rest, payload) = split_preimage_at_payload(tx);
    let output0 = tx.outputs.get(0).expect("grandparent tx must have output 0");
    let out0_script = output0.script_public_key.script().to_vec();

    let prefix_len = TX_VERSION_SIZE
        + U64_SIZE
        + tx.inputs.len() * INPUT_NO_SIG_SCRIPT_SIZE
        + U64_SIZE
        + OUTPUT_VALUE_SIZE
        + SPK_VERSION_SIZE
        + SPK_SCRIPT_LEN_SIZE;
    let suffix_start = prefix_len + out0_script.len();
    assert!(suffix_start <= rest.len(), "grandparent preimage slice out of bounds");
    debug_assert_eq!(
        &rest[prefix_len - SPK_SCRIPT_LEN_SIZE..prefix_len],
        (out0_script.len() as u64).to_le_bytes().as_slice(),
        "grandparent output0 script length mismatch"
    );

    let prefix = rest[..prefix_len].to_vec();
    let suffix = rest[suffix_start..].to_vec();
    (prefix, out0_script, suffix, payload)
}

/// Build a signature script from CAT backtrace fragments.
pub fn build_sig_script_from_backtrace(backtrace: &CatBacktrace, covenant_script: &[u8]) -> Vec<u8> {
    ScriptBuilder::new()
        .add_data(&backtrace.gp_prefix)
        .unwrap()
        .add_data(&backtrace.gp_out0_script)
        .unwrap()
        .add_data(&backtrace.gp_suffix)
        .unwrap()
        .add_data(&backtrace.gp_payload)
        .unwrap()
        .add_data(&backtrace.parent_rest)
        .unwrap()
        .add_data(&backtrace.parent_payload)
        .unwrap()
        // For P2SH the redeem script must be the last stack item in the signature script.
        .add_data(covenant_script)
        .unwrap()
        .drain()
}

/// This is the key CAT patch: verify grandparent and use its output0 scriptPubKey to decide "genesis vs continuation".
///
/// Stack at entry (bottom -> top):
///   [gp_prefix, gp_out0_script, gp_suffix, gp_payload, parent_rest, parent_payload]
///
/// After this block returns, the stack ends with:
///   [gp_prefix, gp_out0_script, gp_suffix, gp_payload, parent_rest, parent_payload, parent_prev_txid, parent_prev_vout, is_continuation]
pub fn cat_verify_parent_and_grandparent(sb: &mut ScriptBuilder) -> Result<(), kaspa_txscript::script_builder::ScriptBuilderError> {
    // --- 1) Verify parent txid == current outpoint txid (your existing pattern) ---
    // Uses the top two stack items: parent_rest, parent_payload
    sb
        .add_op(Op2Dup)?              // ... parent_rest parent_payload parent_rest parent_payload
        .add_op(OpCat)?               // ... parent_rest parent_payload parent_preimage
        .add_data(b"TransactionID")?
        .add_op(OpBlake2bWithKey)?    // ... parent_rest parent_payload parent_txid
        .add_op(OpTxInputIndex)?
        .add_op(OpOutpointTxId)?      // ... parent_rest parent_payload parent_txid outpoint_txid
        .add_op(OpEqualVerify)?; // ... parent_rest parent_payload

    // --- 2) Extract parent input0 prevout (txid + index) ---
    // After step (1) the stack is still:
    //   [gp_prefix, gp_out0_script, gp_suffix, gp_payload, parent_rest, parent_payload]

    // Extract txid of input0 prevout: parent_prev_txid
    sb
        .add_i64(1)? .add_op(OpPick)?     // copy parent_rest
        .add_i64(PARENT_OUTPOINT_TXID_START as i64)?
        .add_i64(PARENT_OUTPOINT_TXID_END as i64)?
        .add_op(OpSubStr)?               // -> parent_prev_txid
        .add_op(OpDup)?; // keep a copy for asset_id binding

    // Extract index of input0 prevout: parent_prev_vout (4 bytes)
    sb
        .add_i64(3)? .add_op(OpPick)?     // copy parent_rest (depth shifted due to the txid + dup above)
        .add_i64(PARENT_OUTPOINT_INDEX_START as i64)?
        .add_i64(PARENT_OUTPOINT_INDEX_END as i64)?
        .add_op(OpSubStr)?               // -> parent_prev_vout (bytes)
        .add_op(OpDup)?; // keep a copy after verifying it is 0

    // Enforce parent spends output index 0 of grandparent (so gp_out0_script is well-defined)
    // (If you want arbitrary indices, you must also prove the correct output script for that index.)
    push_4bytes_le_zero(sb)?;
    sb.add_op(OpEqualVerify)?; // consumes one copy of vout + zeros, leaves one vout on stack

    // Current stack suffix is now:
    //   [..., parent_prev_txid(copy_for_asset), parent_prev_txid(copy_for_compare), parent_prev_vout]

    // Reorder so the compare-txid is on top for OpEqualVerify later
    sb.add_op(OpSwap)?; // [..., parent_prev_txid(asset), parent_prev_vout, parent_prev_txid(compare)]

    // --- 3) Compute grandparent txid from witness pieces and verify it matches parent_prev_txid(compare) ---
    //
    // At this point, stack is:
    // [gp_prefix, gp_out0_script, gp_suffix, gp_payload, parent_rest, parent_payload, parent_prev_txid(asset), parent_prev_vout, parent_prev_txid(compare)]

    // Build gp_preimage = gp_prefix || gp_out0_script || gp_suffix || gp_payload
    // Indexes from top (0=top):
    // 0: parent_prev_txid(compare)
    // 1: parent_prev_vout
    // 2: parent_prev_txid(asset)
    // 3: parent_payload
    // 4: parent_rest
    // 5: gp_payload
    // 6: gp_suffix
    // 7: gp_out0_script
    // 8: gp_prefix
    sb
        .add_i64(8)? .add_op(OpPick)?     // gp_prefix
        .add_i64(8)? .add_op(OpPick)?     // gp_out0_script
        .add_i64(8)? .add_op(OpPick)?     // gp_suffix
        .add_i64(8)? .add_op(OpPick)?     // gp_payload
        .add_op(OpCat)?                  // gp_suffix||gp_payload
        .add_op(OpCat)?                  // gp_out0_script||gp_suffix||gp_payload
        .add_op(OpCat)?                  // gp_prefix||gp_out0_script||gp_suffix||gp_payload
        .add_data(b"TransactionID")?
        .add_op(OpBlake2bWithKey)?       // gp_txid
        .add_op(OpEqualVerify)?; // gp_txid == parent_prev_txid(compare), consumes both

    // After this, stack ends with:
    //   [..., parent_prev_txid(asset), parent_prev_vout]

    // --- 4) CAT gate: is gp_out0_script == current covenant script bytes? ---
    // If yes => continuation (parent spent an older covenant UTXO).
    // If no  => genesis-style transition (parent spent a non-covenant UTXO).
    //
    // We copy gp_out0_script and compare it to the current input script bytes (version stripped).
    // gp_out0_script is now at depth 6 from top:
    // stack is [gp_prefix, gp_out0_script, gp_suffix, gp_payload, parent_rest, parent_payload, parent_prev_txid(asset), parent_prev_vout]
    // indices: vout=0, txid(asset)=1, parent_payload=2, parent_rest=3, gp_payload=4, gp_suffix=5, gp_out0_script=6, gp_prefix=7
    sb
        .add_i64(6)? .add_op(OpPick)?     // gp_out0_script copy
        .add_op(OpTxInputIndex)?
        .add_op(OpTxInputSpk)?
        .add_op(OpTxInputIndex)?
        .add_op(OpTxInputSpkLen)?
        .add_i64(2)?
        .add_op(OpSwap)?
        .add_op(OpSubStr)?
        .add_op(OpEqual)?; // bool = (gp_out0_script == self_spk_script)

    // Now the caller can OP_IF on that bool and enforce:
    // - continuation branch: drop parent_prev_txid(asset) and parent_prev_vout
    // - genesis branch: (a) optionally require prev_seq==0, and (b) bind/verify asset_id == blake2b(txid||vout)
    Ok(())
}

/// Patch your minter covenant:
/// - Uses CAT grandparent gating instead of "prev_seq==0" gating.
/// - In mint path, output1 must be a token covenant scriptPubKey (hash provided as constant).
pub fn build_minter_covenant_script_knat20(
    token_covenant_spk_hash: [u8; 32],
    authority_spk: &[u8],
) -> Result<Vec<u8>, kaspa_txscript::script_builder::ScriptBuilderError> {
    let mut sb = ScriptBuilder::new();

    // (A) Verify parent + grandparent, leaving a bool on top: (gp_out0_script == self_spk_script)
    cat_verify_parent_and_grandparent(&mut sb)?;

    // Stack now ends with: [... parent_prev_txid(asset), parent_prev_vout, is_continuation_bool]
    sb.add_op(OpIf)?; // continuation?

    // --- Continuation branch (parent spent previous minter covenant output) ---
    // Drop parent_prev_vout + parent_prev_txid(asset); we don't need genesis binding.
    sb.add_op(OpDrop)?; // drop parent_prev_vout
    sb.add_op(OpDrop)?; // drop parent_prev_txid(asset)

    sb.add_op(OpElse)?; // --- Genesis branch ---

    // Optional hardening: require prev_seq == 0 (encoded as 1) on the very first spend.
    // parent_payload is at depth 2 from top right now (txid/vout are still present),
    // but we'll just use OpPick to copy it.
    sb.add_i64(2)?
        .add_op(OpPick)?
        .add_i64(OFF_SEQ_START as i64)?
        .add_i64(OFF_SEQ_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(ENCODED_GENESIS_SEQ as i64)?
        .add_op(OpEqualVerify)?;

    // Bind asset_id to the *genesis outpoint* (parent input0 prevout), per CAT tokenId logic.
    // Compute blake2b( parent_prev_txid(asset) || parent_prev_vout )
    sb.add_op(OpCat)?; // consumes (txid, vout) -> txid||vout
    sb.add_op(OpBlake2b)?; // -> computed_asset_id

    // Verify computed_asset_id == prev_payload.asset_id
    sb
        .add_i64(1)? .add_op(OpPick)? // parent_payload
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpSubStr)?
        .add_op(OpEqualVerify)?;

    sb.add_op(OpEndIf)?; // end CAT gate

    // (B) From here on, you can keep MOST of your existing covenant:
    //     - payload len + magic + version checks
    //     - asset_id immutability: prev_payload.asset_id == cur_payload.asset_id
    //     - seq++ enforcement on the minter state
    //     - authority check via input[1] spk hash
    //     - remaining_supply update
    //
    // The only KNAT20-style change below is in the Mint path: output1 is token covenant, not a recipient address.

    // ---- Basic payload checks (same idea as your existing code) ----
    sb.add_op(OpTxPayloadLen)?;
    sb.add_i64(PAYLOAD_LEN as i64)?;
    sb.add_op(OpEqualVerify)?;

    // Magic / version
    sb.add_i64(OFF_MAGIC_START as i64)?.add_i64(OFF_MAGIC_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_data(PAYLOAD_MAGIC)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_VERSION_START as i64)?.add_i64(OFF_VERSION_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_i64(PAYLOAD_VERSION as i64)?;
    sb.add_op(OpEqualVerify)?;

    // Enforce OP_MINT for minter spends.
    sb.add_i64(OFF_OP_START as i64)?.add_i64(OFF_OP_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_op(OpData1)?;
    sb.add_op(OP_MINT)?;
    sb.add_op(OpEqualVerify)?;

    // asset_id immutability: prev_payload.asset_id == cur_payload.asset_id
    // (prev_payload is `parent_payload` in this scheme; caller must ensure it is still on stack)
    sb.add_op(OpDup)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;

    // Require at least 2 inputs (so input[1] can authorize)
    sb.add_op(OpTxInputCount)?;
    sb.add_i64(2)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // Authority must be provided as input[1] with the exact ScriptPublicKey bytes.
    sb.add_i64(1)?;
    sb.add_op(OpTxInputSpk)?;
    sb.add_data(authority_spk)?;
    sb.add_op(OpEqualVerify)?;

    // Output 0 keeps minter covenant script (your existing check: input_spk == output0_spk)
    sb.add_op(OpTxInputIndex)?.add_op(OpTxInputSpk)?.add_i64(0)?.add_op(OpTxOutputSpk)?.add_op(OpEqualVerify)?;

    // Output 1 MUST be a token covenant output (KNAT20-style token UTXO locked by token covenant).
    // Compare hash(output1_script) == token_covenant_spk_hash
    sb.add_i64(1)?
        .add_op(OpTxOutputSpk)?
        .add_i64(1)?
        .add_op(OpTxOutputSpkLen)?
        .add_i64(2)?
        .add_op(OpSwap)?
        .add_op(OpSubStr)?
        .add_op(OpBlake2b)?
        .add_data(&token_covenant_spk_hash)?
        .add_op(OpEqualVerify)?;

    // You can still use OFF_AMOUNT and OFF_RECIPIENT_HASH in payload to describe the minted token state:
    // - amount = token amount minted in the token output
    // - recipient_hash = owner/recipient identity for the token (to be enforced by token covenant on spend)

    // Finally ensure outputcount >= 2 (minter covenant output + token output)
    sb.add_op(OpTxOutputCount)?.add_i64(2)?.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;
    sb.add_op(OpDrop)?; // drop parent_payload
    sb.add_op(OpDrop)?; // drop parent_rest
    sb.add_op(OpDrop)?; // drop gp_payload
    sb.add_op(OpDrop)?; // drop gp_suffix
    sb.add_op(OpDrop)?; // drop gp_out0_script
    sb.add_op(OpDrop)?; // drop gp_prefix
    sb.add_op(OpTrue)?;

    Ok(sb.drain())
}

/// A minimal token covenant skeleton showing CAT parent+grandparent proof.
/// It enforces that a token UTXO is spendable only if its parent spent either:
///   - another token UTXO of the SAME scriptPubKey (transfer case), or
///   - a minter covenant UTXO of a known scriptPubKey hash (mint-origin case).
///
/// This is the CAT "grandparent script decides the branch" idea.
pub fn build_token_covenant_script_knat20(
    minter_covenant_spk_hash: [u8; 32],
) -> Result<Vec<u8>, kaspa_txscript::script_builder::ScriptBuilderError> {
    let mut sb = ScriptBuilder::new();

    // Same inductive verification block; leaves is_continuation_bool on top.
    cat_verify_parent_and_grandparent(&mut sb)?;

    // Now decide:
    // - If continuation: parent spent a token UTXO (gp_out0_script == self_spk_script).
    // - Else: require gp_out0_script hashes to minter covenant script hash.

    sb.add_op(OpIf)?; // continuation (transfer)

    // Drop the parent_prev_txid(asset) and parent_prev_vout (we don't need them for transfer)
    sb.add_op(OpDrop)?;
    sb.add_op(OpDrop)?;

    sb.add_op(OpElse)?; // mint-origin (first spend after mint)

    // Here, gp_out0_script != self_spk_script, so enforce gp_out0_script is a MINTER covenant output.
    // We have gp_out0_script available in the witness (and still on stack under other items).
    // Hash it and compare to minter_covenant_spk_hash.
    //
    // Depth calculation will depend on what else you leave on stack; easiest is to OP_PICK it right after
    // cat_verify_parent_and_grandparent, similarly to the minter script.
    sb
        .add_i64(6)? // gp_out0_script
        .add_op(OpPick)?
        .add_op(OpBlake2b)?
        .add_data(&minter_covenant_spk_hash)?
        .add_op(OpEqualVerify)?;

    // If this passes, the token was minted by spending a minter UTXO, as required by KNAT20.
    sb.add_op(OpDrop)?; // drop parent_prev_vout
    sb.add_op(OpDrop)?; // drop parent_prev_txid(asset)
    sb.add_op(OpEndIf)?;

    // ---- Basic payload checks ----
    sb.add_op(OpTxPayloadLen)?;
    sb.add_i64(PAYLOAD_LEN as i64)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_MAGIC_START as i64)?.add_i64(OFF_MAGIC_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_data(PAYLOAD_MAGIC)?;
    sb.add_op(OpEqualVerify)?;

    sb.add_i64(OFF_VERSION_START as i64)?.add_i64(OFF_VERSION_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_i64(PAYLOAD_VERSION as i64)?;
    sb.add_op(OpEqualVerify)?;

    // Require OP_TOKEN_TRANSFER for token spends.
    sb.add_i64(OFF_OP_START as i64)?.add_i64(OFF_OP_END as i64)?.add_op(OpTxPayloadSubstr)?;
    sb.add_i64(OP_TOKEN_TRANSFER as i64)?;
    sb.add_op(OpEqualVerify)?;

    // TokenId preservation: parent_payload.asset_id == cur_payload.asset_id
    sb.add_op(OpDup)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_ASSET_ID_START as i64)?
        .add_i64(OFF_ASSET_ID_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;

    // Amount preservation: parent_payload.amount == cur_payload.amount
    sb.add_op(OpDup)?
        .add_i64(OFF_AMOUNT_START as i64)?
        .add_i64(OFF_AMOUNT_END as i64)?
        .add_op(OpSubStr)?
        .add_i64(OFF_AMOUNT_START as i64)?
        .add_i64(OFF_AMOUNT_END as i64)?
        .add_op(OpTxPayloadSubstr)?
        .add_op(OpEqualVerify)?;

    // Require at least 2 inputs (input[1] proves ownership).
    sb.add_op(OpTxInputCount)?;
    sb.add_i64(2)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;

    // Owner authorization: hash(input[1].spk.script) == parent_payload.recipient_hash
    sb.add_i64(1)?
        .add_op(OpTxInputSpk)?
        .add_i64(1)?
        .add_op(OpTxInputSpkLen)?
        .add_i64(2)?
        .add_op(OpSwap)?
        .add_op(OpSubStr)?
        .add_op(OpBlake2b)?
        .add_i64(1)?
        .add_op(OpPick)?
        .add_i64(OFF_RECIPIENT_HASH_START as i64)?
        .add_i64(OFF_RECIPIENT_HASH_END as i64)?
        .add_op(OpSubStr)?
        .add_op(OpEqualVerify)?;

    // Token script propagation: enforce output at SAME index as this input has same spk as this input spk
    // (so token UTXO stays locked by this covenant even if it's at output[1], output[7], etc.)
    sb
        .add_op(OpTxInputIndex)?
        .add_op(OpTxInputSpk)?
        .add_op(OpTxInputIndex)? // push index again
        .add_op(OpTxOutputSpk)?
        .add_op(OpEqualVerify)?;

    // Require at least 1 output
    sb.add_op(OpTxOutputCount)?;
    sb.add_i64(1)?;
    sb.add_op(OpGreaterThanOrEqual)?;
    sb.add_op(OpVerify)?;
    sb.add_op(OpDrop)?; // drop parent_payload
    sb.add_op(OpDrop)?; // drop parent_rest
    sb.add_op(OpDrop)?; // drop gp_payload
    sb.add_op(OpDrop)?; // drop gp_suffix
    sb.add_op(OpDrop)?; // drop gp_out0_script
    sb.add_op(OpDrop)?; // drop gp_prefix
    sb.add_op(OpTrue)?;

    Ok(sb.drain())
}

/// Build a mint transaction with an authorization input at index 1.
pub fn build_mint_tx(
    state: &NativeAssetState,
    next_payload: &NativeAssetPayload,
    minter_spk: &kaspa_consensus_core::tx::ScriptPublicKey,
    token_spk: &kaspa_consensus_core::tx::ScriptPublicKey,
    token_value: u64,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    covenant_script: &[u8],
) -> Transaction {
    let payload = next_payload.encode();
    let sig_script = state.build_sig_script(covenant_script);

    let covenant_input = TransactionInput::new(state.utxo_outpoint, sig_script, 0, 0);

    let temp_covenant_value = state.utxo_entry.amount.saturating_sub(token_value);
    let temp_outputs =
        vec![TransactionOutput::new(temp_covenant_value, minter_spk.clone()), TransactionOutput::new(token_value, token_spk.clone())];
    let temp_tx = Transaction::new(
        0,
        vec![covenant_input.clone(), auth_input.clone()],
        temp_outputs,
        0,
        SubnetworkId::default(),
        0,
        payload.clone(),
    );
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![state.utxo_entry.clone(), auth_entry.clone()]);

    let calculator = MassCalculator::new_with_consensus_params(&NetworkType::Devnet.into());
    let storage_mass = calculator.calc_contextual_masses(&temp_tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(temp_tx.tx);
    let mass = storage_mass.max(compute_mass).max(transient_mass);

    let covenant_value = state.utxo_entry.amount.saturating_sub(token_value).saturating_sub(mass);
    let outputs =
        vec![TransactionOutput::new(covenant_value, minter_spk.clone()), TransactionOutput::new(token_value, token_spk.clone())];

    let mut tx = Transaction::new(0, vec![covenant_input, auth_input], outputs, 0, SubnetworkId::default(), 0, payload);
    tx.finalize();
    tx
}

/// Build a token transfer transaction with an authorization input at index 1.
pub fn build_token_transfer_tx(
    state: &NativeAssetState,
    next_payload: &NativeAssetPayload,
    token_spk: &kaspa_consensus_core::tx::ScriptPublicKey,
    auth_input: TransactionInput,
    auth_entry: UtxoEntry,
    covenant_script: &[u8],
) -> Transaction {
    let payload = next_payload.encode();
    let sig_script = state.build_sig_script(covenant_script);

    let covenant_input = TransactionInput::new(state.utxo_outpoint, sig_script, 0, 0);

    let temp_output = TransactionOutput::new(state.utxo_entry.amount, token_spk.clone());
    let temp_tx = Transaction::new(
        0,
        vec![covenant_input.clone(), auth_input.clone()],
        vec![temp_output],
        0,
        SubnetworkId::default(),
        0,
        payload.clone(),
    );
    let temp_tx = PopulatedTransaction::new(&temp_tx, vec![state.utxo_entry.clone(), auth_entry.clone()]);

    let calculator = MassCalculator::new_with_consensus_params(&NetworkType::Devnet.into());
    let storage_mass = calculator.calc_contextual_masses(&temp_tx).map(|mass| mass.storage_mass).unwrap_or_default();
    let NonContextualMasses { compute_mass, transient_mass } = calculator.calc_non_contextual_masses(temp_tx.tx);
    let mass = storage_mass.max(compute_mass).max(transient_mass);

    let token_value = state.utxo_entry.amount.saturating_sub(mass);
    let output = TransactionOutput::new(token_value, token_spk.clone());

    let mut tx = Transaction::new(0, vec![covenant_input, auth_input], vec![output], 0, SubnetworkId::default(), 0, payload);
    tx.finalize();
    tx
}

fn encode_small_non_zero(value: u8, field: &str) -> u8 {
    assert!((1..=MAX_SMALL_INT).contains(&value), "{field} must be 1..=127");
    value
}

fn encode_plus_one(value: u8, field: &str) -> u8 {
    let encoded = value.checked_add(1).expect("value overflow");
    assert!((1..=MAX_SMALL_INT).contains(&encoded), "{field} must fit 0..=126");
    encoded
}

fn decode_small_non_zero(value: u8) -> Option<u8> {
    if (1..=MAX_SMALL_INT).contains(&value) {
        Some(value)
    } else {
        None
    }
}

fn decode_plus_one(value: u8) -> Option<u8> {
    if (1..=MAX_SMALL_INT).contains(&value) {
        Some(value - 1)
    } else {
        None
    }
}

fn push_4bytes_le_zero(sb: &mut ScriptBuilder) -> Result<(), kaspa_txscript::script_builder::ScriptBuilderError> {
    sb.add_data(&[0u8, 0u8, 0u8, 0u8])?;
    Ok(())
}
