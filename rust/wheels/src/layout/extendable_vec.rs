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
/// The field stores its count and capacity in two `N`-byte little-endian headers.
/// Canonical `encode()` sets capacity to the count. A storage growth policy can
/// reserve more slots. After reopening, `len()` counts active elements and
/// `capacity()` counts all slots belonging to the field.
///
/// Each slot must occupy at least one byte.
///
/// ```compile_fail,E0080
/// # extern crate alloc;
/// use wheels::fixed_offset_layout;
/// #[fixed_offset_layout(buffer_offset = any)]
/// struct Record {
///     #[extendable = 1]
///     items: Vec<[u8; 0]>,
/// }
/// ```
///
/// The same rule applies to the encoded size of a nested layout.
///
/// ```compile_fail,E0080
/// # extern crate alloc;
/// use wheels::fixed_offset_layout;
/// #[fixed_offset_layout(buffer_offset = any)]
/// struct Empty {}
/// #[fixed_offset_layout(buffer_offset = any)]
/// struct Record {
///     #[extendable = 1]
///     entries: Vec<Empty>,
/// }
/// ```
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
        let (len, capacity) = decode_extendable_vec_header(bytes, offset, len_width, elem_size)?;
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
        let (len, capacity) = decode_extendable_vec_header(bytes, offset, len_width, elem_size)?;
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
        let (len, capacity) = decode_extendable_vec_header(storage, offset, len_width, elem_size)
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

    /// Size of the compact encoding, including both headers and the active elements.
    /// Use [`Self::storage_len`] for the field's size including spare slots.
    pub fn encoded_len(&self) -> usize {
        2 * self.len_width + self.len * self.elem_size
    }

    /// Bytes occupied by both headers and all reserved element slots.
    /// With two 2-byte headers and four `u16` slots, this is `2 + 2 + 4 * 2 = 12`.
    /// Clearing the vector keeps its capacity and storage length.
    pub fn storage_len(&self) -> usize {
        2 * self.len_width + self.capacity * self.elem_size
    }

    fn data_start(&self) -> usize {
        self.offset + 2 * self.len_width
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

    /// Returns both headers and all slots belonging to this field.
    pub fn storage_bytes(&self) -> &'a [u8] {
        &self.all_bytes()[self.offset..self.offset + self.storage_len()]
    }

    /// Returns the active Vec payload bytes, excluding both headers.
    pub fn active_bytes(&self) -> &'a [u8] {
        &self.all_bytes()[self.active_range()]
    }
}

impl<'a, T> ExtendableVec<'a, T, (), FixedValueElement>
where
    T: Pod,
{
    /// Returns the active elements as a borrowed slice.
    pub fn as_slice(&self) -> &'a [T] {
        if self.is_empty() {
            return &[];
        }
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
        if len <= self.capacity {
            return Ok(());
        }
        let max_spare_elements = max_len(self.len_width)
            .checked_sub(len)
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        let elem_size = self.elem_size;
        let old_len = self.storage().len();
        let required = (len - self.capacity)
            .checked_mul(elem_size)
            .and_then(|extra| old_len.checked_add(extra))
            .ok_or(DataLayoutError::LengthExceedsCapacity)?;
        let suffix_start = self.offset + self.storage_len();
        let capacity_offset = self.offset + self.len_width;
        let width = self.len_width;
        let old_capacity = self.capacity;
        let storage = self.storage_mut();
        storage.grow(required, elem_size, max_spare_elements)?;

        let added = storage.len() - old_len;
        let capacity = old_capacity + added / elem_size;
        if suffix_start < old_len {
            storage.copy_within(suffix_start..old_len, suffix_start + added);
            storage[suffix_start..suffix_start + added].fill(0);
        }
        write_len_header(storage, capacity_offset, width, capacity)?;
        self.capacity = capacity;
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

/// Reads the count and capacity of an extendable field and checks that all slots fit.
#[doc(hidden)]
pub fn decode_extendable_vec_header(
    bytes: &[u8],
    offset: usize,
    len_width: usize,
    elem_size: usize,
) -> Result<(usize, usize), DataLayoutError> {
    if !(1..=8).contains(&len_width) || elem_size == 0 {
        return Err(DataLayoutError::InvalidDataLength);
    }
    let (len_header, rest) = bytes
        .get(offset..)
        .and_then(|field| field.split_at_checked(len_width))
        .ok_or(DataLayoutError::MissingLengthHeader)?;
    let (capacity_header, payload) = rest
        .split_at_checked(len_width)
        .ok_or(DataLayoutError::MissingCapacityHeader)?;
    let len = read_len_header(len_header)?;
    let capacity = read_len_header(capacity_header)?;
    if len > capacity || capacity > max_len(len_width) {
        return Err(DataLayoutError::LengthExceedsCapacity);
    }
    if capacity > payload.len() / elem_size {
        return Err(DataLayoutError::TruncatedVectorPayload);
    }
    Ok((len, capacity))
}

fn read_len_header(bytes: &[u8]) -> Result<usize, DataLayoutError> {
    let mut raw = [0u8; 8];
    raw[..bytes.len()].copy_from_slice(bytes);
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
