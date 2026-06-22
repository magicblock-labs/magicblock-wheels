use crate::DataLayoutError;

pub trait DataLayoutKind {
    const IS_FIXED: bool;
}

pub trait Encodable {
    fn encoded_len(&self) -> Result<usize, DataLayoutError>;

    fn encode(&self) -> Result<alloc::vec::Vec<u8>, DataLayoutError> {
        let mut bytes = ::alloc::vec![0; self.encoded_len()?];
        self.encode_to(&mut bytes)?;
        Ok(bytes)
    }

    ///
    /// Returns the unwritten buffer
    ///
    fn encode_to<'a>(&self, out: &'a mut [u8]) -> Result<&'a mut [u8], DataLayoutError>;
}

///
/// Exact-slice decoding for layouts that are not necessarily self-delimiting.
///
/// This is required for layouts with implicit `Option` fields, where option
/// presence is inferred from total encoded length. For example, with valid
/// lengths `[12, 44]`, a 12-byte `None` value plus 32 unrelated bytes is
/// indistinguishable from a 44-byte `Some` value unless the caller has already
/// framed the slice.
///
/// Such layouts implement this trait instead of `Decodable` so callers cannot
/// accidentally parse them as prefixes of a larger buffer.
///
pub trait ExactDecodable {
    type View<'a>;

    ///
    /// Decodes exactly one value from `bytes`.
    ///
    fn decode_exact<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError>;
}

///
/// Prefix decoding for self-delimiting layouts.
///
/// Implementors can decode one value from the front of a larger buffer.
///
pub trait Decodable {
    type View<'a>;

    ///
    /// Decodes the first self-delimiting value from `bytes`.
    ///
    fn decode<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError> {
        Ok(Self::decode_prefix(bytes)?.0)
    }

    ///
    /// Decodes a prefix and returns the decoded view plus unconsumed bytes.
    ///
    fn decode_prefix<'a>(bytes: &'a [u8]) -> Result<(Self::View<'a>, &'a [u8]), DataLayoutError>;
}

impl<T> ExactDecodable for T
where
    T: Decodable,
{
    type View<'a> = <T as Decodable>::View<'a>;

    fn decode_exact<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError> {
        let (view, remaining) = T::decode_prefix(bytes)?;
        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength);
        }
        Ok(view)
    }
}
