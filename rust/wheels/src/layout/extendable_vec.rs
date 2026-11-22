use core::{marker::PhantomData, ops::Range};

use bytemuck::Pod;
use pinocchio::error::ProgramError;

use super::{Encodable, FixedSizeLayout, LayoutWriteGuard, MAX_SUPPORTED_VEC_LEN};
use crate::DataLayoutError;

/// Marker for extendable Vec elements that are fixed-value/POD layout fields.
///
/// This is used by generated code to select the `&[T]`-style access path.
#[derive(Clone, Copy, Debug)]
pub enum FixedValueElement {}

/// Marker for extendable Vec elements that are fixed-size layout structs.
///
/// This is used by generated code to select the `T::View<'a>` access path.
#[derive(Clone, Copy, Debug)]
pub enum FixedLayoutElement {}

enum ExtendableVecSource<'a, S: ?Sized> {
    Bytes(&'a [u8]),
    Storage(&'a mut S),
}

/// View over a trailing `#[extendable = N] Vec<T>` in `fixed_offset_layout`.
///
/// Users continue to write `Vec<T>` in layout structs. Generated immutable
/// getters return `ExtendableVec` for a trailing extendable Vec so the view can
/// expose both the active logical length and the storage capacity represented
/// by the backing bytes. Generated mutable getters from `decode_mut()` return
/// the same type backed by [`LayoutWriteGuard`], which enables Vec-like
/// operations such as `push`, `extend_from_slice`, `pop`, `set`, `truncate`,
/// and `clear`.
///
/// An extendable Vec has no reserved bytes in canonical `encode()` output. When
/// decoding account/storage bytes, however, the final field may include spare
/// trailing storage. `len()` reports the active element count, while
/// `capacity()` reports how many elements fit in the supplied backing storage.
///
/// CHECKPOINT(CU): generated fixed-layout getters pass compile-time constant
/// offsets and element metadata into this generic helper. They are stored as
/// runtime fields to keep return types simple. If CU measurements show this
/// matters for very small budgets, benchmark const-generic offsets/metadata or
/// field-specific generated wrappers.
pub struct ExtendableVec<'a, T, S: ?Sized = (), K = FixedValueElement> {
    source: ExtendableVecSource<'a, S>,
    offset: usize,
    len_width: usize,
    elem_size: usize,
    len: usize,
    capacity: usize,
    _marker: PhantomData<(T, K)>,
}

impl<'a, T> ExtendableVec<'a, T, (), FixedValueElement> {
    #[doc(hidden)]
    pub fn new_fixed_value(
        bytes: &'a [u8],
        offset: usize,
        len_width: usize,
        elem_size: usize,
    ) -> Result<Self, DataLayoutError> {
        let (len, capacity) = decode_storage_len(bytes, offset, len_width, elem_size)?;
        Ok(Self {
            source: ExtendableVecSource::Bytes(bytes),
            offset,
            len_width,
            elem_size,
            len,
            capacity,
            _marker: PhantomData,
        })
    }
}

impl<'a, T> ExtendableVec<'a, T, (), FixedLayoutElement> {
    #[doc(hidden)]
    pub fn new_fixed_layout(
        bytes: &'a [u8],
        offset: usize,
        len_width: usize,
        elem_size: usize,
    ) -> Result<Self, DataLayoutError>
    where
        T: FixedSizeLayout,
    {
        let (len, capacity) = decode_storage_len(bytes, offset, len_width, elem_size)?;
        let vec = Self {
            source: ExtendableVecSource::Bytes(bytes),
            offset,
            len_width,
            elem_size,
            len,
            capacity,
            _marker: PhantomData,
        };

        for chunk in vec.active_bytes().chunks_exact(T::DATA_LEN) {
            T::decode(chunk)?;
        }

        Ok(vec)
    }
}

impl<'a, T, S: LayoutWriteGuard, K> ExtendableVec<'a, T, S, K> {
    #[doc(hidden)]
    pub fn new_storage(
        storage: &'a mut S,
        offset: usize,
        len_width: usize,
        elem_size: usize,
    ) -> Self {
        let (len, capacity) = decode_storage_len(storage, offset, len_width, elem_size)
            .expect("validated extendable Vec");
        Self {
            source: ExtendableVecSource::Storage(storage),
            offset,
            len_width,
            elem_size,
            len,
            capacity,
            _marker: PhantomData,
        }
    }
}

impl<'a, T, S: ?Sized, K> ExtendableVec<'a, T, S, K> {
    /// Returns the active logical element count.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns how many elements the vector can hold in its current storage.
    ///
    /// This counts both occupied and spare slots, up to the supported element
    /// limit for the length header.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns whether the active logical element count is zero.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the active encoded length of this Vec field.
    ///
    /// Empty extendable Vec values omit the length header and payload, so their
    /// active encoded length is zero.
    pub fn encoded_len(&self) -> usize {
        if self.len == 0 {
            0
        } else {
            self.len_width + self.len * self.elem_size
        }
    }

