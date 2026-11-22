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
//! extendable Vec in `fixed_offset_layout`, generated getters return
//! [`ExtendableVec`] so callers can see both active `len` and backing storage
//! `capacity`. Generated mutable views use helper types like
//! [`LayoutCopyMut`] and [`FixedCapacityVec`] so account storage can be
//! updated without rebuilding an owned value.
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
//! - tagged `Option<T>` containing a user-defined layout
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
//! returns [`FixedLayoutSlice`] for fixed-capacity fields or [`ExtendableVec`]
//! for a final extendable Vec field. A user-defined `Option<T>` must be the
//! final field. A normal option requires `T: FixedSizeLayout`. With
//! `#[flexible]`, the child must implement [`PrefixDecodable`] so it can
//! identify the end of its payload. This rules out children using
//! `option = implicit`, which infer presence from the supplied slice length.
//! An absent flexible option occupies one byte for its zero tag.
//!
//! [`variable_offset_layout!`](crate::variable_offset_layout) supports `Vec<T>`
//! where `T` is a user-defined layout type. Those fields must choose
//! `#[element_size = fixed]` or `#[element_size = variable]` explicitly.
//! User-defined `Option<T>` fields use tagged encoding and require
//! `T: FixedSizeLayout`; they are not supported with `option = implicit`.
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
//! | Field offsets | Field starts are compile-time offsets, except total length may vary when the final field is variable-length. | Offsets after a variable-size field are computed from encoded lengths at decode time. |
//! | Encoded size | Constant-size layouts expose `DATA_LEN`; trailing variable-length layouts expose `MIN_DATA_LEN` and `MAX_DATA_LEN`. | Fixed-size layouts expose `DATA_LEN`; finite optional-size layouts expose `DATA_LENS`; Vec layouts expose `DATA_LEN_RANGE`. |
//! | Normal `Option<T>` | Encodes a 1-byte tag plus payload when present. | Encodes a 1-byte tag plus payload when present. |
//! | Flexible `Option<T>` | The final field may use `#[flexible]`; `None` writes `00` and `Some` writes `01` followed by the payload. | Not supported as a field attribute. Use normal tagged options, or `option = implicit` for eligible compatibility types. |
//! | Implicit `Option<T>` | Not supported. | ⚠️ Backward compatibility only; avoid for new types. The struct-level `option = implicit` mode omits option tags and saves one byte per `Option<T>`, but is allowed only when there are no Vec fields and option presence can be inferred unambiguously from total encoded length. |
//! | `Vec<T>` of supported scalar/key types | Uses `#[capacity = N]` to reserve space for `N` elements. Views expose active `len` and a `<field>_capacity()` method. | Uses `#[flexible = N]` to encode only active elements. Views return borrowed slices like `&[T]`. |
//! | Extendable `Vec<T>` | Allowed only as the final field with `#[extendable = N]`, where `N` can be `1..=8`. Stores the count and capacity, followed by the reserved slots. Canonical `encode()` sets capacity to the count. | Not supported. Every Vec uses `#[flexible = N]` and models only active encoded elements. |
//! | `Vec<T>` of user-defined layout types | Requires `T: FixedSizeLayout`; uses `#[capacity = N]` or final `#[extendable = N]`. Fixed-capacity getters return [`FixedLayoutSlice`]; final extendable getters return [`ExtendableVec`]. | Requires `#[element_size = fixed]` or `#[element_size = variable]`. Fixed elements return [`FixedLayoutSlice`]; variable elements return [`VariableLayoutSlice`]. |
//! | `Option<T>` of a user-defined layout | Must be the final field. A normal option requires `T: FixedSizeLayout` and reserves a tag plus `T::DATA_LEN` bytes. With `#[flexible]`, `T: Encodable + PrefixDecodable + LayoutBounds` may have variable size. `None` occupies one tag byte and `Some` adds the encoded payload. | Requires `T: FixedSizeLayout`; uses tagged encoding and dynamically advances later field offsets. It is not supported with `option = implicit`. |
//! | Mutation | `decode_mut(storage)` is generated for every fixed layout. Fixed-capacity Vec fields return [`FixedCapacityVec`] and can grow active `len` up to schema capacity without resizing storage. Final extendable Vec fields return [`ExtendableVec`] and may resize storage. | Not currently generated. |
//! | Prefix decoding | All fixed-offset layouts implement [`PrefixDecodable`]. Use `decode_prefix()` to read one record and return the remaining bytes. | Normal layouts implement [`PrefixDecodable`]. Layouts using `option = implicit` need exact framing and implement [`Decodable`]. |
//! | [Alignment Requirement](#alignment-requirement) | Check the address of the borrowed payload at its fixed offset. | Check every position the borrowed payload can occupy as earlier fields change length. |
//!
//! # Attribute Reference
//!
//! | Attribute | `fixed_offset_layout` | `variable_offset_layout` | Meaning |
//! |---|---|---|---|
//! | `buffer_offset = 0..=7` | Struct-level; required. | Struct-level; required. | Declares the known byte offset of the decoded slice from an 8-byte aligned base. The decoder validates that pointer offset, and the macro uses it to decide which borrowed views are alignment-safe. |
//! | `buffer_offset = any` | Struct-level; required alternative. | Struct-level; required alternative. | Use when the slice can start at any address. The decoder skips pointer-offset checking, and the macro rejects borrowed views that need alignment greater than 1. |
//! | `option = implicit` | Not supported. | Struct-level; optional. | Backward-compatibility mode for structs without Vec fields. It omits option tags and infers option presence from the total encoded length. |
//! | `#[capacity = N]` | Field-level; fixed-capacity `Vec<T>` only. | Not supported. | Reserves `N` slots, where `N` must be in `1..=65535`. The active length can range from zero to `N` without resizing storage. |
//! | `#[extendable = N]` | Field-level; final `Vec<T>` only. | Not supported. | Stores the count and capacity in two `N`-byte headers (`1..=8`). Both headers are present for an empty vector. |
//! | `#[flexible]` | Field-level; final `Option<T>` only. | Not supported. | Stores a single `00` tag for `None` or `01` followed by the payload for `Some`. |
//! | `#[flexible = N]` | Not supported; fixed trailing Vecs use `#[extendable = N]`. | Field-level; every `Vec<T>` field. | Uses an `N`-byte length header (`1..=8`) and encodes only active elements. `N` is length-header width, not capacity, and later field offsets are decoded dynamically. |
//! | `#[element_size = fixed]` | Not supported. | Field-level; user-defined `Vec<T>` element types only. | Declares that each nested layout element has constant encoded width, so generated getters can return [`FixedLayoutSlice`]. |
//! | `#[element_size = variable]` | Not supported. | Field-level; user-defined `Vec<T>` element types only. | Declares that nested layout elements are prefix-decodable variable-size values, so generated getters return [`VariableLayoutSlice`]. |
//!
//! ## What is the difference between `#[extendable = N]` and `#[flexible = N]` on a Vec?
//!
//! Both write the element count in `N` little-endian bytes, including a zero count.
//!
//! | `#[extendable = N]` | `#[flexible = N]` |
//! |---|---|
//! | Must be the **last field** of a fixed-offset struct. | Can appear **anywhere** in a variable-offset struct. |
//! | Stores capacity in a second `N`-byte header. The next record starts after all `capacity` slots. | Stores only the count. The next field starts after `count` elements. |
//! | A mutable view can append in place and grow storage when the reserved slots fill. | To append, update the owned vector and re-encode the struct. |
//!
//! For a flexible vector followed by another field, the layout is
//! `[vec_field: [count][items]][next_field]`. The decoder uses the count to locate `next_field`.
//!
//! For an extendable vector in shared storage:
//! `[vec_field: [count = 3][capacity = 8][3 items][5 spare slots]][next_record]`.
//! Five pushes leave the next record at the same offset. The sixth grows storage
//! and moves the following bytes once. The extra capacity header costs `N` bytes,
//! and each spare slot occupies one encoded element. Use `decode_prefix()` to
//! locate the next record and `decode_prefix_mut(storage)` to mutate the first.
//! If later records require a specific `buffer_offset`, choose slot counts and
//! growth steps that preserve their alignment when the records move.
//!
//! # Alignment Requirement
//!
//! | `buffer_offset` | When to use it |
//! |---|---|
//! | `any` | The struct can start anywhere. Its getters copy values or borrow types with alignment 1.<ul><li>`threshold: u16`</li><li>`authority: Pubkey`</li></ul> |
//! | `0` through `7` | Use the struct's starting address modulo 8 to guarantee alignment. This also enables [direct mutation](#mutability) of aligned fields in fixed layouts. The following borrowed fields require a numeric offset.<ul><li>`items: Vec<u16>`</li><li>`counters: [u32; 3]`</li></ul> |
//!
//! <details style="border-inline-start: 4px solid #548ed0; background-color: rgba(84, 142, 208, 0.1); padding: 0.75rem 1rem;">
//! <summary><strong><span aria-hidden="true">ⓘ</span> How to check alignment</strong></summary>
//!
//! A borrowed `T` must start at a multiple of `align_of::<T>()`. For every
//! possible payload position, `payload_offset` counts all preceding bytes in
//! the struct, including tags, counts, and capacity headers.
//!
//! `(buffer_offset + payload_offset) % align_of::<T>() == 0`
//!
//! - **Borrowing a slice.** With a one-byte tag and two eight-byte vector headers,
//!   a struct starting at byte 1 puts the first `u16` at byte `1 + 1 + 16 = 18`
//!   (even, aligned). Starting at byte 0 puts it at byte 17 (odd, misaligned).
//! - **Packing nested structs.** Two 39-byte entries starting at bytes 8 and 47
//!   can both use `any` if their fields allow it. A child declaring
//!   `buffer_offset = 0` can start at byte 8. A fixed-offset parent reserving
//!   both slots fails to compile because byte 47 has offset 7.
//!
//! </details>
//!
//! A nested layout's encoded width also contributes to the next field's offset.
//! Here the count and the single reserved byte put `counters` at byte 2.
//! Insert two padding bytes before `counters` to place it at byte 4.
//!
//! ```compile_fail,E0080
//! # extern crate alloc;
//! use wheels::fixed_offset_layout;
//! #[fixed_offset_layout(buffer_offset = any)]
//! struct Entry { value: u8 }
//! #[fixed_offset_layout(buffer_offset = 0)]
//! struct Record {
//!     #[capacity = 1]
//!     entries: Vec<Entry>,
//!     counters: [u32; 4],
//! }
//! ```
//!
//! ## Consecutive tagged options
//!
//! In a `variable_offset_layout`, each tagged option takes one byte for its
//! tag and adds its payload only for `Some`. For two adjacent options, use
//! the getters to determine which payloads need an aligned address.
//!
//! | First and second payload | Placement rule |
//! |---|---|
//! | Both copied | Any address works for these two fields, e.g. `Option<u64>` followed by `Option<u32>`. |
//! | Borrowed, then copied | Align the first payload. The second can start at any address. |
//! | Copied, then borrowed | Align the second payload with the first option set to `None`. The first payload's encoded size must be a multiple of the second payload's alignment so `Some` also works. |
//! | Both borrowed | If both require alignment greater than one, they cannot be adjacent. Treat an alignment-one borrow like a copy when applying the other rows. |
//!
//! For a nested layout, check its declared `buffer_offset`. A numeric offset
//! must match the child's starting address modulo eight for every possible
//! encoding of the preceding fields. A child declaring `any` accepts every address.
//!
//! <details style="border-inline-start: 4px solid #548ed0; background-color: rgba(84, 142, 208, 0.1); padding: 0.75rem 1rem;">
//! <summary><strong><span aria-hidden="true">ⓘ</span> Why the one-byte tag matters</strong></summary>
//!
//! Let `p` be the first tag's address. When the first option is `Some`, its
//! payload starts at `p + 1`. When it is `None` and the second option is
//! `Some`, the second payload starts at `p + 2`. One of these addresses is
//! odd. If both payloads need even addresses, one of those encodings fails.
//!
//! Now consider a parent at byte 0 with `meta: Option<Meta>` followed by
//! `leaf: Option<Leaf>`. When `leaf` is present, the two tags put its payload at
//! byte 2 if `meta` is absent, or at `2 + Meta::DATA_LEN` if `meta` is present.
//! An eight-byte `Meta` gives starts of 2 and 10. Both satisfy the leaf's
//! declared `buffer_offset = 2`. A seven-byte `Meta` gives starts of 2 and 9,
//! so the following declaration fails at `leaf`.
//!
//! ```compile_fail,E0080
//! # extern crate alloc;
//! use wheels::{fixed_offset_layout, variable_offset_layout};
//! #[fixed_offset_layout(buffer_offset = any)]
//! struct Meta { stamp: [u8; 7] }
//! #[fixed_offset_layout(buffer_offset = 2)]
//! struct Leaf { value: u8 }
//! #[variable_offset_layout(buffer_offset = 0)]
//! struct Parent { meta: Option<Meta>, leaf: Option<Leaf> }
//! ```
//!
//! </details>
//!
//! # Mutability
//!
//! Mutability is supported only by
//! [`fixed_offset_layout!`](crate::fixed_offset_layout). Every fixed-offset
//! layout generates `decode_mut(storage)`, where `storage` implements
//! [`LayoutStorageMut`]. This is designed for account/state storage: fixed
//! fields can be updated in place, fixed-capacity Vec fields can change active
//! `len` within reserved storage, and trailing extendable fields can resize the
//! backing storage when their encoded length changes.
//!
//! `decode_mut` takes one exclusive borrow and validates its bytes. Each
//! `<field>_mut()` borrows from that view. After the last use of a field handle,
//! you can edit another field or resize a trailing field through the same view.
//! Use `view.bytes()` to inspect the buffer, or drop the view to borrow the
//! storage independently. A failed resize leaves the view usable.
//!
//! | Field form in `fixed_offset_layout` | Generated mutable helper | Mutation behavior |
//! |---|---|---|
//! | Integer, [`Pubkey`](crate::Pubkey)/`Address`, or fixed-size integer array with guaranteed alignment | `&mut T` | A numeric `buffer_offset` allows direct writes when the macro can prove the field is aligned. Types with alignment 1, such as `u8` and `Pubkey`, also allow direct writes with `any`.<br><br>`*view.shuttle_id_mut() = 101;` |
//! | Copyable integer or fixed-size integer array whose alignment cannot be guaranteed | [`LayoutCopyMut`] | With `any`, a field like `shuttle_id: u32` uses `get()` and `set(value)` to copy its bytes. A numeric offset also uses this wrapper if the field's position does not guarantee alignment.<br><br>`view.shuttle_id_mut().set(101);` |
//! | `bool` | [`LayoutBoolMut`] | Exposes `get()` and `set(bool)` because the encoded form is a backing `u8`.<br><br>`view.enabled_mut().set(true);` |
//! | Fixed `Option<T>` | [`LayoutOptionMut`] or [`LayoutBoolOptionMut`] | Updates the tag and payload in place; the reserved option slot size does not change.<br><br>`view.authority_mut().set(Some(key));` |
//! | Trailing flexible `Option<T>` | [`LayoutOptionMut`] or [`LayoutBoolOptionMut`] | `None` resizes storage to the end of the zero tag. `Some` resizes it to fit the tag and payload.<br><br>`view.delegate_mut().set(None)?;` |
//! | Final `Option<T>` with a nested layout | [`FixedLayoutOptionMut`] or [`FlexibleLayoutOptionMut`] | Encodes a replacement nested value directly into storage. A normal option retains its reserved payload bytes. A flexible option resizes storage to fit the encoded child.<br><br>`view.config_mut().set(Some(&config))?;` |
//! | `#[capacity = N] Vec<T>` | [`FixedCapacityVec`] | Can `push`, `pop`, `set`, `truncate`, and `clear` active elements up to schema capacity `N`; it never resizes storage.<br><br>`view.items_mut().push(item)?;` |
//! | Final `#[extendable = N] Vec<T>` | [`ExtendableVec`] | Can `push`, `extend_from_slice`, `pop`, `set`, `truncate`, and `clear`; it may resize storage to grow capacity because the field is trailing.<br><br>`view.tail_mut().push(item)?;` |
//!
//! In other words, only a trailing extendable Vec can expand its backing
//! capacity. A non-trailing fixed-capacity Vec can still expand its active
//! length, but only up to the capacity reserved by the schema.
//! Direct storage implementations such as `AccountView` resize to exactly the
//! requested byte length. To reduce repeated reallocations while growing a
//! trailing extendable Vec in a loop, wrap the storage in [`MaxLenStorage`]
//! before calling `decode_mut()`.
//!
//! # Practical Guidelines
//!
//! Use [`fixed_offset_layout!`](crate::fixed_offset_layout) when the encoded
//! data is long-lived state and stable offsets are useful. This is the better
//! fit when a `Vec<T>` should have schema-level capacity, because the layout
//! reserves storage even when the active length is smaller.
//! User-defined `Vec<T>` elements in `fixed_offset_layout` must be fixed-size
//! layouts and do not use `#[element_size = ...]`.
//! A final `#[extendable = N] Vec<T>` is the exception to fixed-size total
//! storage. Canonical `encode()` writes equal count and capacity headers,
//! followed by the active elements. The generated [`ExtendableVec`] getter
//! reads both `len()` and `capacity()` from the record.
//! For mutation, generated `decode_mut(storage)` works with
//! [`LayoutStorageMut`] and exposes `<field>_mut()` helpers for scalar,
//! option, and Vec fields.
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

