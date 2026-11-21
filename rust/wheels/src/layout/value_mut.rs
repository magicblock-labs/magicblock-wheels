use core::marker::PhantomData;

use bytemuck::Pod;
use pinocchio::error::ProgramError;

use super::{Encodable, FixedSizeLayout, LayoutBounds, LayoutWriteGuard};
use crate::DataLayoutError;

/// Mutable slot for an integer or fixed-size integer array accessed by byte copy.
///
/// - `field: u64`
/// - `field: [u16; 2]`
///
/// The macro returns this helper when it cannot guarantee alignment for a
/// mutable reference. For the integer field, `view.field_mut().set(42)`
/// writes the encoded value.
pub struct LayoutCopyMut<'a, T> {
    bytes: &'a mut [u8],
    _marker: PhantomData<T>,
}

impl<'a, T: Pod> LayoutCopyMut<'a, T> {
    #[doc(hidden)]
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            bytes,
            _marker: PhantomData,
        }
    }

    pub fn get(&self) -> T {
        bytemuck::pod_read_unaligned(self.bytes)
    }

    pub fn set(&mut self, value: T) {
        self.bytes.copy_from_slice(bytemuck::bytes_of(&value));
    }
}

/// Mutable slot for a boolean encoded as one byte.
///
/// - `field: bool`
///
/// `view.field_mut().set(true)` writes the encoded byte. Optional booleans
/// use [`LayoutBoolOptionMut`].
pub struct LayoutBoolMut<'a>(&'a mut u8);

impl<'a> LayoutBoolMut<'a> {
    #[doc(hidden)]
    pub fn new(byte: &'a mut u8) -> Self {
        Self(byte)
    }

    pub fn get(&self) -> bool {
        *self.0 != 0
    }

    pub fn set(&mut self, value: bool) {
        *self.0 = u8::from(value);
    }
}

/// Resizable backing store used by [`LayoutOptionMut`], [`LayoutBoolOptionMut`],
/// and [`FlexibleLayoutOptionMut`].
///
/// - `#[flexible] field: Option<Pubkey>`
/// - `#[flexible] field: Option<u64>`
/// - `#[flexible] field: Option<[u8; 32]>`
/// - `#[flexible] field: Option<bool>`
/// - `#[flexible] field: Option<AccountConfig>`
///
/// For `Some`, `LayoutOptionMut` requests space for the tag and the bytes of
/// a key, integer, or array. `LayoutBoolOptionMut` delegates to it with a `u8`
/// payload. `FlexibleLayoutOptionMut` uses the nested layout's encoded length
/// to size the slot before encoding the value. All three clear the trailing
/// field by resizing storage back to its offset.
pub struct FlexibleOptionBytes<'a, G> {
    borrowed: &'a mut G,
    offset: usize,
}

impl<'a, G: LayoutWriteGuard> FlexibleOptionBytes<'a, G> {
    #[doc(hidden)]
    pub fn new(borrowed: &'a mut G, offset: usize) -> Self {
        Self { borrowed, offset }
    }

    fn resize(&mut self, payload_len: Option<usize>) -> Result<&mut [u8], ProgramError> {
        let len = match payload_len {
            Some(len) => self
                .offset
                .checked_add(1)
                .and_then(|start| start.checked_add(len))
                .ok_or(DataLayoutError::InvalidDataLength)?,
            None => self.offset,
        };
        self.borrowed.resize(len)?;
        Ok(&mut self.borrowed[self.offset..])
    }
}

impl<G: LayoutWriteGuard> AsRef<[u8]> for FlexibleOptionBytes<'_, G> {
    fn as_ref(&self) -> &[u8] {
        &self.borrowed[self.offset..]
    }
}

/// Mutable slot for `Option<T>` where `T` is a supported integer, a fixed-size
/// integer array, or a `Pubkey`/`Address`.
///
/// - `field: Option<u64>`
/// - `field: Option<[u8; 32]>`
/// - `field: Option<Address>`
/// - `#[flexible] field: Option<Pubkey>`
///
/// For a fixed field, `B` is `&mut [u8]` and `view.field_mut().set(None)`
/// clears the tag in place. For a trailing `#[flexible]` field, `B` is
/// [`FlexibleOptionBytes`] and `view.field_mut().set(None)?` can shrink storage.
pub struct LayoutOptionMut<B, T> {
    slot: B,
    _marker: PhantomData<T>,
}

impl<B: AsRef<[u8]>, T: Pod> LayoutOptionMut<B, T> {
    #[doc(hidden)]
    pub fn new(slot: B) -> Self {
        Self {
            slot,
            _marker: PhantomData,
        }
    }

    pub fn get(&self) -> Option<T> {
        let (tag, payload) = self.slot.as_ref().split_first()?;
        (*tag == 1).then(|| bytemuck::pod_read_unaligned(payload))
    }
}