    fn data_start(&self) -> usize {
        self.offset + self.len_width
    }

    fn active_range(&self) -> Range<usize> {
        let start = self.data_start();
        start..start + self.len * self.elem_size
    }
}

impl<'a, T, K> ExtendableVec<'a, T, (), K> {
    fn all_bytes(&self) -> &'a [u8] {
        match self.source {
            ExtendableVecSource::Bytes(bytes) => bytes,
            ExtendableVecSource::Storage(_) => unreachable!(),
        }
    }

    /// Returns the trailing storage region for this Vec field.
    ///
    /// The returned bytes include the length header when one is present and all
    /// spare trailing capacity supplied to `decode()`. They do not include the
    /// fixed fields that come before the extendable Vec.
    pub fn storage_bytes(&self) -> &'a [u8] {
        &self.all_bytes()[self.offset..]
    }

    /// Returns the byte count from the start of this field to the end of the buffer.
    ///
    /// This includes the length header, when present, and all occupied and spare
    /// slots. With a two-byte header and four `u16` slots, [`capacity()`](Self::capacity)
    /// is 4 and `storage_len()` is `2 + 4 * 2 = 10` bytes. An empty vector
    /// encoded without a header uses zero bytes.
    pub fn storage_len(&self) -> usize {
        self.storage_bytes().len()
    }

    /// Returns the active Vec payload bytes, without the length header.
    pub fn active_bytes(&self) -> &'a [u8] {
        if self.len == 0 {
            &[]
        } else {
            &self.all_bytes()[self.active_range()]
        }
    }
}

impl<'a, T> ExtendableVec<'a, T, (), FixedValueElement>
where
    T: Pod,
{
    /// Returns the active elements as a borrowed slice.
    pub fn as_slice(&self) -> &'a [T] {
        bytemuck::cast_slice(self.active_bytes())
    }
}

impl<'a, T> ExtendableVec<'a, T, (), FixedLayoutElement>
where
    T: FixedSizeLayout,
{
    /// Returns the active element at `index`.
    pub fn get(&self, index: usize) -> Option<T::View<'a>> {
        if index >= self.len {
            return None;
        }

        let start = self.data_start() + index * T::DATA_LEN;
        let end = start + T::DATA_LEN;
        Some(T::decode(&self.all_bytes()[start..end]).expect("validated fixed layout element"))
    }

    /// Iterates over active fixed-size layout elements.
    pub fn iter(&self) -> FixedLayoutExtendableVecIter<'a, T> {
        FixedLayoutExtendableVecIter {
            bytes: self.active_bytes(),
            _marker: PhantomData,
        }
    }
}

pub struct FixedLayoutExtendableVecIter<'a, T>
where
    T: FixedSizeLayout,
{
    bytes: &'a [u8],
    _marker: PhantomData<T>,
}

impl<'a, T> Iterator for FixedLayoutExtendableVecIter<'a, T>
where
    T: FixedSizeLayout,
{
    type Item = T::View<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.bytes.is_empty() {
            return None;
        }

        let (element, remaining) = self.bytes.split_at(T::DATA_LEN);
        self.bytes = remaining;
        Some(T::decode(element).expect("validated fixed layout element"))
    }
}

