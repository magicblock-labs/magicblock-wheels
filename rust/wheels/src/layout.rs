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

macro_rules! impl_tuple_encodable {
    ($($name:ident),+ $(,)?) => {
        impl<$($name),+> Encodable for ($($name,)+)
        where
            $($name: Encodable,)+
        {
            fn encoded_len(&self) -> Result<usize, DataLayoutError> {
                #[allow(non_snake_case)]
                let ($($name,)+) = self;

                let mut len = 0usize;
                $(
                    len = len
                        .checked_add($name.encoded_len()?)
                        .ok_or(DataLayoutError::LengthExceedsCapacity)?;
                )+
                Ok(len)
            }

            fn encode_to<'a>(&self, out: &'a mut [u8]) -> Result<&'a mut [u8], DataLayoutError> {
                #[allow(non_snake_case)]
                let ($($name,)+) = self;

                $(
                    let out = $name.encode_to(out)?;
                )+
                Ok(out)
            }
        }
    };
}

impl_tuple_encodable!(A);
impl_tuple_encodable!(A, B);
impl_tuple_encodable!(A, B, C);

///
/// Exact-slice decoding.
///
/// Exact decoding is required for layouts with implicit `Option` fields, where option
/// presence is inferred from total encoded length. For example, with valid
/// lengths `[12, 44]`, a 12-byte `None` value plus 32 unrelated bytes is
/// indistinguishable from a 44-byte `Some` value unless the caller has already
/// framed the slice.
///
/// Layouts that can safely decode from the front of a larger buffer also
/// implement `PrefixDecodable`.
///
pub trait Decodable {
    type View<'a>;

    ///
    /// Decodes exactly one value from `bytes`.
    ///
    fn decode<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError>;
}

///
/// Prefix decoding for self-delimiting layouts.
///
/// Implementors can decode one value from the front of a larger buffer.
///
pub trait PrefixDecodable {
    type View<'a>;

    ///
    /// Decodes a prefix and returns the decoded view plus unconsumed bytes.
    ///
    fn decode_prefix<'a>(bytes: &'a [u8]) -> Result<(Self::View<'a>, &'a [u8]), DataLayoutError>;
}

impl<T> Decodable for T
where
    T: PrefixDecodable,
{
    type View<'a> = <T as PrefixDecodable>::View<'a>;

    fn decode<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError> {
        let (view, remaining) = T::decode_prefix(bytes)?;
        if !remaining.is_empty() {
            return Err(DataLayoutError::InvalidDataLength);
        }
        Ok(view)
    }
}