impl<T: Pod> LayoutOptionMut<&mut [u8], T> {
    pub fn set(&mut self, value: Option<T>) {
        self.slot[0] = u8::from(value.is_some());
        if let Some(value) = value {
            self.slot[1..].copy_from_slice(bytemuck::bytes_of(&value));
        }
    }
}

impl<G: LayoutWriteGuard, T: Pod> LayoutOptionMut<FlexibleOptionBytes<'_, G>, T> {
    pub fn set(&mut self, value: Option<T>) -> Result<(), ProgramError> {
        let slot = self
            .slot
            .resize(value.as_ref().map(core::mem::size_of_val))?;
        if let Some(value) = value {
            LayoutOptionMut::new(slot).set(Some(value));
        }
        Ok(())
    }
}

/// Mutable slot for an `Option<bool>` field, with one byte for the boolean payload.
///
/// - `field: Option<bool>`
/// - `#[flexible] field: Option<bool>`
///
/// `view.field_mut().set(Some(false))` updates a fixed slot in place.
/// The flexible form uses [`FlexibleOptionBytes`] and returns a `Result`
/// because setting the value can grow the trailing field.
pub struct LayoutBoolOptionMut<B>(LayoutOptionMut<B, u8>);

impl<B: AsRef<[u8]>> LayoutBoolOptionMut<B> {
    #[doc(hidden)]
    pub fn new(slot: B) -> Self {
        Self(LayoutOptionMut::new(slot))
    }

    pub fn get(&self) -> Option<bool> {
        self.0.get().map(|byte| byte != 0)
    }
}

impl LayoutBoolOptionMut<&mut [u8]> {
    pub fn set(&mut self, value: Option<bool>) {
        self.0.set(value.map(u8::from));
    }
}

impl<G: LayoutWriteGuard> LayoutBoolOptionMut<FlexibleOptionBytes<'_, G>> {
    pub fn set(&mut self, value: Option<bool>) -> Result<(), ProgramError> {
        self.0.set(value.map(u8::from))
    }
}

/// Mutable slot for a final `Option<T>` field where `T` is a user-defined
/// [`FixedSizeLayout`].
///
/// - `field: Option<AccountConfig>`
///
/// `view.field_mut().set(Some(&config))?` encodes the nested fields in the
/// reserved slot. Clearing it retains the slot.
pub struct FixedLayoutOptionMut<'a, T> {
    slot: &'a mut [u8],
    _marker: PhantomData<T>,
}

impl<'a, T: FixedSizeLayout> FixedLayoutOptionMut<'a, T> {
    #[doc(hidden)]
    pub fn new(slot: &'a mut [u8]) -> Self {
        Self {
            slot,
            _marker: PhantomData,
        }
    }

    pub fn set(&mut self, value: Option<&T>) -> Result<(), ProgramError> {
        write_nested_option(self.slot, value)
    }
}

/// Mutable slot for a final `#[flexible] Option<T>` containing a user-defined
/// layout whose encoded size has known [bounds](LayoutBounds).
///
/// - `#[flexible] field: Option<AccountConfig>`
/// - `#[flexible] field: Option<BytePayload>`
///
/// The nested layout can have a fixed size (`AccountConfig`) or contain a
/// trailing vector (`BytePayload`). `view.field_mut().set(Some(&config))?`
/// resizes the field to fit the encoded value.
pub struct FlexibleLayoutOptionMut<'a, G, T> {
    slot: FlexibleOptionBytes<'a, G>,
    _marker: PhantomData<T>,
}

impl<'a, G: LayoutWriteGuard, T: Encodable + LayoutBounds> FlexibleLayoutOptionMut<'a, G, T> {
    #[doc(hidden)]
    pub fn new(slot: FlexibleOptionBytes<'a, G>) -> Self {
        Self {
            slot,
            _marker: PhantomData,
        }
    }

    pub fn set(&mut self, value: Option<&T>) -> Result<(), ProgramError> {
        let Some(value) = value else {
            self.slot.resize(None)?;
            return Ok(());
        };
        let len = value.encoded_len()?;
        if !(T::MIN_DATA_LEN..=T::MAX_DATA_LEN).contains(&len) {
            return Err(DataLayoutError::InvalidDataLength.into());
        }
        write_nested_option(self.slot.resize(Some(len))?, Some(value))
    }
}

fn write_nested_option<T: Encodable>(
    slot: &mut [u8],
    value: Option<&T>,
) -> Result<(), ProgramError> {
    if let Some(value) = value {
        if !value.encode_to(&mut slot[1..])?.is_empty() {
            return Err(DataLayoutError::InvalidDataLength.into());
        }
    }
    slot[0] = u8::from(value.is_some());
    Ok(())
}
