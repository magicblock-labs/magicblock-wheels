use super::PrefixDecodable;
use crate::DataLayoutError;

///
/// Borrowed view over a `Vec<T>` field whose elements are variable-size
/// layouts.
///
/// Users do not write this type in their layout structs. They continue to write
/// `Vec<T>`, and generated view getters return `VariableLayoutSlice<'a, T>` for
/// user-defined layout element types marked with `#[element_size = variable]`.
///
/// Unlike `FixedLayoutSlice`, this type does not require elements to have a
/// fixed encoded width. It validates and walks elements sequentially, using the
/// encoded Vec length header supplied by the parent layout.
///
/// # Tradeoffs
///
/// - `iter` is sequential and allocation-free.
/// - `len` is cheap because the parent layout passes in the encoded Vec length.
/// - `get(index)` is O(n), because variable-width elements do not have a
///   constant stride.
/// - Exact-only layouts are not supported; elements must implement
///   `PrefixDecodable` so their boundaries are self-delimiting.
///
#[derive(Clone, Copy, Debug)]
pub struct VariableLayoutSlice<'a, T>
where
    T: PrefixDecodable,
{
    bytes: &'a [u8],
    len: usize,
    _marker: core::marker::PhantomData<T>,
}

impl<'a, T> VariableLayoutSlice<'a, T>
where
    T: PrefixDecodable,
{
    pub fn new(bytes: &'a [u8], len: usize) -> Result<Self, DataLayoutError> {
        let mut remaining = bytes;

        for _ in 0..len {
            let before_len = remaining.len();
            let (_, next) = T::decode_prefix(remaining)?;
            if next.len() >= before_len {
                return Err(DataLayoutError::InvalidDataLength);
            }
            remaining = next;
        }

        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength);
        }

        Ok(Self {
            bytes,
            len,
            _marker: core::marker::PhantomData,
        })
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    ///
    /// Returns the item at `index`.
    ///
    /// This is O(n), because prefix-decodable elements do not have a fixed
    /// encoded stride.
    ///
    pub fn get(&self, index: usize) -> Option<T::View<'a>> {
        if index >= self.len {
            return None;
        }

        self.iter().nth(index)
    }

    pub fn iter(&self) -> VariableLayoutSliceIter<'a, T> {
        VariableLayoutSliceIter {
            bytes: self.bytes,
            remaining_len: self.len,
            _marker: core::marker::PhantomData,
        }
    }
}

pub struct VariableLayoutSliceIter<'a, T>
where
    T: PrefixDecodable,
{
    bytes: &'a [u8],
    remaining_len: usize,
    _marker: core::marker::PhantomData<T>,
}

impl<'a, T> Iterator for VariableLayoutSliceIter<'a, T>
where
    T: PrefixDecodable,
{
    type Item = T::View<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_len == 0 {
            return None;
        }

        let before_len = self.bytes.len();
        let (item, remaining) =
            T::decode_prefix(self.bytes).expect("validated prefix layout element");
        if remaining.len() >= before_len {
            panic!("validated prefix layout element consumed no bytes");
        }

        self.bytes = remaining;
        self.remaining_len -= 1;
        Some(item)
    }
}
