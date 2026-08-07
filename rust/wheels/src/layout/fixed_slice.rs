use super::{Decodable, FixedSizeLayout};
use crate::DataLayoutError;

///
/// Borrowed view over a `Vec<T>` field whose elements are fixed-size layouts.
///
/// Users do not write this type in their layout structs. They continue to write
/// `Vec<T>`. For supported scalar/POD element types, generated getters return
/// `&[T]`. For user-defined fixed-size layout element types, generated getters
/// return `FixedLayoutSlice<'a, T>` so callers can access each element as
/// `T::View<'a>`.
///
/// # Why it exists
///
/// - Users continue to write `Vec<T>` in layout structs.
/// - Generated views can return `&[T]` for supported scalar/POD element types,
///   but not for user-defined layout types whose public decoded form is
///   `T::View<'a>`.
/// - Generated views should still expose collection-like access through `len`,
///   `is_empty`, `get`, `iter`, and `as_bytes`.
/// - `FixedLayoutSlice` adapts generated Vec payloads into borrowed
///   `T::View<'a>` values without allocation.
/// - Variable-size element layouts use `VariableLayoutSlice` instead.
///
/// A `FixedLayoutSlice` contains only the active Vec payload: exactly
/// `len * T::DATA_LEN` bytes. For fixed-capacity Vec fields in
/// `fixed_offset_layout`, generated getters slice out only the active elements;
/// reserved trailing storage remains part of the parent layout and is exposed
/// separately through the generated `<field>_capacity()` method.
///
#[derive(Clone, Copy, Debug)]
pub struct FixedLayoutSlice<'a, T>
where
    T: FixedSizeLayout,
{
    bytes: &'a [u8],
    _marker: core::marker::PhantomData<T>,
}

impl<'a, T> FixedLayoutSlice<'a, T>
where
    T: FixedSizeLayout,
{
    pub fn new(bytes: &'a [u8]) -> Result<Self, DataLayoutError> {
        if T::DATA_LEN == 0 || bytes.len() % T::DATA_LEN != 0 {
            return Err(DataLayoutError::InvalidDataLength);
        }

        for chunk in bytes.chunks_exact(T::DATA_LEN) {
            <T as Decodable>::decode(chunk)?;
        }

        Ok(Self {
            bytes,
            _marker: core::marker::PhantomData,
        })
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len() / T::DATA_LEN
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<T::View<'a>> {
        if index >= self.len() {
            return None;
        }

        let start = index * T::DATA_LEN;
        let end = start + T::DATA_LEN;
        Some(
            <T as Decodable>::decode(&self.bytes[start..end])
                .expect("validated fixed-size layout element"),
        )
    }

    pub fn iter(&self) -> FixedLayoutSliceIter<'a, T> {
        FixedLayoutSliceIter {
            bytes: self.bytes,
            _marker: core::marker::PhantomData,
        }
    }
}

pub struct FixedLayoutSliceIter<'a, T>
where
    T: FixedSizeLayout,
{
    bytes: &'a [u8],
    _marker: core::marker::PhantomData<T>,
}

impl<'a, T> Iterator for FixedLayoutSliceIter<'a, T>
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
        Some(<T as Decodable>::decode(element).expect("validated fixed-size layout element"))
    }
}
