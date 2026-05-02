use proc_macro::TokenStream;
use syn::{parse_macro_input, ItemStruct};

mod data_layout;
mod runtime;

/// Generates a binary layout with an immutable view type.
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
pub fn data_layout(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr_string = attr.to_string();
    let input = parse_macro_input!(item as ItemStruct);

    match data_layout::expand_data_layout(&attr_string, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
