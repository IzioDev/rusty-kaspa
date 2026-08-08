//!
//!  Address type (`Receive` or `Change`) used in HD wallet address derivation.
//!

use crate::Error;
use std::fmt;

/// Address type used in HD wallet address derivation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressType {
    Receive = 0,
    Change,
}

impl fmt::Display for AddressType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Receive => write!(f, "Receive"),
            Self::Change => write!(f, "Change"),
        }
    }
}

impl AddressType {
    pub fn index(&self) -> u32 {
        match self {
            Self::Receive => 0,
            Self::Change => 1,
        }
    }
}

impl From<AddressType> for u32 {
    fn from(address_type: AddressType) -> Self {
        match address_type {
            AddressType::Receive => 0,
            AddressType::Change => 1,
        }
    }
}

impl TryFrom<u32> for AddressType {
    type Error = Error;

    fn try_from(index: u32) -> Result<Self, Self::Error> {
        match index {
            0 => Ok(Self::Receive),
            1 => Ok(Self::Change),
            _ => Err(Error::AddressType(index)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_type_round_trip_index() {
        let receive: u32 = AddressType::Receive.into();
        let change: u32 = AddressType::Change.into();

        assert_eq!(receive, 0);
        assert_eq!(change, 1);
        assert_eq!(AddressType::try_from(receive).unwrap(), AddressType::Receive);
        assert_eq!(AddressType::try_from(change).unwrap(), AddressType::Change);
    }

    #[test]
    fn address_type_rejects_unknown_index() {
        assert!(AddressType::try_from(2).is_err());
    }
}
