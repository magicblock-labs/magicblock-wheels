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

pub trait Decodable {
    type View<'a>;

    ///
    /// Decodes the first value from bytes, ignoring any trailing bytes.
    ///
    fn decode<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError> {
        Ok(Self::decode_prefix(bytes)?.0)
    }

    ///
    /// Decodes one value and fails if any trailing bytes remain.
    ///
    fn decode_exact<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError> {
        let (view, remaining) = Self::decode_prefix(bytes)?;
        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength);
        }
        Ok(view)
    }

    ///
    /// Decodes a prefix and returns the decoded view plus unconsumed bytes.
    ///
    fn decode_prefix<'a>(bytes: &'a [u8]) -> Result<(Self::View<'a>, &'a [u8]), DataLayoutError>;
}
