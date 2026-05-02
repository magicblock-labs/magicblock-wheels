use proc_macro::TokenStream;
use syn::{parse_macro_input, ItemStruct};

mod fixed_layout;
mod runtime;
mod variable_layout;

/// Generates a fixed-offset binary layout with an immutable view type.
///
/// Notes:
/// - `Vec<T>` fields require `#[capacity = N]`.
/// - The last `Vec<T>` field may use `#[flexible = 1]` or `#[flexible = 2]`.
/// - The last `Option<T>` field may use `#[flexible]`.
/// - Borrowed getters are only generated when alignment can be guaranteed.
///
/// Generated items include:
/// - `StructNameView<'a>`
/// - `DATA_LEN` for fully fixed layouts, or `MIN_DATA_LEN` / `MAX_DATA_LEN`
///   for flexible tails
/// - `OFFSETS`
/// - `decode`, `encode`, and `encode_to`
#[proc_macro_attribute]
pub fn fixed_offset_layout(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_string = attr.to_string();
    let input = parse_macro_input!(item as ItemStruct);

    match fixed_layout::expand_fixed_offset_layout(&attr_string, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Generates a variable-offset binary layout with an immutable view type.
///
/// Notes:
/// - Requires `buffer_offset = 0..7`.
/// - `Vec<T>` fields use `#[flexible = 1]` or `#[flexible = 2]`.
/// - `option = implicit` enables tagless `Option<T>` encoding when there are
///   no `Vec` fields and option payload sizes have unique subset sums.
/// - `bool` and `Option<bool>` are supported; `Vec<bool>` is not.
///
/// Generated length metadata depends on the layout shape:
/// - `DATA_LEN`
/// - `DATA_LENS`
/// - `DATA_LEN_RANGE`
///
/// Generated methods include `decode`, `encode`, and `encode_to`.
#[proc_macro_attribute]
pub fn variable_offset_layout(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_string = attr.to_string();
    let input = parse_macro_input!(item as ItemStruct);

    match variable_layout::expand_variable_offset_layout(&attr_string, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
