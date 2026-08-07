use crate::common::{
    ensure_allow_dead_code, impl_where_clause, is_bool, is_string, option_inner, parse_value_kind,
    read_copy_expr, strip_field_attr, usize_lit, vec_inner, AccessMode, FixedValueKind,
};
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{spanned::Spanned, Expr, ExprLit, Fields, Ident, ItemStruct, Lit, LitInt, Type};

const FIELD_ATTRIBUTES: &[&str] = &["capacity", "flexible"];
const LAYOUT_NAME: &str = "fixed_offset_layout";
const UNSUPPORTED_FIELD_MESSAGE: &str =
    "fixed_offset_layout fields must be bool, Pubkey, integer primitives, or fixed-size arrays";
const INTEGER_FIELD_MESSAGE: &str = "field must be an integer primitive";

// [capacity = flexible] array-len is always encoded as 2-bytes
const MAX_LEN_WIDTH: usize = 2;

const MAX_CAPACITY: usize = 0xffff;

pub(crate) fn expand_fixed_offset_layout(
    attr: &str,
    input: &ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    parse_args(attr)?;
    let mut emitted_input = input.clone();
    emitted_input
        .attrs
        .retain(|attr| !attr.path().is_ident("fixed_offset_layout"));

    ensure_allow_dead_code(&mut emitted_input.attrs);

    let struct_name = &emitted_input.ident;
    let view_name = format_ident!("{}View", struct_name);

    let Fields::Named(fields) = &mut emitted_input.fields else {
        return Err(syn::Error::new_spanned(
            &emitted_input.fields,
            "fixed_offset_layout requires named fields",
        ));
    };

    let mut offset_expr = quote!(0usize);
    let mut offset_value = Some(0usize);
    let mut offsets = vec![quote!(0usize)];

    let mut fields_encode_expr = quote!();

    let mut where_bounds = Vec::<proc_macro2::TokenStream>::new();
    let mut view_methods = Vec::new();

    let mut validate_steps = Vec::new();
    let mut layout_error: Option<syn::Error> = None;
    let mut required_alignment = 1usize;

    let mut trailing_flexible_field = None;
    let field_count = fields.named.len();
    for (index, field) in fields.named.iter_mut().enumerate() {
        let field_ident = field.ident.as_ref().expect("named field");

        let is_last_field = index + 1 == field_count;

        let layout = parse_field_layout(field, is_last_field)?;

        strip_field_attr(&mut field.attrs, FIELD_ATTRIBUTES);

        if let Some(offset) = offset_value {
            match layout.check_ref_alignment(offset, field_ident) {
                Ok(Some(issue)) => {
                    if let Some(existing) = &mut layout_error {
                        existing.combine(issue.error);
                    } else {
                        layout_error = Some(issue.error);
                    }
                    offset_value = Some(offset + issue.padding);
                    let padding = usize_lit(issue.padding);
                    offset_expr = quote!((#offset_expr + #padding));
                }
                Ok(None) => {}
                Err(err) => {
                    if let Some(existing) = &mut layout_error {
                        existing.combine(err);
                    } else {
                        layout_error = Some(err);
                    }
                }
            }
        }

        required_alignment = required_alignment.max(layout.borrowed_alignment());

        validate_steps.push(layout.gen_validate_step(offset_expr.clone(), field_ident));
        view_methods.push(layout.gen_view_methods(offset_expr.clone(), field_ident)?);

        fields_encode_expr =
            layout.gen_field_encode(fields_encode_expr, offset_expr.clone(), field_ident);

        if let Some(bound) = layout.bound() {
            where_bounds.push(bound);
        }

        match layout.slot_min_len_expr() {
            Ok(slot_len) => {
                let slot_len_expr = slot_len.expr;
                let slot_len_value = slot_len.value;
                offset_expr = quote!((#offset_expr + #slot_len_expr));
                offset_value = match (offset_value, slot_len_value) {
                    (Some(offset), Some(slot_len)) => Some(offset + slot_len),
                    _ => None,
                };
                offsets.push(offset_expr.clone());
            }
            Err(len_width) => {
                assert!(is_last_field, "field must the last item");

                // The last offset becomes datalen which in this case is MIN_DATA_LEN.
                offsets.push(offset_expr.clone());

                trailing_flexible_field = Some((field_ident, len_width));
            }
        }
    }

    let field_count_expr = {
        let field_count = usize_lit(field_count);
        quote!(#field_count)
    };
    let datalen_expr = offsets.pop().unwrap();
    let datalen_value = offset_value;
    let offsets_expr = quote! { [#(#offsets),*] };

    if let Some(err) = layout_error {
        return Err(err);
    }

    let where_clause = impl_where_clause(&where_bounds);

    let msg = datalen_value
        .map(|datalen| format!("Sum of encodable-sizes must be {}.", datalen))
        .unwrap_or_else(|| {
            "Sum of encodable-sizes is computed from nested fixed-size layouts.".to_string()
        });
    let has_trailing_flexible_field = trailing_flexible_field.is_some();
    let required_alignment_lit = usize_lit(required_alignment);

    let (datalen_vars, datalen_check, check_log_expr, encoded_len_expr) =
        match trailing_flexible_field {
            Some((trailing_flexible_field, comptime_optlen)) => {
                let max_datalen = comptime_optlen.max_datalen_expr();
                let encoded_len_expr = match &comptime_optlen {
                    ComptimeOptionalLen::ArrayLen {
                        len_width,
                        elem_size,
                        ..
                    } => {
                        let max_capacity = usize_lit(2usize.pow(*len_width as u32 * 8) - 1);
                        let len_width = usize_lit(*len_width);
                        quote! {
                            let field_len = self.#trailing_flexible_field.len();
                            if field_len > #max_capacity {
                                ::pinocchio_log::log!(
                                    "Cannot encode field {}: len {} exceeds max {}",
                                    stringify!(#trailing_flexible_field),
                                    field_len,
                                    #max_capacity,
                                );
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            }

                            Ok(Self::MIN_DATA_LEN + if field_len == 0 {
                                0
                            } else {
                                #len_width + field_len * #elem_size
                            })
                        }
                    }
                    ComptimeOptionalLen::Option { value_size } => {
                        let value_size = usize_lit(*value_size);
                        quote! {
                            Ok(Self::MIN_DATA_LEN + self.#trailing_flexible_field.as_ref().map(|_| 1 + #value_size).unwrap_or(0))
                        }
                    }
                };
                (
                    quote! {
                        pub const MIN_DATA_LEN: usize = #datalen_expr;
                        pub const MAX_DATA_LEN: usize = Self::MIN_DATA_LEN + #max_datalen;
                    },
                    quote!(bytes.len() < Self::MIN_DATA_LEN || bytes.len() > Self::MAX_DATA_LEN),
                    quote! {
                        ::pinocchio_log::log!(
                            "bytes [len={}] cannot be deserialized to {} which needs at least {} or at most {} bytes",
                            bytes.len(),
                            stringify!(#struct_name),
                            Self::MIN_DATA_LEN,
                            Self::MAX_DATA_LEN,
                        );
                    },
                    encoded_len_expr,
                )
            }
            None => (
                quote!(pub const DATA_LEN: usize = #datalen_expr;),
                quote!(bytes.len() != Self::DATA_LEN),
                quote! {
                    ::pinocchio_log::log!(
                        "bytes [len={}] cannot be deserialized to {} which needs exactly {} bytes",
                        bytes.len(),
                        stringify!(#struct_name),
                        Self::DATA_LEN,
                    );
                },
                quote!(Ok(#struct_name::DATA_LEN)),
            ),
        };
    let decode_trait_impls = if has_trailing_flexible_field {
        quote! {
            impl ::wheels::layout::Decodable for #struct_name {
                type View<'a> = #view_name<'a>;

                fn decode<'a>(
                    bytes: &'a [u8],
                ) -> core::result::Result<Self::View<'a>, ::wheels::DataLayoutError> {
                    Self::__validate_bytes(bytes)?;
                    Ok(#view_name { bytes })
                }
            }
        }
    } else {
        quote! {
            impl ::wheels::layout::PrefixDecodable for #struct_name {
                type View<'a> = #view_name<'a>;

                fn decode_prefix<'a>(
                    bytes: &'a [u8],
                ) -> core::result::Result<(Self::View<'a>, &'a [u8]), ::wheels::DataLayoutError> {
                    if bytes.len() < #struct_name::DATA_LEN {
                        ::pinocchio_log::log!(
                            "bytes [len={}] are too small to decode {} which needs {} bytes",
                            bytes.len(),
                            stringify!(#struct_name),
                            #struct_name::DATA_LEN,
                        );
                        return Err(::wheels::DataLayoutError::InvalidDataLength);
                    }

                    let (bytes, remaining) = bytes.split_at(#struct_name::DATA_LEN);
                    Self::__validate_bytes(bytes)?;
                    Ok((#view_name { bytes }, remaining))
                }
            }

            impl ::wheels::layout::FixedSizeLayout for #struct_name {
                const DATA_LEN: usize = #struct_name::DATA_LEN;
            }
        }
    };
    let layout_bounds_impl = if has_trailing_flexible_field {
        quote! {
            impl ::wheels::layout::LayoutBounds for #struct_name {
                const MIN_DATA_LEN: usize = #struct_name::MIN_DATA_LEN;
                const MAX_DATA_LEN: usize = #struct_name::MAX_DATA_LEN;
            }
        }
    } else {
        quote!()
    };

    Ok(quote! {
        #emitted_input

        impl #struct_name {
            #[doc = #msg]
            #datalen_vars

            #[doc = "Byte offsets marking the start of each field"]
            pub const OFFSETS: [usize; #field_count_expr] = #offsets_expr;

            fn __validate_bytes(
                bytes: &[u8],
            ) -> core::result::Result<(), ::wheels::DataLayoutError> {
                if #datalen_check {
                    #check_log_expr
                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                } else if #required_alignment_lit > 1
                    && bytes.as_ptr().align_offset(#required_alignment_lit) != 0
                {
                    ::pinocchio_log::log!(
                        "bytes [align_offset={}] cannot be deserialized to {} which requires {}-byte alignment",
                        bytes.as_ptr().align_offset(#required_alignment_lit),
                        stringify!(#struct_name),
                        #required_alignment_lit,
                    );
                    return Err(::wheels::DataLayoutError::InvalidFieldAlignment);
                }

                #(#validate_steps)*

                Ok(())
            }

            fn __validate_option(
                bytes: &[u8],
                offset: usize,
                field_name: &'static str,
            ) -> core::result::Result<(), ::wheels::DataLayoutError> {
                match bytes[offset] {
                    0 | 1 => {}

                    tag => {
                        ::pinocchio_log::log!("Invalid Option tag for field {}::{} : tag = {} (which should be either 0 or 1)", stringify!(#struct_name), field_name, tag);
                        return Err(::wheels::DataLayoutError::InvalidOptionTag);
                    }
                }
                Ok(())
            }

            fn __validate_flexible_option(
                bytes: &[u8],
                offset: usize,
                value_size: usize,
                field_name: &'static str,
            ) -> core::result::Result<(), ::wheels::DataLayoutError> {
                if bytes.len() == offset {
                    return Ok(());
                }

                let expected_len = offset + 1 + value_size;
                if bytes.len() < expected_len {
                    ::pinocchio_log::log!(
                        "Truncated flexible Option payload for field {}::{} : expected {} bytes, found {}",
                        stringify!(#struct_name),
                        field_name,
                        expected_len,
                        bytes.len(),
                    );
                    return Err(::wheels::DataLayoutError::TruncatedPayload);
                }
                if bytes.len() > expected_len {
                    ::pinocchio_log::log!(
                        "Invalid flexible Option length for field {}::{} : expected {} bytes, found {}",
                        stringify!(#struct_name),
                        field_name,
                        expected_len,
                        bytes.len(),
                    );
                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                }

                match bytes[offset] {
                    1 => {}

                    tag => {
                        ::pinocchio_log::log!("Invalid flexible Option tag for field {}::{} : tag = {} (which should be 1 when present; None omits the field)", stringify!(#struct_name), field_name, tag);
                        return Err(::wheels::DataLayoutError::InvalidOptionTag);
                    }
                }
                Ok(())
            }

            fn __read_vec_len(bytes: &[u8], offset: usize, len_width: usize) -> usize {
                match len_width {
                    1 => bytes[offset] as usize,
                    2 => {
                        let raw: [u8; 2] = bytes[offset..offset + 2].try_into().expect("validated len");
                        u16::from_le_bytes(raw) as usize
                    },
                    _ => {
                        unreachable!()
                    }
                }
            }

            fn __validate_vec_len(
                bytes: &[u8],
                offset: usize,
                capacity: usize,
                len_width: usize,
                field_name: &'static str,
            ) -> core::result::Result<(), ::wheels::DataLayoutError> {
                let len = Self::__read_vec_len(bytes, offset, len_width);
                if len > capacity {
                    ::pinocchio_log::log!("Invalid Vec length for field {}::{} : capacity = {}, len = {}", stringify!(#struct_name), field_name, capacity, len);
                    return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                }
                Ok(())
            }

            fn __validate_flexible_vec_len(
                bytes: &[u8],
                offset: usize,
                capacity: usize,
                len_width: usize,
                elem_size: usize,
                field_name: &'static str,
            ) -> core::result::Result<(), ::wheels::DataLayoutError> {
                if bytes.len() == offset {
                    return Ok(());
                }
                if bytes.len() < offset + len_width {
                    ::pinocchio_log::log!(
                        "Missing Vec length header for field {}::{} : need {} bytes, found {}",
                        stringify!(#struct_name),
                        field_name,
                        offset + len_width,
                        bytes.len(),
                    );
                    return Err(::wheels::DataLayoutError::MissingLengthHeader);
                }

                let len = Self::__read_vec_len(bytes, offset, len_width);
                if len == 0 {
                    ::pinocchio_log::log!(
                        "Invalid flexible Vec length for field {}::{} : empty Vec must omit the length header",
                        stringify!(#struct_name),
                        field_name,
                    );
                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                }
                if len > capacity {
                    ::pinocchio_log::log!("Invalid Vec length for field {}::{} : capacity = {}, len = {}", stringify!(#struct_name), field_name, capacity, len);
                    return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                }

                let expected_len = offset + len_width + len * elem_size;
                if bytes.len() < expected_len {
                    ::pinocchio_log::log!(
                        "Truncated Vec payload for field {}::{} : expected {} bytes, found {}",
                        stringify!(#struct_name),
                        field_name,
                        expected_len,
                        bytes.len(),
                    );
                    return Err(::wheels::DataLayoutError::TruncatedVectorPayload);
                }
                if bytes.len() > expected_len {
                    ::pinocchio_log::log!(
                        "Invalid Vec payload length for field {}::{} : expected {} bytes, found {}",
                        stringify!(#struct_name),
                        field_name,
                        expected_len,
                        bytes.len(),
                    );
                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                }

                Ok(())
            }
        }

        impl ::wheels::layout::Encodable for #struct_name {
            fn encoded_len(
                &self,
            ) -> core::result::Result<usize, ::wheels::DataLayoutError> {
                #encoded_len_expr
            }

            fn encode_to<'a>(
                &self,
                out: &'a mut [u8],
            ) -> core::result::Result<&'a mut [u8], ::wheels::DataLayoutError> {
                let encoded_len = self.encoded_len()?;
                if out.len() < encoded_len {
                    ::pinocchio_log::log!(
                        "bytes [len={}] are too small to encode {} which needs {} bytes",
                        out.len(),
                        stringify!(#struct_name),
                        encoded_len,
                    );
                    return Err(::wheels::DataLayoutError::OutputBufferTooSmall);
                }

                let (bytes, remaining) = out.split_at_mut(encoded_len);
                #fields_encode_expr;
                Ok(remaining)
            }
        }

        #decode_trait_impls

        #layout_bounds_impl

        #[allow(dead_code)]
        #[derive(Debug)]
        pub struct #view_name<'a> {
            bytes: &'a [u8],
        }

        impl<'a> #view_name<'a> #where_clause {
            pub fn bytes(&self) -> &'a [u8] {
                self.bytes
            }

            #(#view_methods)*
        }
    })
}

///
/// Describes whether Option field has #[flexible] or not.
///
enum Optional {
    Fixed,    // no attribute implies fixed
    Flexible, // #[flexible]
}

enum FixedFieldKind {
    Value {
        value: FixedValueKind,
        optional: Option<Optional>,
    },
    Vec {
        elem: FixedVecElementKind,
        capacity: Capacity,
    },
}

#[derive(Clone)]
enum FixedVecElementKind {
    FixedValue(FixedValueKind),
    FixedSizeLayout { ty: Type },
}

impl FixedVecElementKind {
    fn ty(&self) -> &Type {
        match self {
            Self::FixedValue(value) => value.ty(),
            Self::FixedSizeLayout { ty } => ty,
        }
    }

    fn size_expr(&self) -> proc_macro2::TokenStream {
        match self {
            Self::FixedValue(value) => value.size_expr(),
            Self::FixedSizeLayout { ty } => {
                quote!(<#ty as ::wheels::layout::FixedSizeLayout>::DATA_LEN)
            }
        }
    }

    fn known_size(&self) -> Option<usize> {
        match self {
            Self::FixedValue(value) => Some(value.size()),
            Self::FixedSizeLayout { .. } => None,
        }
    }

    fn borrowed_alignment(&self) -> usize {
        match self {
            Self::FixedValue(value) => value.align(),
            Self::FixedSizeLayout { .. } => 1,
        }
    }

    fn needs_pod_bound(&self) -> bool {
        match self {
            Self::FixedValue(value) => value.needs_pod_bound(),
            Self::FixedSizeLayout { .. } => false,
        }
    }
}

struct SlotLen {
    expr: proc_macro2::TokenStream,
    value: Option<usize>,
}

#[derive(Clone)]
enum ComptimeOptionalLen {
    ArrayLen {
        len_width: usize,
        elem_size: proc_macro2::TokenStream,
    },
    Option {
        value_size: usize,
    },
}

impl ComptimeOptionalLen {
    fn max_datalen_expr(&self) -> proc_macro2::TokenStream {
        match self {
            ComptimeOptionalLen::ArrayLen {
                len_width,
                elem_size,
                ..
            } => {
                let len_width_value = *len_width;
                let len_width = usize_lit(len_width_value);
                let max_capacity = usize_lit(2usize.pow(len_width_value as u32 * 8) - 1);
                quote!(#len_width + #max_capacity * #elem_size)
            }
            ComptimeOptionalLen::Option { value_size } => {
                let value_size = usize_lit(*value_size);
                quote!(1usize + #value_size)
            }
        }
    }
}

struct PaddingIssue {
    padding: usize,
    error: syn::Error,
}

impl FixedFieldKind {
    fn borrowed_alignment(&self) -> usize {
        match self {
            Self::Value { value, .. } => match value.access_mode() {
                AccessMode::Copy => 1,
                AccessMode::Ref => value.align(),
            },
            Self::Vec { elem, .. } => elem.borrowed_alignment(),
        }
    }

    fn slot_min_len_expr(&self) -> Result<SlotLen, ComptimeOptionalLen> {
        match self {
            Self::Value { value, optional } => {
                let value_size_expr = value.size_expr();
                match optional {
                    Some(Optional::Fixed) => Ok(SlotLen {
                        expr: quote!((1usize + #value_size_expr)),
                        value: Some(1 + value.size()),
                    }),
                    Some(Optional::Flexible) => Err(ComptimeOptionalLen::Option {
                        value_size: value.size(),
                    }),
                    None => Ok(SlotLen {
                        expr: value_size_expr,
                        value: Some(value.size()),
                    }),
                }
            }
            Self::Vec { elem, capacity } => {
                let elem_size_expr = elem.size_expr();
                let len_width_lit = capacity.len_width_lit();
                capacity
                    .comptime_capacity_lit()
                    .map(|cap| {
                        let value = elem.known_size().map(|size| {
                            capacity.len_width() + size * capacity.comptime_capacity().unwrap()
                        });
                        SlotLen {
                            expr: quote!((#len_width_lit + #elem_size_expr * #cap)),
                            value,
                        }
                    })
                    .ok_or(ComptimeOptionalLen::ArrayLen {
                        len_width: capacity.len_width(),
                        elem_size: elem_size_expr,
                    })
            }
        }
    }

    fn bound(&self) -> Option<proc_macro2::TokenStream> {
        match self {
            Self::Value { value, .. } => {
                if value.needs_pod_bound() {
                    let ty = value.ty();
                    Some(quote!(#ty: ::bytemuck::Pod))
                } else {
                    None
                }
            }
            Self::Vec { elem, .. } => {
                if elem.needs_pod_bound() {
                    let ty = elem.ty();
                    Some(quote!(#ty: ::bytemuck::Pod))
                } else if let FixedVecElementKind::FixedSizeLayout { ty } = elem {
                    Some(quote!(#ty: ::wheels::layout::FixedSizeLayout))
                } else {
                    None
                }
            }
        }
    }

    fn check_ref_alignment(
        &self,
        offset: usize,
        field_ident: &Ident,
    ) -> syn::Result<Option<PaddingIssue>> {
        match self {
            Self::Value { value, optional } => {
                if matches!(value.access_mode(), AccessMode::Copy) {
                    return Ok(None);
                }

                let align = value.align();
                if align > 8 {
                    return Err(syn::Error::new(
                        field_ident.span(),
                        format!(
                            "field `{}` cannot be borrowed by fixed_offset_layout: size is {} byte(s) but alignment is {} byte(s), and fixed_offset_layout only assumes the input buffer is 8-byte aligned",
                            field_ident,
                            value.size(),
                            align,
                        ),
                    ));
                }

                let payload_offset = offset + usize::from(optional.is_some());
                let misalignment = payload_offset % align;
                if misalignment == 0 {
                    return Ok(None);
                }

                let padding = align - misalignment;
                let message = if optional.is_some() {
                    format!(
                        "field `{}` needs {} byte(s) of padding before it: its Option payload would start at offset {}, but borrowed values of this field must be {}-byte aligned. Insert `_pad: [u8; {}]` before `{}` so the payload starts at offset {}",
                        field_ident,
                        padding,
                        payload_offset,
                        align,
                        padding,
                        field_ident,
                        payload_offset + padding,
                    )
                } else {
                    format!(
                        "field `{}` needs {} byte(s) of padding before it: it would start at offset {}, but borrowed values of this field must be {}-byte aligned. Insert `_pad: [u8; {}]` before `{}` so it starts at offset {}",
                        field_ident,
                        padding,
                        offset,
                        align,
                        padding,
                        field_ident,
                        offset + padding,
                    )
                };
                Ok(Some(PaddingIssue {
                    padding,
                    error: syn::Error::new(field_ident.span(), message),
                }))
            }
            Self::Vec { elem, capacity } => {
                let FixedVecElementKind::FixedValue(elem) = elem else {
                    return Ok(None);
                };

                let align = elem.align();
                if align > 8 {
                    return Err(syn::Error::new(
                        field_ident.span(),
                        format!(
                            "field `{}` cannot expose a slice view in fixed_offset_layout: each Vec element is {} byte(s) but alignment is {} byte(s), and fixed_offset_layout only assumes the input buffer is 8-byte aligned, so it cannot support type which requires alignment greater than 8",
                            field_ident,
                            elem.size(),
                            align,
                        ),
                    ));
                }

                let len_width = capacity.len_width();
                let first_elem_offset = offset + len_width;
                let misalignment = first_elem_offset % align;
                if misalignment == 0 {
                    return Ok(None);
                }

                let padding = align - misalignment;
                Ok(Some(PaddingIssue {
                    padding,
                    error: syn::Error::new(
                        field_ident.span(),
                        format!(
                        "field `{}` needs {} byte(s) of padding before it: its Vec elements start after a {}-byte length prefix, so element 0 would start at offset {}, but slice views require {}-byte alignment. Insert `_pad: [u8; {}]` before `{}` so element 0 starts at offset {}",
                        field_ident,
                        padding,
                        len_width,
                        first_elem_offset,
                        align,
                        padding,
                        field_ident,
                        first_elem_offset + padding,
                        ),
                    ),
                }))
            }
        }
    }

    fn gen_validate_step(
        &self,
        offset: proc_macro2::TokenStream,
        field_ident: &Ident,
    ) -> proc_macro2::TokenStream {
        let field_name = field_ident.to_string();
        let offset_expr = quote!((#offset));
        match self {
            Self::Value {
                value,
                optional: Some(Optional::Fixed),
            } => {
                let alignment_check =
                    value_alignment_check(value, quote!(#offset_expr + 1usize), &field_name);
                quote! {
                    Self::__validate_option(bytes, #offset_expr, #field_name)?;
                    if bytes[#offset_expr] != 0 {
                        #alignment_check
                    }
                }
            }
            Self::Value {
                value,
                optional: Some(Optional::Flexible),
            } => {
                let value_size = usize_lit(value.size());
                let alignment_check =
                    value_alignment_check(value, quote!(#offset_expr + 1usize), &field_name);
                quote! {
                    Self::__validate_flexible_option(bytes, #offset_expr, #value_size, #field_name)?;
                    if bytes.len() != #offset_expr {
                        #alignment_check
                    }
                }
            }
            Self::Vec { elem, capacity } => {
                let len_width_lit = capacity.len_width_lit();
                let elem_size = elem.size_expr();
                let len_expr = quote!(Self::__read_vec_len(bytes, #offset_expr, #len_width_lit));
                let data_offset = quote!(#offset_expr + #len_width_lit);
                let end_expr = quote!(#data_offset + #len_expr * #elem_size);
                let active_layout_validation = match elem {
                    FixedVecElementKind::FixedValue(_) => quote!(),
                    FixedVecElementKind::FixedSizeLayout { ty } => quote! {
                        let _ = ::wheels::layout::FixedLayoutSlice::<#ty>::new(
                            &bytes[#data_offset..#end_expr],
                        )?;
                    },
                };
                let alignment_check = match elem {
                    FixedVecElementKind::FixedValue(value) if value.align() > 1 => {
                        let align = usize_lit(value.align());
                        quote! {
                            if #len_expr != 0 && #data_offset % #align != 0 {
                                ::pinocchio_log::log!(
                                    "Invalid alignment for field {} : element data starts at offset {}, expected {}-byte alignment",
                                    #field_name,
                                    #data_offset,
                                    #align,
                                );
                                return Err(::wheels::DataLayoutError::InvalidFieldAlignment);
                            }
                        }
                    }
                    _ => quote!(),
                };
                match capacity {
                    Capacity::Fixed { .. } => {
                        let capacity_lit = capacity.max_capacity_lit();
                        quote! {
                            Self::__validate_vec_len(bytes, #offset_expr, #capacity_lit, #len_width_lit, #field_name)?;
                            #alignment_check
                            #active_layout_validation
                        }
                    }
                    Capacity::Flexible { .. } => {
                        let capacity_lit = capacity.max_capacity_lit();
                        quote! {
                            Self::__validate_flexible_vec_len(bytes, #offset_expr, #capacity_lit, #len_width_lit, #elem_size, #field_name)?;
                            if bytes.len() != #offset_expr {
                                #alignment_check
                                #active_layout_validation
                            }
                        }
                    }
                }
            }
            Self::Value {
                value,
                optional: None,
            } => value_alignment_check(value, offset_expr, &field_name),
        }
    }

    fn gen_field_encode(
        &self,
        fields_encode_expr: proc_macro2::TokenStream,
        offset: proc_macro2::TokenStream,
        field_ident: &Ident,
    ) -> proc_macro2::TokenStream {
        let offset = quote!((#offset));
        match self {
            Self::Value { value, optional } => {
                let len = usize_lit(value.size());
                match optional {
                    Some(Optional::Fixed) => match value {
                        FixedValueKind::Bool { .. } => quote! {
                            #fields_encode_expr

                            if let Some(value) = &self.#field_ident {
                                bytes[#offset] = 1;
                                bytes[#offset + 1] = u8::from(*value);
                            } else {
                                bytes[#offset] = 0;
                                bytes[#offset + 1] = 0;
                            }
                        },
                        _ => quote! {
                            #fields_encode_expr

                            if let Some(value) = &self.#field_ident {
                                bytes[#offset] = 1;
                                bytes[#offset + 1 .. #offset + 1 + #len].copy_from_slice(::bytemuck::bytes_of(value));
                            } else {
                                bytes[#offset] = 0;
                                bytes[#offset + 1 .. #offset + 1 + #len].fill(0);
                            }
                        },
                    },
                    Some(Optional::Flexible) => match value {
                        FixedValueKind::Bool { .. } => quote! {
                            #fields_encode_expr

                            if let Some(value) = &self.#field_ident {
                                bytes[#offset] = 1;
                                bytes[#offset + 1] = u8::from(*value);
                            }
                        },
                        _ => quote! {
                            #fields_encode_expr

                            if let Some(value) = &self.#field_ident {
                                bytes[#offset] = 1;
                                bytes[#offset + 1 .. #offset + 1 + #len].copy_from_slice(::bytemuck::bytes_of(value));
                            }
                        },
                    },
                    None => match value {
                        FixedValueKind::Bool { .. } => quote! {
                            #fields_encode_expr

                            bytes[#offset] = u8::from(self.#field_ident);
                        },
                        _ => quote! {
                            #fields_encode_expr

                            bytes[#offset..#offset + #len].copy_from_slice(::bytemuck::bytes_of(&self.#field_ident));
                        },
                    },
                }
            }
            Self::Vec { elem, capacity } => {
                let elem_size = elem.size_expr();
                let len_width_ty = capacity.len_width_ty();
                let len_width = capacity.len_width_lit();
                if let Some(cap) = capacity.comptime_capacity_lit() {
                    match elem {
                        FixedVecElementKind::FixedValue(_) => quote! {
                            #fields_encode_expr

                            if self.#field_ident.len() > #cap {
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            }

                            bytes[#offset..#offset + #len_width].copy_from_slice(::bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
                            bytes[#offset + #len_width..#offset + #len_width + self.#field_ident.len() * #elem_size].copy_from_slice(::bytemuck::cast_slice(&self.#field_ident.as_slice()));
                            if self.#field_ident.len() < #cap {
                                 bytes[#offset + #len_width + self.#field_ident.len() * #elem_size..#offset + #len_width + #cap * #elem_size].fill(0);
                            }
                        },
                        FixedVecElementKind::FixedSizeLayout { .. } => quote! {
                            #fields_encode_expr

                            if self.#field_ident.len() > #cap {
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            }

                            bytes[#offset..#offset + #len_width].copy_from_slice(::bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
                            let start = #offset + #len_width;
                            let active_end = start + self.#field_ident.len() * #elem_size;
                            for (index, value) in self.#field_ident.iter().enumerate() {
                                let element_start = start + index * #elem_size;
                                let element_end = element_start + #elem_size;
                                let remaining = ::wheels::layout::Encodable::encode_to(
                                    value,
                                    &mut bytes[element_start..element_end],
                                )?;
                                if !remaining.is_empty() {
                                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                                }
                            }
                            if self.#field_ident.len() < #cap {
                                bytes[active_end..start + #cap * #elem_size].fill(0);
                            }
                        },
                    }
                } else {
                    // it must be the last field of Vec type with #[capacity = flexible]
                    let max_capacity = capacity.max_capacity_lit();
                    match elem {
                        FixedVecElementKind::FixedValue(_) => quote! {
                            #fields_encode_expr

                            if self.#field_ident.len() > #max_capacity {
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            } else if !self.#field_ident.is_empty() {
                                bytes[#offset..#offset + #len_width].copy_from_slice(::bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
                                bytes[#offset + #len_width..#offset + #len_width + self.#field_ident.len() * #elem_size].copy_from_slice(::bytemuck::cast_slice(&self.#field_ident.as_slice()));
                            } else {
                                // Empty flexible Vec omits the length header and payload.
                            }
                        },
                        FixedVecElementKind::FixedSizeLayout { .. } => quote! {
                            #fields_encode_expr

                            if self.#field_ident.len() > #max_capacity {
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            } else if !self.#field_ident.is_empty() {
                                bytes[#offset..#offset + #len_width].copy_from_slice(::bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
                                let start = #offset + #len_width;
                                for (index, value) in self.#field_ident.iter().enumerate() {
                                    let element_start = start + index * #elem_size;
                                    let element_end = element_start + #elem_size;
                                    let remaining = ::wheels::layout::Encodable::encode_to(
                                        value,
                                        &mut bytes[element_start..element_end],
                                    )?;
                                    if !remaining.is_empty() {
                                        return Err(::wheels::DataLayoutError::InvalidDataLength);
                                    }
                                }
                            } else {
                                // Empty flexible Vec omits the length header and payload.
                            }
                        },
                    }
                }
            }
        }
    }

    fn gen_view_methods(
        &self,
        offset: proc_macro2::TokenStream,
        field_ident: &Ident,
    ) -> syn::Result<proc_macro2::TokenStream> {
        let offset = quote!((#offset));
        match self {
            Self::Value { value, optional } => {
                let ty = value.ty();
                let access_mode = value.access_mode();
                let getter_body = getter_tokens(value, offset.clone())?;

                match (optional, access_mode) {
                    (None, AccessMode::Copy) => Ok(quote! {
                        pub fn #field_ident(&self) -> #ty {
                            #getter_body
                        }
                    }),
                    (None, AccessMode::Ref) => Ok(quote! {
                        pub fn #field_ident(&self) -> &#ty {
                            #getter_body
                        }
                    }),
                    (Some(Optional::Fixed), AccessMode::Copy) => {
                        let value_body = getter_tokens(value, quote!(#offset + 1usize))?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<#ty> {
                                (self.bytes[(#offset)] != 0).then(||#value_body)
                            }
                        })
                    }
                    (Some(Optional::Fixed), AccessMode::Ref) => {
                        let value_body = getter_tokens(value, quote!(#offset + 1usize))?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<&#ty> {
                                (self.bytes[(#offset)] != 0).then(||#value_body)
                            }
                        })
                    }
                    (Some(Optional::Flexible), AccessMode::Copy) => {
                        let value_body = getter_tokens(value, quote!(#offset + 1usize))?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<#ty> {
                                if self.bytes.len() == #offset {
                                    return None;
                                }
                                Some(#value_body)
                            }
                        })
                    }
                    (Some(Optional::Flexible), AccessMode::Ref) => {
                        let value_body = getter_tokens(value, quote!(#offset + 1usize))?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<&#ty> {
                                if self.bytes.len() == #offset {
                                    return None;
                                }
                                Some(#value_body)
                            }
                        })
                    }
                }
            }
            Self::Vec { elem, capacity } => {
                let elem_ty = elem.ty();
                let elem_size = elem.size_expr();
                let len_expr = read_len_expr(offset.clone(), capacity.len_width());
                let len_width_lit = capacity.len_width_lit();
                if let Some(cap) = capacity.comptime_capacity_lit() {
                    let capacity_name = format_ident!("{}_capacity", accessor_ident(field_ident));
                    match elem {
                        FixedVecElementKind::FixedValue(_) => Ok(quote! {
                            pub fn #field_ident(&self) -> &[#elem_ty] {
                                let len = #len_expr;
                                let start = #offset + #len_width_lit;
                                let end = start + (len * #elem_size);
                                ::bytemuck::cast_slice::<u8, #elem_ty>(&self.bytes[start..end])
                            }

                            pub const fn #capacity_name(&self) -> usize {
                                #cap
                            }
                        }),
                        FixedVecElementKind::FixedSizeLayout { .. } => Ok(quote! {
                            pub fn #field_ident(
                                &self,
                            ) -> ::wheels::layout::FixedLayoutSlice<'a, #elem_ty> {
                                let len = #len_expr;
                                let start = #offset + #len_width_lit;
                                let end = start + (len * #elem_size);
                                ::wheels::layout::FixedLayoutSlice::new(&self.bytes[start..end])
                                    .expect("validated fixed-size layout Vec")
                            }

                            pub const fn #capacity_name(&self) -> usize {
                                #cap
                            }
                        }),
                    }
                } else {
                    match elem {
                        FixedVecElementKind::FixedValue(_) => Ok(quote! {
                            pub fn #field_ident(&self) -> &[#elem_ty] {
                                if self.bytes.len() == #offset {
                                    return &[];
                                }
                                let len = #len_expr;
                                let start = #offset + #len_width_lit;
                                let end = start + (len * #elem_size);
                                ::bytemuck::cast_slice::<u8, #elem_ty>(&self.bytes[start..end])
                            }
                        }),
                        FixedVecElementKind::FixedSizeLayout { .. } => Ok(quote! {
                            pub fn #field_ident(
                                &self,
                            ) -> ::wheels::layout::FixedLayoutSlice<'a, #elem_ty> {
                                if self.bytes.len() == #offset {
                                    return ::wheels::layout::FixedLayoutSlice::new(&[])
                                        .expect("validated fixed-size layout Vec");
                                }
                                let len = #len_expr;
                                let start = #offset + #len_width_lit;
                                let end = start + (len * #elem_size);
                                ::wheels::layout::FixedLayoutSlice::new(&self.bytes[start..end])
                                    .expect("validated fixed-size layout Vec")
                            }
                        }),
                    }
                }
            }
        }
    }
}

fn parse_field_layout(field: &syn::Field, is_last_field: bool) -> syn::Result<FixedFieldKind> {
    let ty = &field.ty;
    let attribute = parse_field_attr(field, is_last_field)?;

    if let Some(elem_ty) = vec_inner(ty, LAYOUT_NAME)? {
        if is_bool(elem_ty) {
            return Err(syn::Error::new_spanned(
                field,
                "Vec<bool> is not supported by fixed_offset_layout",
            ));
        }
        let attribute = attribute.ok_or_else(|| {
            syn::Error::new_spanned(
                field,
                "Vec fields in fixed_offset_layout require `#[capacity = N]`",
            )
        })?;
        let capacity = match attribute {
            FieldAttribute::Capacity(capacity) => Capacity::Fixed { capacity },
            FieldAttribute::Flexible(Some(len_width)) => Capacity::Flexible { len_width },
            FieldAttribute::Flexible(None) => {
                return Err(syn::Error::new_spanned(
                    field,
                    "Vec fields in fixed_offset_layout require `#[capacity = N]`",
                ))
            }
        };
        let elem = if vec_inner(elem_ty, LAYOUT_NAME)?.is_some() {
            return Err(syn::Error::new_spanned(
                elem_ty,
                "Vec<Vec<T>> is not supported by fixed_offset_layout",
            ));
        } else if is_string(elem_ty) {
            return Err(syn::Error::new_spanned(
                elem_ty,
                "String is not supported by fixed_offset_layout",
            ));
        } else {
            match parse_value_kind(elem_ty, UNSUPPORTED_FIELD_MESSAGE) {
                Ok(value) => FixedVecElementKind::FixedValue(value),
                Err(_) => FixedVecElementKind::FixedSizeLayout {
                    ty: elem_ty.clone(),
                },
            }
        };
        return Ok(FixedFieldKind::Vec { elem, capacity });
    }

    if let Some(inner) = option_inner(ty) {
        let optional = attribute
            .map(|attribute| match attribute {
                FieldAttribute::Flexible(None) => Ok(Optional::Flexible),
                FieldAttribute::Capacity(_) | FieldAttribute::Flexible(Some(_)) => {
                    Err(syn::Error::new_spanned(
                        field,
                        "#[flexible = N] cannot be applied on Option fields",
                    ))
                }
            })
            .transpose()?
            .unwrap_or(Optional::Fixed);

        if vec_inner(inner, LAYOUT_NAME)?.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "Option<Vec<T>> is not supported in fixed_offset_layout",
            ));
        }
        if is_string(inner) {
            return Err(syn::Error::new_spanned(
                field,
                "String is not supported by fixed_offset_layout",
            ));
        }

        return Ok(FixedFieldKind::Value {
            value: parse_value_kind(inner, UNSUPPORTED_FIELD_MESSAGE)?,
            optional: Some(optional),
        });
    }

    if attribute.is_some() {
        return Err(syn::Error::new_spanned(
            field,
            "attributes are allowed on Vec or Option field only",
        ));
    }

    if is_string(ty) {
        return Err(syn::Error::new_spanned(
            field,
            "String is not supported by fixed_offset_layout",
        ));
    }

    Ok(FixedFieldKind::Value {
        value: parse_value_kind(ty, UNSUPPORTED_FIELD_MESSAGE)?,
        optional: None,
    })
}

fn parse_args(attr: &str) -> syn::Result<()> {
    match attr.trim() {
        "" => Ok(()),
        _ => Err(syn::Error::new(
            Span::call_site(),
            "fixed_offset_layout does not support parameters",
        )),
    }
}

#[derive(Copy, Clone)]
enum Capacity {
    Fixed { capacity: usize },

    Flexible { len_width: usize },
}

impl Capacity {
    fn comptime_capacity(self) -> Option<usize> {
        match self {
            Capacity::Fixed { capacity } => Some(capacity),
            Capacity::Flexible { len_width: _ } => None,
        }
    }

    fn comptime_capacity_lit(&self) -> Option<LitInt> {
        self.comptime_capacity().map(usize_lit)
    }

    fn max_capacity(self) -> usize {
        match self {
            Capacity::Fixed { capacity } => capacity,
            Capacity::Flexible { len_width } => 2usize.pow(len_width as u32 * 8) - 1,
        }
    }

    fn max_capacity_lit(self) -> LitInt {
        usize_lit(self.max_capacity())
    }

    fn len_width(self) -> usize {
        match self {
            Capacity::Fixed { capacity } => {
                if capacity <= 0xff {
                    1
                } else {
                    2
                }
            }
            Capacity::Flexible { len_width } => len_width,
        }
    }

    fn len_width_lit(&self) -> LitInt {
        usize_lit(self.len_width())
    }

    fn len_width_ty(&self) -> proc_macro2::TokenStream {
        match self.len_width() {
            1 => quote!(u8),
            2 => quote!(u16),
            _ => unreachable!(),
        }
    }
}

enum FieldAttribute {
    /// #[capacity = N]
    Capacity(usize),

    ///
    /// forms:
    ///
    ///   #[flexible = 1|2]
    ///     applicable on Vec only
    ///     Some(usize) represents it's a Vec and its length is encoded as len_width  bytes
    ///
    ///   #[flexible]
    ///     applicable on Option only
    ///     None represents that is an Option
    ///
    Flexible(Option<usize>),
}

fn parse_field_attr(
    field: &syn::Field,
    is_last_field: bool,
) -> syn::Result<Option<FieldAttribute>> {
    let mut attributes = vec![];
    for attr in &field.attrs {
        if attr.path().is_ident("capacity") {
            let syn::Meta::NameValue(meta) = &attr.meta else {
                return Err(syn::Error::new_spanned(
                    attr,
                    "capacity must use the form `#[capacity = IntLiteral]`",
                ));
            };
            let Expr::Lit(ExprLit {
                lit: Lit::Int(lit_int),
                ..
            }) = &meta.value
            else {
                return Err(syn::Error::new_spanned(
                    attr,
                    "capacity must use the form `#[capacity = IntLiteral]`",
                ));
            };

            let cap: usize = lit_int.base10_parse()?;

            if cap > MAX_CAPACITY {
                return Err(syn::Error::new(
                    field.span(),
                    "capacity above 0xFFFF is not supported (because len_width <= 2)",
                ));
            }

            attributes.push(FieldAttribute::Capacity(cap));
        } else if attr.path().is_ident("flexible") {
            if !is_last_field {
                return Err(syn::Error::new_spanned(
                        field,
                        "#[flexible] or #[flexible = 1|2] is applicable on the last field only if it is an Option or a Vec type",
                    ));
            }
            match &attr.meta {
                syn::Meta::NameValue(meta) => {
                    let Expr::Lit(ExprLit {
                        lit: Lit::Int(lit_int),
                        ..
                    }) = &meta.value
                    else {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "flexible must use the form `#[flexible = 1|2]` (on Vec field) or #[flexible] (on Option field)",
                        ));
                    };

                    let len_width: usize = lit_int.base10_parse()?;

                    if !(1..=MAX_LEN_WIDTH).contains(&len_width) {
                        return Err(syn::Error::new(
                            field.span(),
                            "flexible must be either 1 or 2",
                        ));
                    }
                    attributes.push(FieldAttribute::Flexible(Some(len_width)));
                }
                syn::Meta::Path(_) => {
                    attributes.push(FieldAttribute::Flexible(None));
                }
                _meta => {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "flexible must use the form `#[flexible = 1|2]` (on Vec field) or #[flexible] (on Option field)",
                    ));
                }
            };
        }
    }

    if attributes.len() > 1 {
        Err(syn::Error::new_spanned(
            field,
            "Multiple attributes on a single field not supported",
        ))
    } else {
        Ok(attributes.pop())
    }
}

fn accessor_ident(field_ident: &Ident) -> Ident {
    let field_name = field_ident.to_string();
    let trimmed = field_name.trim_start_matches('_');
    if trimmed.is_empty() {
        format_ident!("{}", field_name)
    } else {
        format_ident!("{}", trimmed)
    }
}

fn value_alignment_check(
    value: &FixedValueKind,
    offset: proc_macro2::TokenStream,
    field_name: &str,
) -> proc_macro2::TokenStream {
    if !matches!(value.access_mode(), AccessMode::Ref) || value.align() <= 1 {
        return quote!();
    }

    let align = usize_lit(value.align());
    quote! {
        if (#offset) % #align != 0 {
            ::pinocchio_log::log!(
                "Invalid alignment for field {} : payload starts at offset {}, expected {}-byte alignment",
                #field_name,
                #offset,
                #align,
            );
            return Err(::wheels::DataLayoutError::InvalidFieldAlignment);
        }
    }
}

fn bytes_slice_expr(offset: proc_macro2::TokenStream, len: usize) -> proc_macro2::TokenStream {
    let len = usize_lit(len);
    quote!(&self.bytes[#offset..#offset + #len])
}

fn getter_tokens(
    value: &FixedValueKind,
    offset: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let ty = value.ty();
    let slice_expr = bytes_slice_expr(offset, value.size());
    match value.access_mode() {
        AccessMode::Copy => read_copy_expr(value, slice_expr, LAYOUT_NAME, INTEGER_FIELD_MESSAGE),
        AccessMode::Ref => Ok(borrow_ref_expr(ty, slice_expr)),
    }
}

fn borrow_ref_expr(ty: &Type, bytes_expr: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    quote!(::bytemuck::from_bytes::<#ty>(#bytes_expr))
}

fn read_len_expr(offset: proc_macro2::TokenStream, len_width: usize) -> proc_macro2::TokenStream {
    match len_width {
        1 => quote!(self.bytes[#offset] as usize),
        2 => quote!({
            let raw: [u8; 2] = self.bytes[#offset..#offset + 2].try_into().expect("validated len");
            u16::from_le_bytes(raw) as usize
        }),
        3 => quote!({
            let mut raw = [0u8; 4];
            raw[0..3].copy_from_slice(&self.bytes[#offset..#offset + 3]);
            u32::from_le_bytes(raw) as usize
        }),
        _ => {
            unreachable!()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::expand_fixed_offset_layout;
    use syn::parse_quote;

    #[test]
    fn fixed_offset_layout_reports_padding_for_large_field() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                payload: [u64; 2],
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `payload` needs 7 byte(s) of padding before it"));
        assert!(error.contains("starts at offset 8"));
    }

    #[test]
    fn fixed_offset_layout_reports_padding_for_optional_payload() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                payload: Option<[u64; 2]>,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `payload` needs 6 byte(s) of padding before it"));
        assert!(error.contains("Option payload would start at offset 2"));
        assert!(error.contains("payload starts at offset 8"));
    }

    #[test]
    fn fixed_offset_layout_reports_padding_for_vec_elements() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                padding: [u8; 7],
                #[capacity = 2]
                values: Vec<[u64; 2]>,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `values` needs 7 byte(s) of padding before it"));
        assert!(error.contains("element 0 would start at offset 9"));
        assert!(error.contains("element 0 starts at offset 16"));
    }

    #[test]
    fn fixed_offset_layout_assumes_earlier_padding_errors_are_fixed() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                payload: [u64; 2],
                _pad1: [u8; 7],
                #[capacity = 2]
                values: Vec<[u64; 2]>,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `payload` needs 7 byte(s) of padding before it"));
        assert!(!error.contains("field `values`"));
    }

    #[test]
    fn fixed_offset_layout_rejects_alignment_above_eight() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                big: u128,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `big` cannot be borrowed by fixed_offset_layout"));
        assert!(error.contains("alignment is 16 byte(s)"));
        assert!(error.contains("8-byte aligned"));
    }

    #[test]
    fn fixed_offset_layout_rejects_vec_without_capacity() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                values: Vec<u16>,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Vec fields in fixed_offset_layout require `#[capacity = N]`"));
    }

    #[test]
    fn fixed_offset_layout_rejects_vec_string() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[capacity = 2]
                values: Vec<String>,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("String is not supported by fixed_offset_layout"));
    }

    #[test]
    fn fixed_offset_layout_rejects_nested_vec() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[capacity = 2]
                values: Vec<Vec<u8>>,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Vec<Vec<T>> is not supported by fixed_offset_layout"));
    }

    #[test]
    fn fixed_offset_layout_rejects_capacity_on_non_vec() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[capacity = 2]
                value: u16,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("attributes are allowed on Vec or Option field only"),
            "error: {}",
            error
        );
    }

    #[test]
    fn fixed_offset_layout_rejects_parameters() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_fixed_offset_layout("mut", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("fixed_offset_layout does not support parameters"));
    }
}
