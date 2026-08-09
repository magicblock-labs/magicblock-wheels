//! Layout traits, generated view helpers, and layout-selection guidance.
//!
//! Most users interact with this module by importing the generated-layout
//! traits:
//!
//! ```ignore
//! use wheels::layout::{Decodable, Encodable, PrefixDecodable};
//! ```
//!
//! Layout structs are still written with ordinary Rust field types such as
//! `Vec<T>` and `Option<T>`. Helper view types like [`FixedLayoutSlice`] and
//! [`VariableLayoutSlice`] appear only in generated view getters. For a final
//! flexible Vec in `fixed_offset_layout`, generated getters return
//! [`FlexibleVec`] so callers can see both active `len` and backing storage
//! `capacity`.
//!
//! # Why Layouts Exist
//!
//! Layouts are not intended to be a better general-purpose serializer than
//! Borsh or bincode. Those libraries serialize Rust values into bytes and
//! deserialize bytes back into Rust values. Wheels layouts are for cases where
//! the encoded bytes are themselves the data structure: validated storage or
//! argument layouts with generated borrowed views.
//!
//! The clearest example is account storage. Borsh and bincode do not model
//! "this `Vec` has capacity 72 in the account, but only len 5 is active." That
//! is not ordinary serialization anymore; it is storage layout. A layout can
//! reserve schema-level capacity, expose the active length, and still let code
//! inspect the encoded account bytes without rebuilding an owned value.
//!
//! The main design goal is CU-friendly access to encoded on-chain data. To
//! achieve that, layouts prioritize:
//!
//! - **zero-alloc** decode;
//! - practically **zero-copy** access, copying only small scalar values where a
//!   copy is cheaper and simpler than a reference;
//! - stable field offsets for account/state data, so fixed-position fields can
//!   be accessed, validated, and updated directly from their schema offsets
//!   while any flexible payload is kept at the end. This applies specifically
//!   to [`fixed_offset_layout!`](crate::fixed_offset_layout);
//! - explicit low-level choices for buffer offset, capacity, length-header
//!   width, alignment, and fixed-size versus variable-size nested elements;
//! - compact encodings for on-chain payloads where every byte and compute unit
//!   matters.
//!
//! This comes with a real cost: layouts expose more low-level decisions than
//! Borsh or bincode. That is intentional. When compute units, storage size, or
//! account layout semantics matter, the macro asks users to choose explicitly
//! instead of relying on hidden defaults. For ordinary serialization, prefer
//! Borsh or bincode.
//!
//! # Supported Types
//!
//! Both layout macros support:
//!
//! - `bool`
//! - `i8`, `u8`, `i16`, `u16`, `i32`, `u32`, `i64`, `u64`, `i128`, `u128`, `isize`, `usize`
//! - `[T; N]`, where `T` is one of those integer primitives
//! - [`Pubkey`](crate::Pubkey)/`Address`
//! - `Option<T>` for supported fixed-value types
//! - `Vec<T>` for supported fixed-value element types except `bool`
//!
//! The following shapes are intentionally not supported:
//!
//! - `String`
//! - `Vec<bool>`
//! - `Option<Vec<T>>`
//!
//! [`fixed_offset_layout!`](crate::fixed_offset_layout) also supports `Vec<T>`
//! where `T` is a user-defined fixed-size layout type. The generated getter
//! returns [`FixedLayoutSlice`] for fixed-capacity fields or [`FlexibleVec`]
//! for a final flexible Vec field.
//!
//! [`variable_offset_layout!`](crate::variable_offset_layout) supports `Vec<T>`
//! where `T` is a user-defined layout type. Those fields must choose
//! `#[element_size = fixed]` or `#[element_size = variable]` explicitly.
//!
//! # Choosing a Layout Macro
//!
//! Wheels provides two layout macros:
//!
//! - [`fixed_offset_layout!`](crate::fixed_offset_layout), for stable field
//!   offsets and reserved storage.
//! - [`variable_offset_layout!`](crate::variable_offset_layout), for compact
//!   encodings where later field offsets may depend on earlier field lengths.
//!
//! | Topic | `fixed_offset_layout` | `variable_offset_layout` |
//! |---|---|---|
//! | Best fit | Account/state data, reserved slots, schemas that benefit from stable field starts. | Instruction args, compact records, data where avoiding reserved bytes matters. |
//! | Field offsets | Field starts are compile-time offsets, except total length may vary when the final field is flexible. | Offsets after a variable-size field are computed from encoded lengths at decode time. |
//! | Encoded size | Constant-size layouts expose `DATA_LEN`; trailing-flexible layouts expose `MIN_DATA_LEN` and `MAX_DATA_LEN`. | Fixed-size layouts expose `DATA_LEN`; finite optional-size layouts expose `DATA_LENS`; Vec layouts expose `DATA_LEN_RANGE`. |
//! | Normal `Option<T>` | Encodes a 1-byte tag plus payload when present. | Encodes a 1-byte tag plus payload when present. |
//! | Flexible `Option<T>` | The final field may use `#[flexible]`; `None` omits the field entirely and `Some` writes tag plus payload. | Not supported as a field attribute. Use normal tagged options, or `option = implicit` for eligible compatibility types. |
//! | Implicit `Option<T>` | Not supported. | ⚠️ Backward compatibility only; avoid for new types. The struct-level `option = implicit` mode omits option tags and saves one byte per `Option<T>`, but is allowed only when there are no Vec fields and option presence can be inferred unambiguously from total encoded length. |
//! | `Vec<T>` of supported scalar/key types | Uses `#[capacity = N]` to reserve space for `N` elements. Views expose active `len` and a `<field>_capacity()` method. | Uses `#[flexible = N]` to encode only active elements. Views return borrowed slices like `&[T]`. |
//! | Flexible `Vec<T>` | Allowed only as the final field with `#[flexible = 1]` or `#[flexible = 2]`. Canonical `encode()` writes only active bytes. `decode()` may receive larger storage and exposes spare trailing bytes as [`FlexibleVec::capacity`]. | Every Vec uses `#[flexible = N]`; Vec fields can appear before later fields. `N` can be `1..=8`. No capacity is modeled; only active encoded elements exist. |
//! | `Vec<T>` of user-defined layout types | Requires `T: FixedSizeLayout`; uses `#[capacity = N]` or final `#[flexible = 1]`/`#[flexible = 2]`. Fixed-capacity getters return [`FixedLayoutSlice`]; final flexible getters return [`FlexibleVec`]. | Requires `#[element_size = fixed]` or `#[element_size = variable]`. Fixed elements return [`FixedLayoutSlice`]; variable elements return [`VariableLayoutSlice`]. |
//! | Prefix decoding | Constant-size layouts implement [`PrefixDecodable`]. Trailing-flexible layouts implement [`Decodable`]; final flexible Vec decodes the supplied storage region, while final flexible Option remains exactly framed. | Normal layouts implement [`PrefixDecodable`]. Layouts using `option = implicit` need exact framing and implement [`Decodable`]. |
//! | Alignment | Use `buffer_offset = 0..=7` when the input starts at a known offset from an 8-byte aligned base. Use `buffer_offset = unaligned` only when generated views do not borrow alignment-sensitive fields. | Use `buffer_offset = 0..=7` when the input starts at a known offset from an 8-byte aligned base; use `buffer_offset = unaligned` only when generated views do not borrow alignment-sensitive fields. |
//!
//! # Practical Guidelines
//!
//! Use [`fixed_offset_layout!`](crate::fixed_offset_layout) when the encoded
//! data is long-lived state and stable offsets are useful. This is the better
//! fit when a `Vec<T>` should have schema-level capacity, because the layout
//! reserves storage even when the active length is smaller.
//! User-defined `Vec<T>` elements in `fixed_offset_layout` must be fixed-size
//! layouts and do not use `#[element_size = ...]`.
//! A final `#[flexible = N] Vec<T>` is the exception to fixed-size total
//! storage: canonical `encode()` stores only active elements, but `decode()`
//! can view an account/storage buffer with spare trailing bytes. The generated
//! getter returns [`FlexibleVec`], which reports both `len()` and `capacity()`.
//! For mutation, generated `decode_mut(storage)` works with
//! [`LayoutStorageMut`] and exposes `<field>_mut()` for Vec-like operations.
//!
//! Use [`variable_offset_layout!`](crate::variable_offset_layout) when compact
//! encoding matters more than stable offsets. This is usually the better fit
//! for instruction arguments, payloads with multiple Vec fields, and nested
//! user-defined layout elements.
//!
//! For user-defined `Vec<T>` elements inside `variable_offset_layout`, make the
//! element-size decision explicit:
//!
//! ```ignore
//! #[flexible = 1]
//! #[element_size = fixed]
//! entries: Vec<FixedEntry>,
//!
//! #[flexible = 1]
//! #[element_size = variable]
//! entries: Vec<VariableEntry>,
//! ```
//!
//! `fixed` means each element has a constant encoded width and the generated
//! getter returns [`FixedLayoutSlice`]. `variable` means each element is
//! self-delimiting through [`PrefixDecodable`] and the generated getter returns
//! [`VariableLayoutSlice`].
//!
//! Avoid `option = implicit` for new types. It exists for backward compatibility
//! with older compact encodings. Because it omits option tags, decoding depends
//! on the total encoded length. That is why it is not available with Vec fields
//! and why the macro rejects ambiguous option-size combinations.
//!
use crate::DataLayoutError;

