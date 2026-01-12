use crate::scriptnum::{deserialize_i64, serialize_i64};
use crate::TxScriptError;
use core::fmt::Debug;
use kaspa_txscript_errors::SerializationError;
use std::cmp::Ordering;
use std::num::TryFromIntError;
use std::ops::{Deref, Index};

#[derive(PartialEq, Eq, Debug, Default, PartialOrd, Ord)]
pub(crate) struct SizedEncodeInt<const LEN: usize>(pub(crate) i64);

impl<const LEN: usize> From<i64> for SizedEncodeInt<LEN> {
    fn from(value: i64) -> Self {
        SizedEncodeInt(value)
    }
}

impl<const LEN: usize> From<i32> for SizedEncodeInt<LEN> {
    fn from(value: i32) -> Self {
        SizedEncodeInt(value as i64)
    }
}

impl<const LEN: usize> TryFrom<SizedEncodeInt<LEN>> for i32 {
    type Error = TryFromIntError;

    fn try_from(value: SizedEncodeInt<LEN>) -> Result<Self, Self::Error> {
        value.0.try_into()
    }
}

impl<const LEN: usize> PartialEq<i64> for SizedEncodeInt<LEN> {
    fn eq(&self, other: &i64) -> bool {
        self.0 == *other
    }
}

impl<const LEN: usize> PartialOrd<i64> for SizedEncodeInt<LEN> {
    fn partial_cmp(&self, other: &i64) -> Option<Ordering> {
        self.0.partial_cmp(other)
    }
}

