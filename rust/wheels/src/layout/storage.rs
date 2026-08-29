use core::ops::{Deref, DerefMut};

use pinocchio::error::ProgramError;

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