mod fixed_slice;
mod flexible_vec;
mod storage;
mod variable_slice;

pub use fixed_slice::{FixedLayoutSlice, FixedLayoutSliceIter};
pub use flexible_vec::{
    FixedLayoutElement, FixedLayoutFlexibleVecIter, FixedValueElement, FlexibleVec,
};
pub use storage::{LayoutStorage, LayoutStorageMut};
pub use variable_slice::{VariableLayoutSlice, VariableLayoutSliceIter};

pub trait DataLayoutKind {
    const IS_FIXED: bool;
}

pub trait FixedSizeLayout: Encodable + Decodable {
    const DATA_LEN: usize;
}

///
/// Compile-time encoded length bounds for generated layouts.
///
/// `LayoutBounds` lets a parent layout reserve and validate space for nested
/// prefix-decodable elements whose exact encoded size is known only at runtime.
///
pub trait LayoutBounds {
    const MIN_DATA_LEN: usize;
    const MAX_DATA_LEN: usize;
}

impl<T> LayoutBounds for T
where
    T: FixedSizeLayout,
{
    const MIN_DATA_LEN: usize = T::DATA_LEN;
    const MAX_DATA_LEN: usize = T::DATA_LEN;
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
/// `Option<T>` exact decoding treats an empty slice as `None` and a non-empty
/// slice as `Some(T::decode(bytes)?)`. This is useful for implicitly encoded
/// options whose absence is known only from the caller's framing.
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

impl<T> Decodable for Option<T>
where
    T: Decodable,
{
    type View<'a> = Option<T::View<'a>>;

    fn decode<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, DataLayoutError> {
        if bytes.is_empty() {
            Ok(None)
        } else {
            T::decode(bytes).map(Some)
        }
    }
}
