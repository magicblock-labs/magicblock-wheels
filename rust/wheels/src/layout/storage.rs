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
    type RefMut<'a>: LayoutWriteGuard
    where
        Self: 'a;

    /// Acquires the guard held by `decode_mut()`. Custom storage implements
    /// [`LayoutWriteGuard`] on this guard to handle resizing during a borrow.
    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError>;

    /// Resizes the backing data buffer.
    fn resize(&self, new_len: usize) -> Result<(), ProgramError> {
        self.borrow_data_mut()?.resize(new_len)
    }
}

/// Exclusive access to the bytes held by a mutable layout view.
pub trait LayoutWriteGuard: DerefMut<Target = [u8]> {
    /// Resizes to the exact length and preserves the common prefix. If the
    /// buffer moves, its address modulo eight must stay the same so a decoded
    /// layout can keep using its aligned fields. On error the original bytes
    /// and length must remain accessible through this guard.
    fn resize(&mut self, new_len: usize) -> Result<(), ProgramError>;

    /// Called when a vector needs more storage. `min_len` is the required
    /// total buffer length. Extra capacity must consist of whole elements.
    /// Growth has the same alignment and error guarantees as [`Self::resize`].
    fn grow(&mut self, min_len: usize, _element_size: usize) -> Result<(), ProgramError> {
        self.resize(min_len)
    }
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
        = AccountWriteGuard<'a>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        Ok(AccountWriteGuard {
            account: self,
            bytes: Some(self.try_borrow_mut()?),
        })
    }

    fn resize(&self, new_len: usize) -> Result<(), ProgramError> {
        self.resize(new_len)
    }
}

/// Exclusive account data borrow used by a mutable layout view.
pub struct AccountWriteGuard<'a> {
    account: &'a pinocchio::account::AccountView,
    bytes: Option<pinocchio::account::RefMut<'a, [u8]>>,
}

impl Deref for AccountWriteGuard<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes.as_deref().expect("account data is borrowed")
    }
}

impl DerefMut for AccountWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.bytes.as_deref_mut().expect("account data is borrowed")
    }
}

impl LayoutWriteGuard for AccountWriteGuard<'_> {
    fn resize(&mut self, new_len: usize) -> Result<(), ProgramError> {
        if new_len == self.len() {
            return Ok(());
        }

        // AccountView requires an unborrowed account to resize. Its synchronous
        // resize runs no user code, so we can immediately reclaim our exclusive
        // borrow at the current length, even when the runtime rejected growth.
        drop(self.bytes.take());
        let result = self.account.resize(new_len);
        self.bytes = Some(
            self.account
                .try_borrow_mut()
                .expect("exclusive account access"),
        );
        result
    }
}

/// Growth-capping wrapper for mutable layout storage.
///
/// A trailing Vec grows in additive steps, with spare bytes rounded down to
/// whole elements. Flexible options and shrink operations use the exact
/// requested length.
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
        = MaxLenWriteGuard<Storage::RefMut<'a>>
    where
        Self: 'a;

    fn borrow_data_mut(&self) -> Result<Self::RefMut<'_>, ProgramError> {
        Ok(MaxLenWriteGuard {
            bytes: self.storage.borrow_data_mut()?,
            max_data_len: self.max_data_len,
            resize_step: self.resize_step,
        })
    }

    fn resize(&self, new_len: usize) -> Result<(), ProgramError> {
        let mut bytes = self.borrow_data_mut()?;
        if new_len > bytes.len() {
            bytes.grow(new_len, 1)
        } else {
            bytes.resize(new_len)
        }
    }
}

/// Exclusive byte access with the growth policy of [`MaxLenStorage`].
pub struct MaxLenWriteGuard<G> {
    bytes: G,
    max_data_len: usize,
    resize_step: usize,
}

impl<G: LayoutWriteGuard> Deref for MaxLenWriteGuard<G> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl<G: LayoutWriteGuard> DerefMut for MaxLenWriteGuard<G> {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

impl<G: LayoutWriteGuard> LayoutWriteGuard for MaxLenWriteGuard<G> {
    fn resize(&mut self, new_len: usize) -> Result<(), ProgramError> {
        if new_len > self.len() && new_len > self.max_data_len {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }
        self.bytes.resize(new_len)
    }

    fn grow(&mut self, min_len: usize, element_size: usize) -> Result<(), ProgramError> {
        if min_len > self.max_data_len {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }
        let stepped = self
            .len()
            .saturating_add(self.resize_step)
            .min(self.max_data_len);
        let spare = stepped.saturating_sub(min_len);
        let remainder = spare
            .checked_rem(element_size)
            .ok_or(DataLayoutError::InvalidDataLength)?;
        self.bytes.resize(min_len + (spare - remainder))
    }
}