// An 8-byte flexible length header can encode far beyond practical account
// sizes. Layout bounds are `usize` values computed as
// `len_width + max_len * elem_size`, so accepting the full `u64::MAX` range
// would make those bounds overflow instead of being useful API.
pub(super) const MAX_SUPPORTED_VEC_LEN: usize = u32::MAX as usize;

mod extendable_vec;
mod fixed_capacity_vec;
mod fixed_slice;
mod storage;
mod value_mut;
mod variable_slice;

#[doc(hidden)]
pub use extendable_vec::decode_extendable_vec_header;
pub use extendable_vec::{
    ExtendableVec, FixedLayoutElement, FixedLayoutExtendableVecIter, FixedValueElement,
};
pub use fixed_capacity_vec::FixedCapacityVec;
pub use fixed_slice::{FixedLayoutSlice, FixedLayoutSliceIter};
pub use storage::{LayoutStorage, LayoutStorageMut, LayoutWriteGuard, MaxLenStorage};
pub use value_mut::{
    FixedLayoutOptionMut, FlexibleLayoutOptionMut, FlexibleOptionBytes, LayoutBoolMut,
    LayoutBoolOptionMut, LayoutCopyMut, LayoutOptionMut,
};
pub use variable_slice::{VariableLayoutSlice, VariableLayoutSliceIter};

