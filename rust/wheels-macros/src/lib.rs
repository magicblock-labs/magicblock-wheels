use proc_macro::TokenStream;
use syn::{parse_macro_input, ItemStruct};

mod common;
mod fixed_offset_layout;
mod variable_offset_layout;

///
/// Usage
/// =====
///
/// ```ignore
///
/// use wheels::Pubkey;
///
/// #[fixed_offset_layout]
/// struct FixedTransferArgs {
///     shuttle_id: u32,
///     validator: Option<Pubkey>,
///     #[capacity = 72]
///     encrypted_destination: Vec<u8>,
///     checksum: u16,
/// }
///
/// #[fixed_offset_layout]
/// struct TransferWithTrailingPayloadArgs {
///     shuttle_id: u32,
///     #[capacity = 4]
///     reserved_tags: Vec<u8>,
///     #[flexible = 2]
///     payload: Vec<u8>,
/// }
///
/// ```
///
/// The generated code refers directly to `::wheels`, `::alloc`, `::bytemuck`,
/// `::pinocchio`, and `::pinocchio_log`. In `no_std` crates, bring `alloc`
/// into scope with `extern crate alloc;`.
///
/// Layout forms
/// ============
///
/// `fixed_offset_layout` keeps field start offsets stable. It has two encoded
/// size forms:
///
///   - Constant-size layouts.
///
///     All fields have fixed encoded slots. `Vec<T>` fields use
///     `#[capacity = N]`, reserve space for `N` elements, and expose both
///     active `len` and schema `capacity` in generated views. The macro emits
///     `DATA_LEN` and implements `Encodable`, `Decodable`, and
///     `FixedSizeLayout`.
///
///   - Trailing-flexible layouts.
///
///     The final field may use `#[flexible = 1]` or `#[flexible = 2]` for a
///     trailing `Vec<T>`, or `#[flexible]` for a trailing `Option<T>`. Earlier
///     fields still have fixed offsets, but total encoded length varies, so the
///     macro emits `MIN_DATA_LEN` and `MAX_DATA_LEN` instead of `DATA_LEN`.
///     These layouts do not implement `FixedSizeLayout`.
///
/// Attributes
/// ==========
///
/// Struct attributes:
///   - none. `#[fixed_offset_layout]` does not take parameters.
///
/// Field attributes:
///   - `#[capacity = N]`
///
///     - Mandatory for non-flexible `Vec<T>` fields.
///     - Reserves space for exactly `N` elements.
///     - The encoded Vec length uses a 1-byte header when `N <= 255`, otherwise
///       a 2-byte header.
///     - Generated views expose an additional `<field>_capacity()` method.
///
///   - `#[flexible = N]`
///
///     - Applicable only to the final field when that field is `Vec<T>`.
///     - `N` must be `1` or `2` and is the width, in bytes, of the encoded Vec
///       length header.
///     - The field contributes only its active payload bytes to encoded length.
///
///   - `#[flexible]`
///
///     - Applicable only to the final field when that field is `Option<T>`.
///     - `None` omits the option tag and payload entirely.
///     - `Some(value)` writes a tag byte followed by the value payload.
///
/// Supported field kinds:
///
///   - Plain `bool` and `Option<bool>` are supported.
///     They are encoded as a single backing `u8` byte where `0` means `false`
///     and any non-zero byte decodes as `true`.
///   - `Vec<bool>` and `Option<Vec<T>>` are intentionally not supported.
///   - Plain `Pubkey`/`Address` and `Option<Pubkey>`/`Option<Address>` are
///     supported. Views return borrowed keys.
///   - Integer primitives, fixed-size arrays of integer primitives, and Vecs of
///     supported fixed-value element types are supported.
///
/// APIs
/// ====
///
/// Fields:
///   - `pub const DATA_LEN: usize`
///     for constant-size layouts
///   - `pub const MIN_DATA_LEN: usize` and `pub const MAX_DATA_LEN: usize`
///     for trailing-flexible layouts
///   - `pub const OFFSETS: [usize; N]`
///     for field start offsets
///
/// Trait APIs:
///   - all layouts implement `Encodable`
///   - all layouts implement exact `Decodable`
///   - constant-size layouts also implement `PrefixDecodable` and
///     `FixedSizeLayout`
///
/// Import the relevant traits from `wheels::layout` to call `encode`,
/// `encode_to`, `decode`, or `decode_prefix`. These APIs return
/// `DataLayoutError`.
#[proc_macro_attribute]
pub fn fixed_offset_layout(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_string = attr.to_string();
    let input = parse_macro_input!(item as ItemStruct);

    match fixed_offset_layout::expand_fixed_offset_layout(&attr_string, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

///
/// Usage
/// =====
///
/// ```ignore
///
/// use wheels::Pubkey;
///
/// #[variable_offset_layout(buffer_offset = 1)]
/// struct DepositAndDelegateShuttleWithPrivateTransferArgs {
///     shuttle_id: u32,
///     amount: u64,
///     validator: Option<Pubkey>,
///     #[flexible = 1]
///     encrypted_destination: Vec<u8>,
///     #[flexible = 2]
///     encrypted_data_suffix: Vec<u8>,
/// }
///
/// #[variable_offset_layout(buffer_offset = 1, option = implicit)]
/// struct DepositAndDelegateShuttleArgs {
///     shuttle_id: u32,
///     validator: Option<Pubkey>,
///     amount: u64,
/// }
///
/// ```
///
/// The generated code refers directly to `::wheels`, `::alloc`, `::bytemuck`, and `::pinocchio_log`.
/// In `no_std` crates, bring `alloc` into scope with `extern crate alloc;`.
///
/// Attributes
/// ==========
///
/// Struct attributes:
///   - `#[variable_offset_layout(buffer_offset = 0..=7)]`
///   - `#[variable_offset_layout(buffer_offset = unaligned)]`
///   - `#[variable_offset_layout(buffer_offset = 0..=7, option = implicit)]`
///
///     - `buffer_offset`
///
///       Mandatory.
///
///       Use `buffer_offset = N` when the input slice always starts at a known
///       offset from an 8-byte aligned base address:
///
///       `(bytes.as_ptr() as usize) % 8`
///
///       Example:
///
///       - if the original instruction input buffer is 8-byte aligned and
///         the payload slice passed to `decode()` is `&input[1..]`, then
///         `buffer_offset = 1`.
///
///       Fixed offsets are used both at runtime and at compile-time:
///
///       - the generated decoder validates that the actual slice pointer matches
///         this offset
///       - borrowed getters are only generated when their alignment can be
///         guaranteed for every valid encoding under this `buffer_offset`
///
///       Use `buffer_offset = unaligned` when the slice may start at any
///       address, such as when decoding the remaining bytes after
///       variable-length data. This mode emits no pointer-offset check and
///       rejects borrowed views whose required alignment is greater than 1.
///       Copy-decoded fields such as integer primitives remain supported.
///
///     - `option = implicit`
///
///       Optional.
///
///       By default, `Option<T>` is encoded explicitly with a tag byte. That
///       default/tagged form is locally self-describing: each option carries
///       its own presence marker, so the meaning of one field does not depend
///       on the total length of the whole struct.
///
///       `option = implicit` instead enables compact `Option<T>` encoding
///       without a tag byte. It is supported only when the struct has no `Vec`
///       fields and the payload sizes of its `Option<T>` fields have unique
///       subset sums.
///
///       In other words, every valid present/absent combination of the
///       implicit options must produce a distinct total encoded length.
///       This makes the implicit form globally length-described: option
///       presence is inferred from the overall encoded length, not from a
///       per-field tag.
///
///       Encoding:
///
///       - `None` omits the optional payload entirely
///       - `Some(value)` writes only the payload bytes
///
///       The generated `decode()` accepts only the total lengths implied by the
///       valid combinations of those implicit options.
///
///       Stability note:
///
///       - Tagged options are easier to extend later because earlier fields
///         remain locally self-describing.
///       - Implicit options are more compact, but future schema evolution can
///         be trickier because adding new trailing options changes the global
///         length mapping used to infer presence.
///       - Adding new trailing fixed-size fields is still possible, but adding
///         more implicit options later may force the layout to stop being
///         representable as `option = implicit`.
///
/// Supported field kinds:
///
///   - Plain `bool` and `Option<bool>` are supported.
///     They are encoded as a single backing `u8` byte where `0` means
///     `false` and any non-zero byte decodes as `true`.
///   - `Vec<bool>` is intentionally not supported by `variable_offset_layout`
///     because its current view API exposes borrowed slices for `Vec` fields.
///   - Plain `Pubkey`/`Address` and `Option<Pubkey>`/`Option<Address>` are supported.
///     `Pubkey` is encoded as 32 raw bytes and views return borrowed keys.
///   - `Vec<Pubkey>`/`Vec<Address>` is supported and views return borrowed key
///     slices.
///
/// Field attributes:
///   - `#[flexible = N]`
///
///     - Mandatory: yes, field-type: `Vec`
///
///     - Examples
///
///       - `#[flexible = 1]`
///       - `#[flexible = 2]`
///       - `#[flexible = 4]`
///       - `#[flexible = 8]`
///
///     The number indicates the width, in bytes, used to encode `Vec` length.
///
///     `N` must be in `1..=8`.
///
///     Length is encoded as an unsigned little-endian integer stored in those
///     `N` bytes. For `N = 8`, the supported Vec length is still capped at
///     `u32::MAX`.
///
/// APIs
/// ====
///
/// Fields:
///   - `pub const DATA_LEN: usize`
///     when the layout has exactly one valid encoded length
///   - `pub const DATA_LENS: [usize; N]`
///     when the layout has no `Vec` fields and finitely many exact valid lengths
///   - `pub const DATA_LEN_RANGE: (usize, usize)`
///     when the layout contains a `Vec` field
///
/// Methods:
///   - all layouts implement `Decodable`
///   - tagged/self-delimiting layouts also implement `PrefixDecodable`
///   - `pub fn encode(&self) -> Result<Vec<u8>, DataLayoutError>`
///   - `pub fn encode_to(&self, bytes: &mut [u8]) -> Result<(), DataLayoutError>`
#[proc_macro_attribute]
pub fn variable_offset_layout(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_string = attr.to_string();
    let input = parse_macro_input!(item as ItemStruct);

    match variable_offset_layout::expand_variable_offset_layout(&attr_string, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