impl<'a, T, S> ExtendableVec<'a, T, S, FixedValueElement>
where
    T: Pod,
    S: LayoutWriteGuard + ?Sized,
{
    /// Appends one fixed-value element, growing the backing storage when needed.
    pub fn push(&mut self, value: T) -> Result<(), ProgramError> {
        let next_len = self
            .len
            .checked_add(1)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        self.ensure_capacity(next_len)?;

        let index = self.len;
        self.write_bytes_at(index, bytemuck::bytes_of(&value));
        self.write_len(next_len)?;
        self.len = next_len;
        Ok(())
    }

    /// Appends fixed-value elements from a borrowed slice.
    pub fn extend_from_slice(&mut self, values: &[T]) -> Result<(), ProgramError> {
        if values.is_empty() {
            return Ok(());
        }

        let next_len = self
            .len
            .checked_add(values.len())
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        self.ensure_capacity(next_len)?;

        let value_bytes = bytemuck::cast_slice(values);
        let byte_len = values
            .len()
            .checked_mul(self.elem_size)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        if byte_len != value_bytes.len() {
            return Err(DataLayoutError::InvalidDataLength.into());
        }

        let start = self
            .data_start()
            .checked_add(
                self.len
                    .checked_mul(self.elem_size)
                    .ok_or(DataLayoutError::LengthExceedsCapacity)?,
            )
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        let end = start
            .checked_add(byte_len)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        self.storage_mut()[start..end].copy_from_slice(value_bytes);

        self.write_len(next_len)?;
        self.len = next_len;
        Ok(())
    }

    /// Removes and returns the last active fixed-value element.
    pub fn pop(&mut self) -> Result<Option<T>, ProgramError> {
        if self.len == 0 {
            return Ok(None);
        }

        let index = self.len - 1;
        let value = {
            let bytes = self.storage();
            let start = self.data_start() + index * self.elem_size;
            bytemuck::pod_read_unaligned(&bytes[start..start + self.elem_size])
        };
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

impl<'a, T, S> ExtendableVec<'a, T, S, FixedLayoutElement>
where
    T: Encodable + FixedSizeLayout,
    S: LayoutWriteGuard + ?Sized,
{
    /// Appends one fixed-size layout element, growing the backing storage when needed.
    pub fn push(&mut self, value: &T) -> Result<(), ProgramError> {
        let next_len = self
            .len
            .checked_add(1)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        self.ensure_capacity(next_len)?;

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

impl<'a, T, S, K> ExtendableVec<'a, T, S, K>
where
    S: LayoutWriteGuard + ?Sized,
{
    /// Returns the byte count from the start of this field to the end of storage.
    ///
    /// This includes the length header, when present, and the bytes for every
    /// occupied and spare slot. [`capacity()`](Self::capacity) counts elements.
    /// [`clear()`](Self::clear) keeps the allocated slots and header, so an empty
    /// vector can still have a nonzero storage length.
    pub fn storage_len(&self) -> usize {
        self.storage().len() - self.offset
    }

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

    fn storage(&self) -> &S {
        match &self.source {
            ExtendableVecSource::Storage(storage) => storage,
            ExtendableVecSource::Bytes(_) => unreachable!(),
        }
    }

    fn storage_mut(&mut self) -> &mut S {
        match &mut self.source {
            ExtendableVecSource::Storage(storage) => storage,
            ExtendableVecSource::Bytes(_) => unreachable!(),
        }
    }

    fn ensure_capacity(&mut self, len: usize) -> Result<(), ProgramError> {
        if len > max_len(self.len_width) {
            return Err(DataLayoutError::LengthExceedsCapacity.into());
        }

        let needed_len = self
            .offset
            .checked_add(self.len_width)
            .and_then(|value| value.checked_add(len.checked_mul(self.elem_size)?))
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;

        if needed_len > self.storage().len() {
            let elem_size = self.elem_size;
            self.storage_mut().grow(needed_len, elem_size)?;
            self.capacity =
                storage_capacity(self.storage().len(), self.offset, self.len_width, elem_size)?;
            if self.capacity < len {
                return Err(DataLayoutError::LengthExceedsCapacity.into());
            }
        }
        Ok(())
    }

    fn write_len(&mut self, len: usize) -> Result<(), ProgramError> {
        let offset = self.offset;
        let len_width = self.len_width;
        write_len_header(self.storage_mut(), offset, len_width, len).map_err(ProgramError::from)
    }

    fn write_bytes_at(&mut self, index: usize, value: &[u8]) {
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        let bytes = self.storage_mut();
        bytes[start..end].copy_from_slice(value);
    }

    fn write_encoded_at(&mut self, index: usize, value: &T) -> Result<(), ProgramError>
    where
        T: Encodable,
    {
        let start = self.data_start() + index * self.elem_size;
        let end = start + self.elem_size;
        let bytes = self.storage_mut();
        let remaining = value.encode_to(&mut bytes[start..end])?;
        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength.into());
        }
        Ok(())
    }
}

fn decode_storage_len(
    bytes: &[u8],
    offset: usize,
    len_width: usize,
    elem_size: usize,
) -> Result<(usize, usize), DataLayoutError> {
    if elem_size == 0 {
        return Err(DataLayoutError::InvalidDataLength);
    }
    if bytes.len() == offset {
        return Ok((0, 0));
    }
    if bytes.len() < offset + len_width {
        return Err(DataLayoutError::MissingLengthHeader);
    }

    let capacity = storage_capacity(bytes.len(), offset, len_width, elem_size)?;
    let len = read_len_header(bytes, offset, len_width)?;
    if len > capacity {
        return Err(DataLayoutError::LengthExceedsCapacity);
    }

    Ok((len, capacity))
}

fn storage_capacity(
    storage_len: usize,
    offset: usize,
    len_width: usize,
    elem_size: usize,
) -> Result<usize, DataLayoutError> {
    if storage_len < offset + len_width {
        return Ok(0);
    }

    let available = storage_len - offset - len_width;
    if !available.is_multiple_of(elem_size) {
        return Err(DataLayoutError::InvalidDataLength);
    }

    Ok((available / elem_size).min(max_len(len_width)))
}

fn read_len_header(
    bytes: &[u8],
    offset: usize,
    len_width: usize,
) -> Result<usize, DataLayoutError> {
    let mut raw = [0u8; 8];
    raw[..len_width].copy_from_slice(&bytes[offset..offset + len_width]);
    let len = u64::from_le_bytes(raw);
    usize::try_from(len).map_err(|_| DataLayoutError::LengthExceedsCapacity)
}

fn write_len_header(
    bytes: &mut [u8],
    offset: usize,
    len_width: usize,
    len: usize,
) -> Result<(), DataLayoutError> {
    if len > max_len(len_width) {
        return Err(DataLayoutError::LengthExceedsCapacity);
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
