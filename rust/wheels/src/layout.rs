use crate::DataLayoutError;

pub trait DataLayoutKind {
    const IS_FIXED: bool;
}

pub trait Encodable {
    fn encode(&self) -> Result<alloc::vec::Vec<u8>, DataLayoutError>;

    ///
    /// Returns the unwritten buffer
    ///
    fn encode_to(&self, out: &mut [u8]) -> Result<&mut [u8], DataLayoutError>;
}

pub trait Decodable {
    type View<'a>;

    fn decode<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError>;

    ///
    /// Decodes a prefix and returns the remaining bytes
    ///
    fn decode_prefix<'a>(bytes: &'a [u8]) -> Result<(Self::View<'a>, &'a [u8]), DataLayoutError>;
}
