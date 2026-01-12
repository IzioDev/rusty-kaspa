use kaspa_txscript::scriptnum::{deserialize_i64, serialize_i64};
use kaspa_txscript_errors::TxScriptError;

use crate::{covenants::PAYLOAD_U64_SIZE, errors::CovenantError};

// writes <[...pad with 0], len_plus_one, scriptnum_bytes> resulting in 9 bytes
pub fn append_scriptnum_padded_u64(payload: &mut Vec<u8>, value: u64, field: &'static str) -> Result<(), CovenantError> {
    let bytes = serialize_u64(value).map_err(|err| match err {
        TxScriptError::NumberTooBig(_) => CovenantError::ScriptNumOverflow { field, value },
        _ => CovenantError::InvalidScriptNum(field),
    })?;
    if bytes.len() > PAYLOAD_U64_SIZE {
        return Err(CovenantError::InvalidScriptNum(field));
    }
    payload.push((bytes.len() + 1) as u8);
    payload.extend_from_slice(&bytes);
    // pad with 0
    payload.extend(std::iter::repeat(0).take(PAYLOAD_U64_SIZE - bytes.len()));
    Ok(())
}

pub fn decode_scriptnum_padded_u64(lenp1: u8, padded: &[u8], field: &'static str) -> Result<u64, CovenantError> {
    if lenp1 == 0 || lenp1 as usize > PAYLOAD_U64_SIZE + 1 {
        return Err(CovenantError::InvalidScriptNum(field));
    }
    let len = (lenp1 - 1) as usize;
    if padded.len() != PAYLOAD_U64_SIZE {
        return Err(CovenantError::InvalidScriptNum(field));
    }
    if padded[len..].iter().any(|&b| b != 0) {
        return Err(CovenantError::InvalidScriptNum(field));
    }
    deserialize_u64(&padded[..len]).map_err(|_| CovenantError::InvalidScriptNum(field))
}

/// serialize u64 into script num representation
///
/// restrictions: u64 <= i64::MAX
pub fn serialize_u64(value: u64) -> Result<Vec<u8>, TxScriptError> {
    let signed = i64::try_from(value)
        .map_err(|_| TxScriptError::NumberTooBig(format!("numeric value {value} exceeds 64-bit signed integer range")))?;
    Ok(serialize_i64(&signed))
}

/// deserialize script num into u64
pub fn deserialize_u64(v: &[u8]) -> Result<u64, TxScriptError> {
    let value = deserialize_i64(v)?;
    u64::try_from(value).map_err(|_| TxScriptError::NumberTooBig(format!("numeric value {value} is negative")))
}
