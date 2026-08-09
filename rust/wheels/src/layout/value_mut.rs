use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use bytemuck::Pod;
use pinocchio::error::ProgramError;

use super::LayoutStorageMut;
use crate::DataLayoutError;

/// Mutable typed view over an aligned fixed-value layout field.
///
/// This guard owns the backing storage borrow, so callers can use it like a
/// normal `&mut T` through [`DerefMut`] without returning a dangling reference
/// into account data.
pub struct LayoutValueMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized + 'a,
    T: Pod,
{
    bytes: S::RefMut<'a>,
    offset: usize,
    _marker: PhantomData<T>,
}

impl<'a, S, T> LayoutValueMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized + 'a,
    T: Pod,
{
    #[doc(hidden)]
    pub fn new(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data_mut()?;
        let end = checked_end(offset, core::mem::size_of::<T>())?;
        if bytes.len() < end {
            return Err(DataLayoutError::InvalidDataLength.into());
        }

        let ptr = bytes[offset..end].as_ptr() as usize;
        let align = core::mem::align_of::<T>();
        if align > 1 && !ptr.is_multiple_of(align) {
            return Err(DataLayoutError::InvalidFieldAlignment.into());
        }

        Ok(Self {
            bytes,
            offset,
            _marker: PhantomData,
        })
    }

    fn range(&self) -> core::ops::Range<usize> {
        let end = self.offset + core::mem::size_of::<T>();
        self.offset..end
    }
}

impl<'a, S, T> Deref for LayoutValueMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized + 'a,
    T: Pod,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        bytemuck::from_bytes(&self.bytes[self.range()])
    }
}

impl<'a, S, T> DerefMut for LayoutValueMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized + 'a,
    T: Pod,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        let range = self.range();
        bytemuck::from_bytes_mut(&mut self.bytes[range])
    }
}

/// Mutable byte-copy slot for fixed-value fields that cannot be borrowed as
/// aligned `&mut T`.
pub struct LayoutCopyMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized,
    T: Pod,
{
    storage: &'a S,
    offset: usize,
    _marker: PhantomData<T>,
}

impl<'a, S, T> LayoutCopyMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized,
    T: Pod,
{
    #[doc(hidden)]
    pub fn new(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        ensure_range(&bytes, offset, core::mem::size_of::<T>())?;
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            _marker: PhantomData,
        })
    }

    pub fn get(&self) -> Result<T, ProgramError> {
        let bytes = self.storage.borrow_data()?;
        let end = checked_end(self.offset, core::mem::size_of::<T>())?;
        Ok(bytemuck::pod_read_unaligned(&bytes[self.offset..end]))
    }

    pub fn set(&mut self, value: T) -> Result<(), ProgramError> {
        let mut bytes = self.storage.borrow_data_mut()?;
        let end = checked_end(self.offset, core::mem::size_of::<T>())?;
        bytes[self.offset..end].copy_from_slice(bytemuck::bytes_of(&value));
        Ok(())
    }
}

/// Mutable slot for a layout-encoded `bool`.
///
/// The encoded representation is a backing `u8`, not Rust's in-memory `bool`
/// representation, so this type exposes `get`/`set` instead of `DerefMut`.
pub struct LayoutBoolMut<'a, S>
where
    S: LayoutStorageMut + ?Sized,
{
    storage: &'a S,
    offset: usize,
}

impl<'a, S> LayoutBoolMut<'a, S>
where
    S: LayoutStorageMut + ?Sized,
{
    #[doc(hidden)]
    pub fn new(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        ensure_range(&bytes, offset, 1)?;
        drop(bytes);

        Ok(Self { storage, offset })
    }

    pub fn get(&self) -> Result<bool, ProgramError> {
        let bytes = self.storage.borrow_data()?;
        ensure_range(&bytes, self.offset, 1)?;
        Ok(bytes[self.offset] != 0)
    }

    pub fn set(&mut self, value: bool) -> Result<(), ProgramError> {
        let mut bytes = self.storage.borrow_data_mut()?;
        ensure_range(&bytes, self.offset, 1)?;
        bytes[self.offset] = u8::from(value);
        Ok(())
    }
}

/// Mutable slot for a layout-encoded `Option<T>`.
///
/// The encoded representation is a tag byte plus payload bytes, so this type
/// exposes `get`/`set` instead of `DerefMut<Target = Option<T>>`.
pub struct LayoutOptionMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized,
    T: Pod,
{
    storage: &'a S,
    offset: usize,
    flexible: bool,
    _marker: PhantomData<T>,
}

