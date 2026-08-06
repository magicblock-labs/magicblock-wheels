use super::{Decodable, FixedSizeLayout};
use crate::DataLayoutError;

/// Borrowed view over a `Vec<T>` field whose elements are fixed-size layouts.
///
/// Users do not write this type in their layout structs. They continue to write
/// `Vec<T>`. The macros return `LayoutSlice<'a, T>` from generated view getters
/// because encoded layout elements are stored as bytes and are decoded into
/// `T::View<'a>` on access.
///
/// # Why it exists
///
/// - Users continue to write `Vec<T>` in layout structs.
/// - Generated views cannot return `&[T]`, because encoded layout bytes are not
///   Rust `T` values.
/// - Generated views should still expose collection-like access through `len`,
///   `is_empty`, `get`, `iter`, and `as_bytes`.
/// - `LayoutSlice` adapts a `variable_offset_layout` Vec payload into borrowed
///   `T::View<'a>` values without allocation.
///
/// For library authors: this type models only the active payload bytes for a
/// `Vec<T>` inside `variable_offset_layout`, where there is no reserved capacity
/// beyond the encoded length. It intentionally does not model fixed-offset Vec
/// slots, because those have both a logical `len` and a schema-level `capacity`
/// with reserved trailing storage.
#[derive(Clone, Copy, Debug)]
pub struct LayoutSlice<'a, T>
where
    T: FixedSizeLayout,
{
    bytes: &'a [u8],
    _marker: core::marker::PhantomData<T>,
}

impl<'a, T> LayoutSlice<'a, T>
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

    pub fn iter(&self) -> LayoutSliceIter<'a, T> {
        LayoutSliceIter {
            bytes: self.bytes,
            _marker: core::marker::PhantomData,
        }
    }
}

pub struct LayoutSliceIter<'a, T>
where
    T: FixedSizeLayout,
{
    bytes: &'a [u8],
    _marker: core::marker::PhantomData<T>,
}

impl<'a, T> Iterator for LayoutSliceIter<'a, T>
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
