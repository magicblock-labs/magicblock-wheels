use core::marker::PhantomData;

use bytemuck::Pod;
use pinocchio::error::ProgramError;

use super::{Encodable, FixedLayoutElement, FixedSizeLayout, FixedValueElement, LayoutStorageMut};
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
pub struct FixedCapacityVec<'a, T, S: ?Sized, K = FixedValueElement> {
    storage: &'a S,
    offset: usize,
    len_width: usize,
    elem_size: usize,
    len: usize,
    capacity: usize,
    _marker: PhantomData<(T, K)>,
}

impl<'a, T, S> FixedCapacityVec<'a, T, S, FixedValueElement>
where
    S: LayoutStorageMut + ?Sized,
{
    #[doc(hidden)]
    pub fn new_fixed_value_storage(
        storage: &'a S,
        offset: usize,
        len_width: usize,
        elem_size: usize,
        capacity: usize,
    ) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        validate_storage_len(bytes.len(), offset, len_width, elem_size, capacity)?;
        let len = read_len_header(&bytes, offset, len_width)?;
        if len > capacity {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            len_width,
            elem_size,
            len,
            capacity,
            _marker: PhantomData,
        })
    }
}

impl<'a, T, S> FixedCapacityVec<'a, T, S, FixedLayoutElement>
where
    S: LayoutStorageMut + ?Sized,
{
    #[doc(hidden)]
    pub fn new_fixed_layout_storage(
        storage: &'a S,
        offset: usize,
        len_width: usize,
        elem_size: usize,
        capacity: usize,
    ) -> Result<Self, ProgramError>
    where
        T: FixedSizeLayout,
    {
        let bytes = storage.borrow_data()?;
        validate_storage_len(bytes.len(), offset, len_width, elem_size, capacity)?;
        let len = read_len_header(&bytes, offset, len_width)?;
        if len > capacity {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        let data_start = offset + len_width;
        let active_end = data_start + len * elem_size;
        for chunk in bytes[data_start..active_end].chunks_exact(T::DATA_LEN) {
            T::decode(chunk).map_err(ProgramError::from)?;
        }
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            len_width,
            elem_size,
            len,
            capacity,
            _marker: PhantomData,
        })
    }
}

impl<'a, T, S: ?Sized, K> FixedCapacityVec<'a, T, S, K> {
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

impl<'a, T, S> FixedCapacityVec<'a, T, S, FixedValueElement>
where
    T: Pod,
    S: LayoutStorageMut + ?Sized,
{
    /// Returns the active element at `index`.
    pub fn get(&self, index: usize) -> Result<Option<T>, ProgramError> {
        if index >= self.len {
            return Ok(None);
        }

        let bytes = self.storage.borrow_data()?;
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        Ok(Some(bytemuck::pod_read_unaligned(&bytes[start..end])))
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
        self.write_bytes_at(index, bytemuck::bytes_of(&value))?;
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
        let value = self.get(index)?.expect("validated Vec length");
        self.write_len(index)?;
        self.len = index;
        Ok(Some(value))
    }

    /// Replaces the active element at `index`.
    pub fn set(&mut self, index: usize, value: T) -> Result<(), ProgramError> {
        if index >= self.len {
            return Err(DataLayoutError::InvalidDataLength.into());
        }

        self.write_bytes_at(index, bytemuck::bytes_of(&value))
    }
}

impl<'a, T, S> FixedCapacityVec<'a, T, S, FixedLayoutElement>
where
    T: Encodable + FixedSizeLayout,
    S: LayoutStorageMut + ?Sized,
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

impl<'a, T, S, K> FixedCapacityVec<'a, T, S, K>
where
    S: LayoutStorageMut + ?Sized,
{
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

    fn write_len(&self, len: usize) -> Result<(), ProgramError> {
        if len > self.capacity || len > max_len(self.len_width) {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        let mut bytes = self.storage.borrow_data_mut()?;
        write_len_header(&mut bytes, self.offset, self.len_width, len)
    }

    fn write_bytes_at(&self, index: usize, value: &[u8]) -> Result<(), ProgramError> {
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        let mut bytes = self.storage.borrow_data_mut()?;
        bytes[start..end].copy_from_slice(value);
        Ok(())
    }

    fn write_encoded_at(&self, index: usize, value: &T) -> Result<(), ProgramError>
    where
        T: Encodable,
    {
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        let mut bytes = self.storage.borrow_data_mut()?;
        let remaining = value.encode_to(&mut bytes[start..end])?;
        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength.into());
        }
        Ok(())
    }
}

fn validate_storage_len(
    storage_len: usize,
    offset: usize,
    len_width: usize,
    elem_size: usize,
    capacity: usize,
) -> Result<(), ProgramError> {
    let expected_len = offset
        .checked_add(len_width)
        .and_then(|len| len.checked_add(capacity.checked_mul(elem_size)?))
        .ok_or(DataLayoutError::LengthExceedsCapacity)?;
    if storage_len < expected_len {
        return Err(DataLayoutError::InvalidDataLength.into());
    }
    Ok(())
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
        _ => usize::MAX,
    }
}
