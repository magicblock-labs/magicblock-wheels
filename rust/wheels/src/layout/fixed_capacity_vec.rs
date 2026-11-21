use core::marker::PhantomData;

use bytemuck::Pod;
use pinocchio::error::ProgramError;

use super::{
    Encodable, FixedLayoutElement, FixedSizeLayout, FixedValueElement, MAX_SUPPORTED_VEC_LEN,
};
use crate::DataLayoutError;

/// Mutable storage-backed view over a fixed-capacity `Vec<T>` slot.
///
/// This type is used by generated `fixed_offset_layout` mutable views for
/// `#[capacity = N] Vec<T>` fields. It can grow and shrink the active logical
/// length within the schema-level capacity, but it never resizes the backing
/// storage.
///
/// CHECKPOINT(CU): generated fixed-layout getters pass compile-time constant
/// offsets and element metadata into this generic helper. They are stored as
/// runtime fields to keep return types simple. If CU measurements show this
/// matters for very small budgets, benchmark const-generic offsets/metadata or
/// field-specific generated wrappers.
pub struct FixedCapacityVec<'a, T, K = FixedValueElement> {
    bytes: &'a mut [u8],
    offset: usize,
    len_width: usize,
    elem_size: usize,
    len: usize,
    capacity: usize,
    _marker: PhantomData<(T, K)>,
}

impl<'a, T, K> FixedCapacityVec<'a, T, K> {
    #[doc(hidden)]
    pub fn new(
        bytes: &'a mut [u8],
        offset: usize,
        len_width: usize,
        elem_size: usize,
        capacity: usize,
    ) -> Self {
        let len = read_len_header(bytes, offset, len_width).expect("validated Vec length");
        Self {
            bytes,
            offset,
            len_width,
            elem_size,
            len,
            capacity,
            _marker: PhantomData,
        }
    }

    /// Returns the active logical element count.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns the schema-level capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns whether the active logical element count is zero.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the active encoded length of this Vec field.
    pub fn encoded_len(&self) -> usize {
        self.len_width + self.len * self.elem_size
    }

    /// Returns the reserved storage length of this Vec field.
    pub fn storage_len(&self) -> usize {
        self.len_width + self.capacity * self.elem_size
    }

    fn data_start(&self) -> usize {
        self.offset + self.len_width
    }
}

impl<'a, T> FixedCapacityVec<'a, T, FixedValueElement>
where
    T: Pod,
{
    /// Returns the active element at `index`.
    pub fn get(&self, index: usize) -> Option<T> {
        if index >= self.len {
            return None;
        }
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        Some(bytemuck::pod_read_unaligned(&self.bytes[start..end]))
    }

    /// Appends one element without resizing the backing storage.
    pub fn push(&mut self, value: T) -> Result<(), ProgramError> {
        let next_len = self
            .len
            .checked_add(1)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        if next_len > self.capacity {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        let index = self.len;
        self.write_bytes_at(index, bytemuck::bytes_of(&value));
        self.write_len(next_len)?;
        self.len = next_len;
        Ok(())
    }

    /// Removes and returns the last active element.
    pub fn pop(&mut self) -> Result<Option<T>, ProgramError> {
        if self.len == 0 {
            return Ok(None);
        }

        let index = self.len - 1;
        let value = self.get(index).expect("validated Vec length");
        self.write_len(index)?;
        self.len = index;
        Ok(Some(value))
    }

    /// Replaces the active element at `index`.
    pub fn set(&mut self, index: usize, value: T) -> Result<(), ProgramError> {
        if index >= self.len {
            return Err(DataLayoutError::InvalidDataLength.into());
        }

        self.write_bytes_at(index, bytemuck::bytes_of(&value));
        Ok(())
    }
}

impl<'a, T> FixedCapacityVec<'a, T, FixedLayoutElement>
where
    T: Encodable + FixedSizeLayout,
{
    /// Appends one fixed-size layout element without resizing backing storage.
    pub fn push(&mut self, value: &T) -> Result<(), ProgramError> {
        let next_len = self
            .len
            .checked_add(1)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        if next_len > self.capacity {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        let index = self.len;
        self.write_encoded_at(index, value)?;
        self.write_len(next_len)?;
        self.len = next_len;
        Ok(())
    }

    /// Removes the last active fixed-size layout element.
    pub fn pop(&mut self) -> Result<(), ProgramError> {
        if self.len == 0 {
            return Ok(());
        }

        let next_len = self.len - 1;
        self.write_len(next_len)?;
        self.len = next_len;
        Ok(())
    }

    /// Replaces the active element at `index`.
    pub fn set(&mut self, index: usize, value: &T) -> Result<(), ProgramError> {
        if index >= self.len {
            return Err(DataLayoutError::InvalidDataLength.into());
        }

        self.write_encoded_at(index, value)
    }
}

impl<'a, T, K> FixedCapacityVec<'a, T, K> {
    /// Shrinks the active logical length.
    pub fn truncate(&mut self, len: usize) -> Result<(), ProgramError> {
        if len > self.len {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }
        if len == self.len {
            return Ok(());
        }

        self.write_len(len)?;
        self.len = len;
        Ok(())
    }

    /// Sets the active logical length to zero.
    pub fn clear(&mut self) -> Result<(), ProgramError> {
        self.truncate(0)
    }

    fn write_len(&mut self, len: usize) -> Result<(), ProgramError> {
        if len > self.capacity || len > max_len(self.len_width) {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        write_len_header(self.bytes, self.offset, self.len_width, len)
    }

    fn write_bytes_at(&mut self, index: usize, value: &[u8]) {
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        self.bytes[start..end].copy_from_slice(value);
    }

    fn write_encoded_at(&mut self, index: usize, value: &T) -> Result<(), ProgramError>
    where
        T: Encodable,
    {
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        let remaining = value.encode_to(&mut self.bytes[start..end])?;
        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength.into());
        }
        Ok(())
    }
}

fn read_len_header(bytes: &[u8], offset: usize, len_width: usize) -> Result<usize, ProgramError> {
    let mut raw = [0u8; 8];
    raw[..len_width].copy_from_slice(&bytes[offset..offset + len_width]);
    let len = u64::from_le_bytes(raw);
    usize::try_from(len).map_err(|_| DataLayoutError::LengthExceedsCapacity.into())
}

fn write_len_header(
    bytes: &mut [u8],
    offset: usize,
    len_width: usize,
    len: usize,
) -> Result<(), ProgramError> {
    if len > max_len(len_width) {
        return Err(DataLayoutError::LengthExceedsCapacity.into());
    }

    let raw = (len as u64).to_le_bytes();
    bytes[offset..offset + len_width].copy_from_slice(&raw[..len_width]);
    Ok(())
}

fn max_len(len_width: usize) -> usize {
    match len_width {
        0 => 0,
        1..=7 => (1usize << (len_width * 8)) - 1,
        _ => MAX_SUPPORTED_VEC_LEN,
    }
}
