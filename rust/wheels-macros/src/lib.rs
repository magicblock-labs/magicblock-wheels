use proc_macro::TokenStream;
use syn::{parse_macro_input, ItemStruct};

mod data_layout;

///
/// Usage
/// =====
///
/// ```ignore
///
/// use wheels::Pubkey;
///
/// #[data_layout(buffer_offset = 1)]
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
/// #[data_layout(buffer_offset = 1, option = implicit)]
/// struct DepositAndDelegateShuttleArgs {
///     shuttle_id: u32,
///     validator: Option<Address>,
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
///   - `#[data_layout(buffer_offset = 0..7)]`
///   - `#[data_layout(buffer_offset = 0..7, option = implicit)]`
///
///     - `buffer_offset`
///
///       Mandatory.
///
///       It specifies the offset of the input slice pointer from the previous
///       8-byte aligned base address, i.e:
///
///       `(bytes.as_ptr() as usize) % 8`
///
///       Example:
///
///       - if the original instruction input buffer is 8-byte aligned and
///         the payload slice passed to `decode()` is `&input[1..]`, then
///         `buffer_offset = 1`.
///
///       The macro uses this contract both at runtime and at compile-time:
///
///       - `decode()` validates that the actual slice pointer matches
///         this offset
///       - borrowed getters are only generated when their alignment can be
///         guaranteed for every valid encoding under this `buffer_offset`
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
///   - `Vec<bool>` is intentionally not supported by `data_layout`
///     because its current view API exposes borrowed slices for `Vec` fields.
///   - Plain `Pubkey` and `Option<Pubkey>` are supported.
///     `Pubkey` is encoded as 32 raw bytes and views return borrowed keys.
///   - `Vec<Pubkey>` is intentionally not supported by `data_layout`
///     because its current view API exposes borrowed slices for `Vec` fields.
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
///   - `pub fn decode(bytes: &[u8]) -> Result<SelfView, DataLayoutError>`
///   - `pub fn encode(&self) -> Result<Vec<u8>, DataLayoutError>`
///   - `pub fn encode_to(&self, bytes: &mut [u8]) -> Result<(), DataLayoutError>`
#[proc_macro_attribute]
pub fn data_layout(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_string = attr.to_string();
    let input = parse_macro_input!(item as ItemStruct);

    match data_layout::expand_data_layout(&attr_string, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