pub trait DataLayoutKind {
    const IS_FIXED: bool;
}

/// Starting address required by a nested layout, expressed modulo eight.
/// Both layout macros implement this trait. Handwritten children used in
/// fixed-offset options or vectors must declare their placement as well.
///
/// A version byte and an option tag place the child at byte 2 in this parent.
///
/// ```compile_fail,E0080
/// # extern crate alloc;
/// use wheels::fixed_offset_layout;
/// #[fixed_offset_layout(buffer_offset = 0)]
/// struct Child { value: u8 }
/// #[fixed_offset_layout(buffer_offset = 0)]
/// struct Parent {
///     version: u8,
///     #[flexible]
///     child: Option<Child>,
/// }
/// ```
///
/// A variable-offset parent starting at byte 1 places its tagged child at byte 2.
///
/// ```compile_fail,E0080
/// # extern crate alloc;
/// use wheels::{fixed_offset_layout, variable_offset_layout};
/// #[fixed_offset_layout(buffer_offset = 0)]
/// struct Leaf { value: u8 }
/// #[variable_offset_layout(buffer_offset = 1)]
/// struct Middle { leaf: Option<Leaf> }
/// ```
///
/// A parent accepting every address also requires its children to accept every
/// address. For variable-offset layouts, this applies to nested options and vectors.
///
/// ```compile_fail,E0080
/// # extern crate alloc;
/// use wheels::{fixed_offset_layout, variable_offset_layout};
/// #[variable_offset_layout(buffer_offset = 2)]
/// struct Child { value: u8 }
/// #[fixed_offset_layout(buffer_offset = any)]
/// struct Parent {
///     version: u8,
///     #[flexible]
///     child: Option<Child>,
/// }
/// ```
pub trait LayoutPlacement {
    /// `None` accepts every address. `Some(0..=7)` requires that byte offset.
    const BUFFER_OFFSET: Option<usize>;
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
/// Decoding one value from a caller-framed slice.
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
    /// Decodes one complete record, including any slots reserved by its capacity header.
    /// Use [`PrefixDecodable::decode_prefix`] when the slice contains following records.
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