impl<'a, S, T> LayoutOptionMut<'a, S, T>
where
    S: LayoutStorageMut + ?Sized,
    T: Pod,
{
    #[doc(hidden)]
    pub fn new_fixed(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        ensure_range(&bytes, offset, 1 + core::mem::size_of::<T>())?;
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            flexible: false,
            _marker: PhantomData,
        })
    }

    #[doc(hidden)]
    pub fn new_flexible(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        if bytes.len() != offset {
            ensure_range(&bytes, offset, 1 + core::mem::size_of::<T>())?;
        }
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            flexible: true,
            _marker: PhantomData,
        })
    }

    pub fn get(&self) -> Result<Option<T>, ProgramError> {
        let bytes = self.storage.borrow_data()?;
        if self.flexible && bytes.len() == self.offset {
            return Ok(None);
        }

        ensure_range(&bytes, self.offset, 1)?;
        if bytes[self.offset] == 0 {
            return Ok(None);
        }
        if bytes[self.offset] != 1 {
            return Err(DataLayoutError::InvalidOptionTag.into());
        }

        let start = self.offset + 1;
        let end = checked_end(start, core::mem::size_of::<T>())?;
        if bytes.len() < end {
            return Err(DataLayoutError::TruncatedPayload.into());
        }
        Ok(Some(bytemuck::pod_read_unaligned(&bytes[start..end])))
    }

    pub fn set(&mut self, value: Option<T>) -> Result<(), ProgramError> {
        match value {
            Some(value) => self.set_some(value),
            None => self.set_none(),
        }
    }

    fn set_some(&mut self, value: T) -> Result<(), ProgramError> {
        let value_size = core::mem::size_of::<T>();
        let end = checked_end(self.offset + 1, value_size)?;
        if self.flexible {
            self.storage.resize(end)?;
        }

        let mut bytes = self.storage.borrow_data_mut()?;
        ensure_range(&bytes, self.offset, 1 + value_size)?;
        bytes[self.offset + 1..end].copy_from_slice(bytemuck::bytes_of(&value));
        bytes[self.offset] = 1;
        Ok(())
    }

    fn set_none(&mut self) -> Result<(), ProgramError> {
        if self.flexible {
            self.storage.resize(self.offset)?;
            return Ok(());
        }

        let mut bytes = self.storage.borrow_data_mut()?;
        ensure_range(&bytes, self.offset, 1)?;
        bytes[self.offset] = 0;
        Ok(())
    }
}

/// Mutable slot for a layout-encoded `Option<bool>`.
pub struct LayoutBoolOptionMut<'a, S>
where
    S: LayoutStorageMut + ?Sized,
{
    storage: &'a S,
    offset: usize,
    flexible: bool,
}

impl<'a, S> LayoutBoolOptionMut<'a, S>
where
    S: LayoutStorageMut + ?Sized,
{
    #[doc(hidden)]
    pub fn new_fixed(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        ensure_range(&bytes, offset, 2)?;
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            flexible: false,
        })
    }

    #[doc(hidden)]
    pub fn new_flexible(storage: &'a S, offset: usize) -> Result<Self, ProgramError> {
        let bytes = storage.borrow_data()?;
        if bytes.len() != offset {
            ensure_range(&bytes, offset, 2)?;
        }
        drop(bytes);

        Ok(Self {
            storage,
            offset,
            flexible: true,
        })
    }

    pub fn get(&self) -> Result<Option<bool>, ProgramError> {
        let bytes = self.storage.borrow_data()?;
        if self.flexible && bytes.len() == self.offset {
            return Ok(None);
        }

        ensure_range(&bytes, self.offset, 1)?;
        if bytes[self.offset] == 0 {
            return Ok(None);
        }
        if bytes[self.offset] != 1 {
            return Err(DataLayoutError::InvalidOptionTag.into());
        }

        ensure_range(&bytes, self.offset + 1, 1)?;
        Ok(Some(bytes[self.offset + 1] != 0))
    }

    pub fn set(&mut self, value: Option<bool>) -> Result<(), ProgramError> {
        match value {
            Some(value) => self.set_some(value),
            None => self.set_none(),
        }
    }

    fn set_some(&mut self, value: bool) -> Result<(), ProgramError> {
        if self.flexible {
            self.storage.resize(self.offset + 2)?;
        }

        let mut bytes = self.storage.borrow_data_mut()?;
        ensure_range(&bytes, self.offset, 2)?;
        bytes[self.offset + 1] = u8::from(value);
        bytes[self.offset] = 1;
        Ok(())
    }

    fn set_none(&mut self) -> Result<(), ProgramError> {
        if self.flexible {
            self.storage.resize(self.offset)?;
            return Ok(());
        }

        let mut bytes = self.storage.borrow_data_mut()?;
        ensure_range(&bytes, self.offset, 1)?;
        bytes[self.offset] = 0;
        Ok(())
    }
}

fn checked_end(offset: usize, len: usize) -> Result<usize, ProgramError> {
    offset
        .checked_add(len)
        .ok_or(DataLayoutError::InvalidDataLength.into())
}

fn ensure_range(bytes: &[u8], offset: usize, len: usize) -> Result<(), ProgramError> {
    let end = checked_end(offset, len)?;
    if bytes.len() < end {
        return Err(DataLayoutError::InvalidDataLength.into());
    }
    Ok(())
}