impl<const LEN: usize> Deref for SizedEncodeInt<LEN> {
    type Target = i64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const LEN: usize> From<SizedEncodeInt<LEN>> for i64 {
    fn from(value: SizedEncodeInt<LEN>) -> Self {
        value.0
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Stack {
    inner: Vec<Vec<u8>>,
    max_element_size: usize,
}

impl Deref for Stack {
    type Target = Vec<Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Index<usize> for Stack {
    type Output = Vec<u8>;

    fn index(&self, index: usize) -> &Self::Output {
        &self.inner[index]
    }
}

#[cfg(test)]
impl From<Vec<Vec<u8>>> for Stack {
    fn from(inner: Vec<Vec<u8>>) -> Self {
        // TODO: Replace with the correct max element size after the fork.
        Self { inner, max_element_size: usize::MAX }
    }
}

impl From<Stack> for Vec<Vec<u8>> {
    fn from(stack: Stack) -> Self {
        stack.inner
    }
}

pub(crate) trait OpcodeData<T> {
    fn deserialize(&self) -> Result<T, TxScriptError>;
    fn serialize(from: &T) -> Result<Self, SerializationError>
    where
        Self: Sized;
}

impl OpcodeData<i64> for Vec<u8> {
    #[inline]
    fn deserialize(&self) -> Result<i64, TxScriptError> {
        OpcodeData::<SizedEncodeInt<8>>::deserialize(self).map(i64::from)
    }

    #[inline]
    fn serialize(from: &i64) -> Result<Self, SerializationError> {
        OpcodeData::<SizedEncodeInt<8>>::serialize(&(*from).into())
    }
}

impl OpcodeData<i32> for Vec<u8> {
    #[inline]
    fn deserialize(&self) -> Result<i32, TxScriptError> {
        OpcodeData::<SizedEncodeInt<4>>::deserialize(self).map(|v| v.try_into().expect("number is within i32 range"))
    }

    #[inline]
    fn serialize(from: &i32) -> Result<Self, SerializationError> {
        OpcodeData::<SizedEncodeInt<4>>::serialize(&(*from).into())
    }
}

impl<const LEN: usize> OpcodeData<SizedEncodeInt<LEN>> for Vec<u8> {
    #[inline]
    fn deserialize(&self) -> Result<SizedEncodeInt<LEN>, TxScriptError> {
        match self.len() > LEN {
            true => Err(TxScriptError::NumberTooBig(format!(
                "numeric value encoded as {:x?} is {} bytes which exceeds the max allowed of {}",
                self,
                self.len(),
                LEN
            ))),
            false => deserialize_i64(self).map(SizedEncodeInt::<LEN>),
        }
    }

    #[inline]
    fn serialize(from: &SizedEncodeInt<LEN>) -> Result<Self, SerializationError> {
        let bytes = serialize_i64(&from.0);
        if bytes.len() > LEN {
            return Err(SerializationError::NumberTooLong(from.0));
        }
        Ok(bytes)
    }
}

impl OpcodeData<bool> for Vec<u8> {
    #[inline]
    fn deserialize(&self) -> Result<bool, TxScriptError> {
        if self.is_empty() {
            Ok(false)
        } else {
            // Negative 0 is also considered false
            Ok(self[self.len() - 1] & 0x7f != 0x0 || self[..self.len() - 1].iter().any(|&b| b != 0x0))
        }
    }

    #[inline]
    fn serialize(from: &bool) -> Result<Self, SerializationError> {
        Ok(match from {
            true => vec![1],
            false => vec![],
        })
    }
}

impl Stack {
    pub(crate) fn new(inner: Vec<Vec<u8>>, max_element_size: usize) -> Self {
        Self { inner, max_element_size }
    }

    #[inline]
    pub fn insert(&mut self, index: usize, element: Vec<u8>) -> Result<(), TxScriptError> {
        if element.len() > self.max_element_size {
            return Err(TxScriptError::ElementTooBig(element.len(), self.max_element_size));
        }
        self.inner.insert(index, element);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &[Vec<u8>] {
        &self.inner
    }

    #[inline]
    pub fn pop_items<const SIZE: usize, T: Debug>(&mut self) -> Result<[T; SIZE], TxScriptError>
    where
        Vec<u8>: OpcodeData<T>,
    {
        if self.len() < SIZE {
            return Err(TxScriptError::InvalidStackOperation(SIZE, self.len()));
        }
        Ok(<[T; SIZE]>::try_from(
            self.inner.split_off(self.len() - SIZE).iter().map(|v| v.deserialize()).collect::<Result<Vec<T>, _>>()?,
        )
        .expect("Already exact item"))
    }

    #[inline]
    pub fn pop_raw<const SIZE: usize>(&mut self) -> Result<[Vec<u8>; SIZE], TxScriptError> {
        if self.len() < SIZE {
            return Err(TxScriptError::InvalidStackOperation(SIZE, self.len()));
        }
        Ok(<[Vec<u8>; SIZE]>::try_from(self.inner.split_off(self.len() - SIZE)).expect("Already exact item"))
    }

    #[inline]
    pub fn peek_raw<const SIZE: usize>(&self) -> Result<[Vec<u8>; SIZE], TxScriptError> {
        if self.len() < SIZE {
            return Err(TxScriptError::InvalidStackOperation(SIZE, self.len()));
        }
        Ok(<[Vec<u8>; SIZE]>::try_from(self.inner[self.len() - SIZE..].to_vec()).expect("Already exact item"))
    }

    #[inline]
    pub fn push_item<T: Debug>(&mut self, item: T) -> Result<(), TxScriptError>
    where
        Vec<u8>: OpcodeData<T>,
    {
        let v = OpcodeData::serialize(&item)?;
        Vec::push(&mut self.inner, v);
        Ok(())
    }

    #[inline]
    pub fn drop_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= SIZE {
            true => {
                self.inner.truncate(self.len() - SIZE);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(SIZE, self.len())),
        }
    }

    #[inline]
    pub fn dup_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= SIZE {
            true => {
                self.inner.extend_from_within(self.len() - SIZE..);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(SIZE, self.len())),
        }
    }

    #[inline]
    pub fn over_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= 2 * SIZE {
            true => {
                self.inner.extend_from_within(self.len() - 2 * SIZE..self.len() - SIZE);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(2 * SIZE, self.len())),
        }
    }

    #[inline]
    pub fn rot_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= 3 * SIZE {
            true => {
                let drained = self.inner.drain(self.len() - 3 * SIZE..self.len() - 2 * SIZE).collect::<Vec<Vec<u8>>>();
                self.inner.extend(drained);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(3 * SIZE, self.len())),
        }
    }

    #[inline]
    pub fn swap_items<const SIZE: usize>(&mut self) -> Result<(), TxScriptError> {
        match self.len() >= 2 * SIZE {
            true => {
                let drained = self.inner.drain(self.len() - 2 * SIZE..self.len() - SIZE).collect::<Vec<Vec<u8>>>();
                self.inner.extend(drained);
                Ok(())
            }
            false => Err(TxScriptError::InvalidStackOperation(2 * SIZE, self.len())),
        }
    }

    pub fn clear(&mut self) {
        self.inner.clear()
    }

    pub fn pop(&mut self) -> Result<Vec<u8>, TxScriptError> {
        self.inner.pop().ok_or(TxScriptError::EmptyStack)
    }

    pub fn split_off(&mut self, at: usize) -> Vec<Vec<u8>> {
        self.inner.split_off(at)
    }

    pub fn push(&mut self, item: Vec<u8>) -> Result<(), TxScriptError> {
        if item.len() > self.max_element_size {
            return Err(TxScriptError::ElementTooBig(item.len(), self.max_element_size));
        }
        self.inner.push(item);
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Vec<u8> {
        self.inner.remove(index)
    }
}
