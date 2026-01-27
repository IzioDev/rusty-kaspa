use kaspa_txscript::script_builder::ScriptBuilderError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CovenantError {
    #[error("invalid field length for {0}")]
    InvalidField(&'static str),
    #[error("script number for {field} exceeds i64 range: {value}")]
    ScriptNumOverflow { field: &'static str, value: u64 },
    #[error("spk length out of range for {field}: expected {min}-{max}, got {actual}")]
    SpkBytesLengthOutOfRange { field: &'static str, min: usize, max: usize, actual: usize },
    #[error("amount {amount} exceeds remaining supply {remaining}")]
    AmountExceedsRemainingSupply { remaining: u64, amount: u64 },
    #[error("insufficient funds: available {available}, required {required}")]
    InsufficientFunds { available: u64, required: u64 },
    #[error(transparent)]
    ScriptBuilder(#[from] ScriptBuilderError),
}
