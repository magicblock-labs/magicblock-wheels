use core::ops::{Deref, DerefMut};

use pinocchio::error::ProgramError;

use crate::DataLayoutError;

/// Byte storage that can back a generated layout view.
///
/// This trait exists so generated `fixed_offset_layout` mutable views can work
/// with account-like types without depending on a single account wrapper API.
/// [`pinocchio::account::AccountView`] implements it out of the box.
pub trait LayoutStorage {
    type Ref<'a>: Deref<Target = [u8]>
    where
        Self: 'a;

    /// Returns the current length of the backing data buffer.
    fn data_len(&self) -> usize;

    /// Borrows the backing data buffer immutably.
    fn borrow_data(&self) -> Result<Self::Ref<'_>, ProgramError>;
}

/// Resizable byte storage that can back a generated mutable layout view.
///
/// Implement this trait for account/storage wrappers that can expose mutable
/// bytes and resize the underlying data buffer. Generated `decode_mut()`
/// methods use it for fixed-layout account mutation. Fixed-capacity fields
/// mutate in place; trailing variable-length fields may also resize the backing
/// data.
pub trait LayoutStorageMut: LayoutStorage {
    type RefMut<'a>: DerefMut<Target = [u8]>
    where
        Self: 'a;

    /// Borrows the backing data buffer mutably.
    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError>;

    /// Resizes the backing data buffer.
    fn resize(&self, new_len: usize) -> Result<(), ProgramError>;
}

impl LayoutStorage for pinocchio::account::AccountView {
    type Ref<'a>
        = pinocchio::account::Ref<'a, [u8]>
    where
        Self: 'a;

    fn data_len(&self) -> usize {
        self.data_len()
    }

    fn borrow_data(&self) -> Result<Self::Ref<'_>, ProgramError> {
        self.try_borrow()
    }
}

impl LayoutStorageMut for pinocchio::account::AccountView {
    type RefMut<'a>
        = pinocchio::account::RefMut<'a, [u8]>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        self.try_borrow_mut()
    }

    fn resize(&self, new_len: usize) -> Result<(), ProgramError> {
        self.resize(new_len)
    }
}

/// Growth-capping wrapper for mutable layout storage.
///
/// Generated mutable views ask storage to resize to the exact byte length they
/// currently need. Wrapping account-like storage in `MaxLenStorage` keeps that
/// exact behavior for shrinking, but grows in additive steps so repeated
/// trailing Vec pushes do not need to realloc on every element.
pub struct MaxLenStorage<'a, Storage: ?Sized> {
    storage: &'a Storage,
    max_data_len: usize,
    resize_step: usize,
}

impl<'a, Storage: ?Sized> MaxLenStorage<'a, Storage> {
    /// Creates a wrapper around `storage`.
    ///
    /// `max_len` is an inclusive cap on the absolute backing storage length in
    /// bytes. `step` controls extra growth beyond the requested length. A step
    /// of zero preserves exact growth while still enforcing `max_len`.
    pub const fn new(storage: &'a Storage, max_data_len: usize, resize_step: usize) -> Self {
        Self {
            storage,
            max_data_len,
            resize_step,
        }
    }

    /// Returns the wrapped storage.
    pub const fn storage(&self) -> &'a Storage {
        self.storage
    }

    /// Returns the maximum absolute backing storage length in bytes.
    pub const fn max_len(&self) -> usize {
        self.max_data_len
    }

    /// Returns the additive growth step in bytes.
    pub const fn step(&self) -> usize {
        self.resize_step
    }
}

impl<Storage> LayoutStorage for MaxLenStorage<'_, Storage>
where
    Storage: LayoutStorage + ?Sized,
{
    type Ref<'a>
        = Storage::Ref<'a>
    where
        Self: 'a;

    fn data_len(&self) -> usize {
        self.storage.data_len()
    }

    fn borrow_data(&self) -> Result<Self::Ref<'_>, ProgramError> {
        self.storage.borrow_data()
    }
}

impl<Storage> LayoutStorageMut for MaxLenStorage<'_, Storage>
where
    Storage: LayoutStorageMut + ?Sized,
{
    type RefMut<'a>
        = Storage::RefMut<'a>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        self.storage.borrow_data_mut()
    }

    fn resize(&self, requested_len: usize) -> Result<(), ProgramError> {
        let current_len = self.storage.data_len();
        if requested_len <= current_len {
            // Shrinks exactly; step growth only applies when storage grows.
            return self.storage.resize(requested_len);
        }

        if requested_len > self.max_data_len {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        let stepped_len = current_len.saturating_add(self.resize_step);
        let new_len = requested_len.max(stepped_len).min(self.max_data_len);
        self.storage.resize(new_len)
    }
}
