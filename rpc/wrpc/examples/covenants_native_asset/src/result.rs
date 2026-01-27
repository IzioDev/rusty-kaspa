use crate::errors::CovenantError;

pub type CovenantResult<T> = Result<T, CovenantError>;
