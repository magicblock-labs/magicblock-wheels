use crate::common::{
    any_child_placement_assertion, ensure_allow_dead_code, impl_where_clause, is_bool, is_string,
    option_inner, parse_value_kind, read_copy_expr, strip_field_attr, usize_lit, vec_inner,
    AccessMode, BufferOffset, FixedValueKind, MAX_SUPPORTED_VEC_LEN,
};
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    parse::Parser, punctuated::Punctuated, spanned::Spanned, Expr, ExprLit, Fields, Ident,
    ItemStruct, Lit, LitInt, Meta, Token, Type,
};

const FIELD_ATTRIBUTES: &[&str] = &["capacity", "extendable", "flexible"];
const LAYOUT_NAME: &str = "fixed_offset_layout";
const UNSUPPORTED_FIELD_MESSAGE: &str =
    "fixed_offset_layout fields must be bool, Pubkey, integer primitives, fixed-size arrays, or a final Option<T> containing a supported layout";
const INTEGER_FIELD_MESSAGE: &str = "field must be an integer primitive";

const MAX_LEN_WIDTH: usize = 8;

const MAX_CAPACITY: usize = 0xffff;

pub(crate) fn expand_fixed_offset_layout(
    attr: &str,
    input: &ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let args = parse_args(attr)?;
    let mut emitted_input = input.clone();
    emitted_input
        .attrs
        .retain(|attr| !attr.path().is_ident("fixed_offset_layout"));

    ensure_allow_dead_code(&mut emitted_input.attrs);

    let struct_name = &emitted_input.ident;
    let layout_placement = args.buffer_offset.layout_placement_impl(struct_name);
    let view_name = format_ident!("{}View", struct_name);
    let view_mut_name = format_ident!("{}ViewMut", struct_name);
    let buffer_offset_validation = match args.buffer_offset {
        BufferOffset::Fixed(buffer_offset) => {
            let buffer_offset_lit = usize_lit(buffer_offset);
            quote! {
                if (bytes.as_ptr() as usize) % 8 != #buffer_offset_lit {
                    ::pinocchio_log::log!(
                        "bytes [ptr_mod_8={}] cannot be deserialized to {} which requires buffer_offset = {} from an 8-byte aligned base",
                        (bytes.as_ptr() as usize) % 8,
                        stringify!(#struct_name),
                        #buffer_offset_lit,
                    );
                    return Err(
                        ::wheels::DataLayoutError::InvalidBufferOffset,
                    );
                }
            }
        }
        BufferOffset::Any => quote!(),
    };

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
    let mut view_mut_methods = Vec::new();

    let mut schema_checks = Vec::new();
    let mut validate_steps = Vec::new();
    let mut layout_error: Option<syn::Error> = None;
    let mut trailing_variable_field = None;
    let field_count = fields.named.len();
    for (index, field) in fields.named.iter_mut().enumerate() {
        let field_ident = field.ident.as_ref().expect("named field");

        let is_last_field = index + 1 == field_count;

        let layout = parse_field_layout(field, is_last_field)?;

        strip_field_attr(&mut field.attrs, FIELD_ATTRIBUTES);

        if let FixedFieldKind::Vec {
            elem,
            capacity: Capacity::Extendable { .. },
        } = &layout
        {
            let size = elem.size_expr();
            let doc = format!("Field `{field_ident}`");
            let message = format!("field `{field_ident}` requires a nonzero encoded element size");
            schema_checks.push(quote! {
                #[doc = #doc]
                const _: () = assert!(#size > 0, #message);
            });
        }

        schema_checks.push(layout.nested_placement_checks(
            &offset_expr,
            args.buffer_offset,
            field_ident,
        ));

        match layout.check_ref_alignment(offset_value, args.buffer_offset, field_ident) {
            Ok(Some(issue)) => {
                if let Some(existing) = &mut layout_error {
                    existing.combine(issue.error);
                } else {
                    layout_error = Some(issue.error);
                }
                offset_value = offset_value.map(|offset| offset + issue.padding);
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

        if offset_value.is_none() {
            schema_checks.push(layout.borrowed_alignment_assertion(
                &offset_expr,
                args.buffer_offset,
                field_ident,
            ));
        }
        validate_steps.push(layout.gen_validate_step(offset_expr.clone(), field_ident));
        view_methods.push(layout.gen_view_methods(offset_expr.clone(), field_ident)?);
        view_mut_methods.push(layout.gen_view_mut_methods(
            offset_expr.clone(),
            offset_value,
            args.buffer_offset,
            field_ident,
        ));

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

                // The final offset marks the start of the trailing count or tag.
                offsets.push(offset_expr.clone());

                trailing_variable_field = Some((field_ident, len_width));
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

    let has_trailing_variable_field = trailing_variable_field.is_some();
    let (short_header_check, decode_prefix_mut) = match &trailing_variable_field {
        Some((_, ComptimeOptionalLen::ArrayLen { len_width, .. })) => (
            {
                // Clippy prefers `len > offset` to `len >= offset + 1` for a one-byte header.
                let has_length_header = if *len_width == 1 {
                    quote!(bytes.len() > #datalen_expr)
                } else {
                    quote!(bytes.len() >= #datalen_expr + #len_width)
                };
                quote! {
                    if #has_length_header {
                        return Err(::wheels::DataLayoutError::MissingCapacityHeader);
                    }
                    if bytes.len() >= #datalen_expr {
                        return Err(::wheels::DataLayoutError::MissingLengthHeader);
                    }
                }
            },
            quote! {
                /// Borrows storage to mutate its first record. Growing the vector moves
                /// any following bytes after the newly reserved slots.
                /// The view's `bytes()` and `storage_len()` cover the entire backing storage.
                pub fn decode_prefix_mut<'a, S>(storage: &'a S)
                    -> core::result::Result<#view_mut_name<'a, S>, ::pinocchio::error::ProgramError>
                where
                    S: ::wheels::layout::LayoutStorageMut + ?Sized,
                {
                    let borrowed = storage.borrow_data_mut()?;
                    Self::__validate_prefix(&borrowed)?;
                    Ok(#view_mut_name { borrowed })
                }
            },
        ),
        _ => (quote!(), quote!()),
    };
    let (datalen_vars, min_len, encoded_len_expr) = match trailing_variable_field {
        Some((field, tail)) => {
            let (header_size, max_datalen) = tail.field_len_bounds();
            let encoded_len = match tail {
                ComptimeOptionalLen::ArrayLen {
                    len_width,
                    elem_size,
                } => {
                    let max_capacity = usize_lit(max_len_for_width(len_width));
                    quote! {
                        let len = self.#field.len();
                        if len > #max_capacity {
                            ::pinocchio_log::log!(
                                "Cannot encode field {}: len {} exceeds max {}",
                                stringify!(#field), len, #max_capacity,
                            );
                            return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                        }
                        Ok(Self::MIN_DATA_LEN + len * #elem_size)
                    }
                }
                ComptimeOptionalLen::Option { value_size } => quote! {
                    Ok(Self::MIN_DATA_LEN + if self.#field.is_some() { #value_size } else { 0 })
                },
                ComptimeOptionalLen::NestedLayoutOption { ty } => quote! {
                    match &self.#field {
                        Some(value) => {
                            let len = ::wheels::layout::Encodable::encoded_len(value)?;
                            if !(<#ty as ::wheels::layout::LayoutBounds>::MIN_DATA_LEN
                                ..=<#ty as ::wheels::layout::LayoutBounds>::MAX_DATA_LEN).contains(&len) {
                                return Err(::wheels::DataLayoutError::InvalidDataLength);
                            }
                            Ok(Self::MIN_DATA_LEN + len)
                        }
                        None => Ok(Self::MIN_DATA_LEN),
                    }
                },
            };
            (
                quote! {
                    /// Minimum encoded length, including the trailing field's headers.
                    pub const MIN_DATA_LEN: usize = #datalen_expr + #header_size;
                    pub const MAX_DATA_LEN: usize = #datalen_expr + #max_datalen;
                },
                quote!(Self::MIN_DATA_LEN),
                encoded_len,
            )
        }
        None => {
            let msg = datalen_value
                .map(|len| format!("Sum of encodable-sizes must be {}.", len))
                .unwrap_or_else(|| {
                    "Sum of encodable-sizes is computed from nested fixed-size layouts.".to_string()
                });
            validate_steps.push(quote!(Ok(Self::DATA_LEN)));
            (
                quote!(#[doc = #msg] pub const DATA_LEN: usize = #datalen_expr;),
                quote!(Self::DATA_LEN),
                quote!(Ok(Self::DATA_LEN)),
            )
        }
    };
    let decode_trait_impls = quote! {
        impl ::wheels::layout::PrefixDecodable for #struct_name {
            type View<'a> = #view_name<'a>;

            fn decode_prefix(bytes: &[u8]) -> core::result::Result<(Self::View<'_>, &[u8]), ::wheels::DataLayoutError> {
                let end = Self::__validate_prefix(bytes)?;
                let (bytes, remaining) = bytes.split_at(end);
                Ok((#view_name { bytes }, remaining))
            }
        }
    };
    let layout_bounds_impl = if has_trailing_variable_field {
        quote! {
            impl ::wheels::layout::LayoutBounds for #struct_name {
                const MIN_DATA_LEN: usize = #struct_name::MIN_DATA_LEN;
                const MAX_DATA_LEN: usize = #struct_name::MAX_DATA_LEN;
            }
        }
    } else {
        quote! {
            impl ::wheels::layout::FixedSizeLayout for #struct_name {
                const DATA_LEN: usize = #struct_name::DATA_LEN;
            }
        }
    };
    let decode_mut_fn = quote! {
        pub fn decode_mut<'a, S>(
            storage: &'a S,
        ) -> core::result::Result<#view_mut_name<'a, S>, ::pinocchio::error::ProgramError>
        where
            S: ::wheels::layout::LayoutStorageMut + ?Sized,
        {
            let borrowed = storage.borrow_data_mut()?;
            <Self as ::wheels::layout::Decodable>::decode(&borrowed)
                .map_err(::pinocchio::error::ProgramError::from)?;

            Ok(#view_mut_name { borrowed })
        }
    };
    let view_mut_bounds = where_bounds.iter();
    let view_mut_type = quote! {
        #[allow(dead_code)]
        pub struct #view_mut_name<'a, S>
        where
            S: ::wheels::layout::LayoutStorageMut + ?Sized + 'a,
        {
            borrowed: S::RefMut<'a>,
        }

        impl<'a, S: ::wheels::layout::LayoutStorageMut + ?Sized> core::fmt::Debug for #view_mut_name<'a, S> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.debug_struct(stringify!(#view_mut_name)).field("storage_len", &self.borrowed.len()).finish()
            }
        }

        impl<'a, S> #view_mut_name<'a, S>
        where
            S: ::wheels::layout::LayoutStorageMut + ?Sized,
            #(#view_mut_bounds,)*
        {
            /// Reads the backing bytes through the borrow held by this view.
            pub fn bytes(&self) -> &[u8] {
                &self.borrowed
            }

            pub fn storage_len(&self) -> usize {
                self.borrowed.len()
            }

            #(#view_mut_methods)*
        }
    };

    Ok(quote! {
        #emitted_input
        #layout_placement
        #(#schema_checks)*

        impl #struct_name {
            #datalen_vars

            #[doc = "Byte offsets marking the start of each field"]
            pub const OFFSETS: [usize; #field_count_expr] = #offsets_expr;

            #decode_mut_fn
            #decode_prefix_mut

            fn __validate_prefix(bytes: &[u8]) -> core::result::Result<usize, ::wheels::DataLayoutError> {
                if bytes.len() < #min_len {
                    ::pinocchio_log::log!(
                        "Truncated {}: need at least {} bytes, found {}",
                        stringify!(#struct_name), #min_len, bytes.len(),
                    );
                    #short_header_check
                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                }
                #buffer_offset_validation
                #(#validate_steps)*
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

            fn __read_vec_len(bytes: &[u8], offset: usize, len_width: usize) -> core::result::Result<usize, ::wheels::DataLayoutError> {
                let mut raw = [0u8; 8];
                raw[..len_width].copy_from_slice(&bytes[offset..offset + len_width]);
                let raw_len = u64::from_le_bytes(raw);
                <usize as core::convert::TryFrom<u64>>::try_from(raw_len)
                    .map_err(|_| ::wheels::DataLayoutError::LengthExceedsCapacity)
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

        #view_mut_type
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
    NestedLayoutOption {
        ty: Type,
        optional: Optional,
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

    fn needs_pod_bound(&self) -> bool {
        match self {
            Self::FixedValue(value) => value.needs_pod_bound(),
            Self::FixedSizeLayout { .. } => false,
        }
    }

    fn extendable_marker(&self) -> proc_macro2::TokenStream {
        match self {
            Self::FixedValue(_) => quote!(::wheels::layout::FixedValueElement),
            Self::FixedSizeLayout { .. } => quote!(::wheels::layout::FixedLayoutElement),
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
        value_size: proc_macro2::TokenStream,
    },
    NestedLayoutOption {
        ty: proc_macro2::TokenStream,
    },
}

impl ComptimeOptionalLen {
    fn field_len_bounds(&self) -> (syn::LitInt, proc_macro2::TokenStream) {
        match self {
            Self::ArrayLen {
                len_width,
                elem_size,
            } => {
                let max_capacity = usize_lit(max_len_for_width(*len_width));
                let header_size = usize_lit(2 * len_width);
                let max = quote!(#header_size + #max_capacity * #elem_size);
                (header_size, max)
            }
            Self::Option { value_size } => (usize_lit(1), quote!(1usize + #value_size)),
            Self::NestedLayoutOption { ty } => (
                usize_lit(1),
                quote!(1usize + <#ty as ::wheels::layout::LayoutBounds>::MAX_DATA_LEN),
            ),
        }
    }
}

struct PaddingIssue {
    padding: usize,
    error: syn::Error,
}

impl FixedFieldKind {
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
                        value_size: value_size_expr,
                    }),
                    None => Ok(SlotLen {
                        expr: value_size_expr,
                        value: Some(value.size()),
                    }),
                }
            }
            Self::NestedLayoutOption { ty, optional } => match optional {
                Optional::Fixed => Ok(SlotLen {
                    expr: quote!((1usize + <#ty as ::wheels::layout::FixedSizeLayout>::DATA_LEN)),
                    value: None,
                }),
                Optional::Flexible => {
                    Err(ComptimeOptionalLen::NestedLayoutOption { ty: quote!(#ty) })
                }
            },
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
            Self::NestedLayoutOption {
                ty,
                optional: Optional::Fixed,
            } => Some(quote!(#ty: ::wheels::layout::FixedSizeLayout)),
            Self::NestedLayoutOption {
                ty,
                optional: Optional::Flexible,
            } => Some(
                quote!(#ty: ::wheels::layout::Encodable + ::wheels::layout::PrefixDecodable + ::wheels::layout::LayoutBounds),
            ),
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
        offset: Option<usize>,
        buffer_offset: BufferOffset,
        field_ident: &Ident,
    ) -> syn::Result<Option<PaddingIssue>> {
        match self {
            Self::Value { value, optional } => {
                if matches!(value.access_mode(), AccessMode::Copy) {
                    return Ok(None);
                }

                let align = value.align();
                if align <= 1 {
                    return Ok(None);
                }
                if matches!(buffer_offset, BufferOffset::Any) {
                    return Err(syn::Error::new(
                        field_ident.span(),
                        format!(
                            "field `{}` cannot be borrowed with `buffer_offset = any`: it requires {}-byte alignment, but the input slice may start at any address",
                            field_ident, align
                        ),
                    ));
                }
                if align > 8 {
                    return Err(syn::Error::new(
                        field_ident.span(),
                        format!(
                            "field `{}` cannot be borrowed with `buffer_offset = {}`: it requires {}-byte alignment, but fixed_offset_layout only assumes the original input buffer is 8-byte aligned",
                            field_ident, buffer_offset.value().expect("fixed buffer_offset"), align
                        ),
                    ));
                }

                let Some(offset) = offset else {
                    return Ok(None);
                };
                let buffer_offset = buffer_offset.value().expect("fixed buffer_offset");
                let payload_offset = offset + usize::from(optional.is_some());
                let misalignment = (buffer_offset + payload_offset) % align;
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
            Self::NestedLayoutOption { .. } => Ok(None),
            Self::Vec { elem, capacity } => {
                let FixedVecElementKind::FixedValue(elem) = elem else {
                    return Ok(None);
                };

                let align = elem.align();
                if align <= 1 {
                    return Ok(None);
                }
                if matches!(buffer_offset, BufferOffset::Any) {
                    return Err(syn::Error::new(
                        field_ident.span(),
                        format!(
                            "field `{}` cannot expose a slice view with `buffer_offset = any`: its elements require {}-byte alignment, but the input slice may start at any address",
                            field_ident, align
                        ),
                    ));
                }
                if align > 8 {
                    return Err(syn::Error::new(
                        field_ident.span(),
                        format!(
                            "field `{}` cannot expose a slice view with `buffer_offset = {}`: its elements require {}-byte alignment, but fixed_offset_layout only assumes the original input buffer is 8-byte aligned",
                            field_ident, buffer_offset.value().expect("fixed buffer_offset"), align
                        ),
                    ));
                }

                let Some(offset) = offset else {
                    return Ok(None);
                };
                let buffer_offset = buffer_offset.value().expect("fixed buffer_offset");
                let header_size = capacity.header_size();
                let first_elem_offset = offset + header_size;
                let misalignment = (buffer_offset + first_elem_offset) % align;
                if misalignment == 0 {
                    return Ok(None);
                }

                let padding = align - misalignment;
                Ok(Some(PaddingIssue {
                    padding,
                    error: syn::Error::new(
                        field_ident.span(),
                        format!(
                        "field `{}` needs {} byte(s) of padding before it: its Vec elements start after a {}-byte header, so element 0 would start at offset {}, but slice views require {}-byte alignment. Insert `_pad: [u8; {}]` before `{}` so element 0 starts at offset {}",
                        field_ident,
                        padding,
                        header_size,
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

    fn nested_placement_checks(
        &self,
        offset: &proc_macro2::TokenStream,
        parent: BufferOffset,
        field: &Ident,
    ) -> proc_macro2::TokenStream {
        let (child, header, multiple_slots) = match self {
            Self::NestedLayoutOption { ty, .. } => (ty, 1, false),
            Self::Vec {
                elem: FixedVecElementKind::FixedSizeLayout { ty },
                capacity,
            } if capacity.max_capacity() > 0 => {
                (ty, capacity.header_size(), capacity.max_capacity() > 1)
            }
            _ => return quote!(),
        };
        let first = nested_offset_assertion(child, field, parent, quote!(#offset + #header));
        if !multiple_slots || parent == BufferOffset::Any {
            return first;
        }
        let doc = format!("Field `{field}`");
        let message = format!("field `{field}` has a stride that places its second child at an incompatible buffer offset");
        quote! {
            #first
            #[doc = #doc]
            const _: () = assert!(
                <#child as ::wheels::layout::LayoutPlacement>::BUFFER_OFFSET.is_none()
                    || <#child as ::wheels::layout::FixedSizeLayout>::DATA_LEN.is_multiple_of(8),
                #message
            );
        }
    }

    fn borrowed_alignment_assertion(
        &self,
        offset: &proc_macro2::TokenStream,
        buffer_offset: BufferOffset,
        field: &Ident,
    ) -> proc_macro2::TokenStream {
        let BufferOffset::Fixed(base) = buffer_offset else {
            return quote!();
        };
        let (value, header) = match self {
            Self::Value { value, optional } if matches!(value.access_mode(), AccessMode::Ref) => {
                (value, usize::from(optional.is_some()))
            }
            Self::Vec {
                elem: FixedVecElementKind::FixedValue(value),
                capacity,
            } => (value, capacity.header_size()),
            _ => return quote!(),
        };
        if value.align() <= 1 {
            return quote!();
        }
        let ty = value.ty();
        let doc = format!("Field `{field}`");
        let message = format!("field `{field}` needs padding before its borrowed payload");
        quote! {
            #[doc = #doc]
            const _: () = assert!((#base + #offset + #header).is_multiple_of(core::mem::align_of::<#ty>()), #message);
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
                optional: Some(Optional::Fixed),
                ..
            } => quote! {
                Self::__validate_option(bytes, #offset_expr, #field_name)?;
            },
            Self::Value {
                value,
                optional: Some(Optional::Flexible),
            } => {
                let value_size = usize_lit(value.size());
                quote! {
                    Self::__validate_option(bytes, #offset_expr, #field_name)?;
                    let end = if bytes[#offset_expr] == 1 {
                        if bytes.len() < Self::MIN_DATA_LEN + #value_size {
                            return Err(::wheels::DataLayoutError::TruncatedPayload);
                        }
                        Self::MIN_DATA_LEN + #value_size
                    } else {
                        Self::MIN_DATA_LEN
                    };
                    Ok(end)
                }
            }
            Self::NestedLayoutOption {
                ty,
                optional: Optional::Fixed,
            } => quote! {
                Self::__validate_option(bytes, #offset_expr, #field_name)?;
                if bytes[#offset_expr] != 0 {
                    let start = #offset_expr + 1usize;
                    let end = start + <#ty as ::wheels::layout::FixedSizeLayout>::DATA_LEN;
                    let _ = <#ty as ::wheels::layout::Decodable>::decode(&bytes[start..end])?;
                }
            },
            Self::NestedLayoutOption {
                ty,
                optional: Optional::Flexible,
            } => quote! {
                Self::__validate_option(bytes, #offset_expr, #field_name)?;
                if bytes[#offset_expr] == 0 {
                    Ok(Self::MIN_DATA_LEN)
                } else {
                    let payload = &bytes[Self::MIN_DATA_LEN..];
                    if payload.len() < <#ty as ::wheels::layout::LayoutBounds>::MIN_DATA_LEN {
                        return Err(::wheels::DataLayoutError::TruncatedPayload);
                    }
                    let (_, remaining) = <#ty as ::wheels::layout::PrefixDecodable>::decode_prefix(payload)?;
                    let child_len = payload.len().checked_sub(remaining.len())
                        .ok_or(::wheels::DataLayoutError::InvalidDataLength)?;
                    if !(<#ty as ::wheels::layout::LayoutBounds>::MIN_DATA_LEN
                        ..=<#ty as ::wheels::layout::LayoutBounds>::MAX_DATA_LEN).contains(&child_len) {
                        return Err(::wheels::DataLayoutError::InvalidDataLength);
                    }
                    Ok(Self::MIN_DATA_LEN + child_len)
                }
            },
            Self::Vec { elem, capacity } => {
                let len_width_lit = capacity.len_width_lit();
                let elem_size = elem.size_expr();
                let read_len = quote!(Self::__read_vec_len(bytes, #offset_expr, #len_width_lit)?);
                let header_size = usize_lit(capacity.header_size());
                let data_offset = quote!(#offset_expr + #header_size);
                let end_expr = quote!(#data_offset + len * #elem_size);
                let active_layout_validation = match elem {
                    FixedVecElementKind::FixedValue(_) => quote!(),
                    FixedVecElementKind::FixedSizeLayout { ty } => quote! {
                        let _ = ::wheels::layout::FixedLayoutSlice::<#ty>::new(
                            &bytes[#data_offset..#end_expr],
                        )?;
                    },
                };
                match capacity {
                    Capacity::Fixed { .. } => {
                        let capacity_lit = capacity.max_capacity_lit();
                        quote! {
                            let len = #read_len;
                            if len > #capacity_lit {
                                ::pinocchio_log::log!(
                                    "Invalid Vec length for field {}: capacity = {}, len = {}",
                                    #field_name, #capacity_lit, len,
                                );
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            }
                            #active_layout_validation
                        }
                    }
                    Capacity::Extendable { .. } => {
                        quote! {
                            let (len, capacity) = ::wheels::layout::decode_extendable_vec_header(
                                bytes, #offset_expr, #len_width_lit, #elem_size,
                            )?;
                            #active_layout_validation
                            Ok(#data_offset + capacity * #elem_size)
                        }
                    }
                }
            }
            Self::Value { optional: None, .. } => quote!(),
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
                            } else {
                                bytes[#offset] = 0;
                            }
                        },
                        _ => quote! {
                            #fields_encode_expr

                            if let Some(value) = &self.#field_ident {
                                bytes[#offset] = 1;
                                bytes[#offset + 1 .. #offset + 1 + #len].copy_from_slice(::bytemuck::bytes_of(value));
                            } else {
                                bytes[#offset] = 0;
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
            Self::NestedLayoutOption { ty, optional } => {
                let value_size = quote!(<#ty as ::wheels::layout::FixedSizeLayout>::DATA_LEN);
                let encode_some = quote! {
                    bytes[#offset] = 1;
                    let start = #offset + 1usize;
                    let end = start + #value_size;
                    let remaining = ::wheels::layout::Encodable::encode_to(
                        value,
                        &mut bytes[start..end],
                    )?;
                    if !remaining.is_empty() {
                        return Err(::wheels::DataLayoutError::InvalidDataLength);
                    }
                };
                match optional {
                    Optional::Fixed => quote! {
                        #fields_encode_expr

                        if let Some(value) = &self.#field_ident {
                            #encode_some
                        } else {
                            bytes[#offset] = 0;
                            bytes[#offset + 1usize..#offset + 1usize + #value_size].fill(0);
                        }
                    },
                    Optional::Flexible => quote! {
                        #fields_encode_expr

                        if let Some(value) = &self.#field_ident {
                            bytes[#offset] = 1;
                            let start = #offset + 1usize;
                            let remaining = ::wheels::layout::Encodable::encode_to(
                                value,
                                &mut bytes[start..],
                            )?;
                            if !remaining.is_empty() {
                                return Err(::wheels::DataLayoutError::InvalidDataLength);
                            }
                        } else {
                            bytes[#offset] = 0;
                        }
                    },
                }
            }
            Self::Vec { elem, capacity } => {
                let elem_size = elem.size_expr();
                let len_width = capacity.len_width_lit();
                if let Some(cap) = capacity.comptime_capacity_lit() {
                    match elem {
                        FixedVecElementKind::FixedValue(_) => quote! {
                            #fields_encode_expr

                            if self.#field_ident.len() > #cap {
                                return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                            }

                            let len_header = (self.#field_ident.len() as u64).to_le_bytes();
                            bytes[#offset..#offset + #len_width].copy_from_slice(&len_header[..#len_width]);
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

                            let len_header = (self.#field_ident.len() as u64).to_le_bytes();
                            bytes[#offset..#offset + #len_width].copy_from_slice(&len_header[..#len_width]);
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
                    let write_elements = match elem {
                        FixedVecElementKind::FixedValue(_) => quote! {
                            bytes[start..].copy_from_slice(::bytemuck::cast_slice(self.#field_ident.as_slice()));
                        },
                        FixedVecElementKind::FixedSizeLayout { .. } => quote! {
                            for (index, value) in self.#field_ident.iter().enumerate() {
                                let element_start = start + index * #elem_size;
                                let remaining = ::wheels::layout::Encodable::encode_to(
                                    value, &mut bytes[element_start..element_start + #elem_size],
                                )?;
                                if !remaining.is_empty() {
                                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                                }
                            }
                        },
                    };
                    quote! {
                        #fields_encode_expr
                        let capacity_offset = #offset + #len_width;
                        let start = capacity_offset + #len_width;
                        let count = (self.#field_ident.len() as u64).to_le_bytes();
                        bytes[#offset..capacity_offset].copy_from_slice(&count[..#len_width]);
                        bytes[capacity_offset..start].copy_from_slice(&count[..#len_width]);
                        #write_elements
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
                    (Some(_), AccessMode::Copy) => {
                        let value_body = getter_tokens(value, quote!(#offset + 1usize))?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<#ty> {
                                (self.bytes[(#offset)] != 0).then(||#value_body)
                            }
                        })
                    }
                    (Some(_), AccessMode::Ref) => {
                        let value_body = getter_tokens(value, quote!(#offset + 1usize))?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<&#ty> {
                                (self.bytes[(#offset)] != 0).then(||#value_body)
                            }
                        })
                    }
                }
            }
            Self::NestedLayoutOption { ty, optional } => {
                let fixed_view = quote!(<#ty as ::wheels::layout::Decodable>::View<'a>);
                let flexible_view = quote!(<#ty as ::wheels::layout::PrefixDecodable>::View<'a>);
                match optional {
                    Optional::Fixed => Ok(quote! {
                        pub fn #field_ident(&self) -> core::option::Option<#fixed_view> {
                            (self.bytes[#offset] != 0).then(|| {
                                let start = #offset + 1usize;
                                let end = start + <#ty as ::wheels::layout::FixedSizeLayout>::DATA_LEN;
                                <#ty as ::wheels::layout::Decodable>::decode(&self.bytes[start..end])
                                    .expect("validated nested fixed-layout Option")
                            })
                        }
                    }),
                    Optional::Flexible => Ok(quote! {
                        pub fn #field_ident(&self) -> core::option::Option<#flexible_view> {
                            if self.bytes[#offset] == 0 {
                                return None;
                            }
                            let (child, _) = <#ty as ::wheels::layout::PrefixDecodable>::decode_prefix(
                                &self.bytes[#offset + 1usize..],
                            ).expect("validated nested flexible Option");
                            Some(child)
                        }
                    }),
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
                                if len == 0 {
                                    return &[];
                                }
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
                            pub fn #field_ident(
                                &self,
                            ) -> ::wheels::layout::ExtendableVec<
                                'a,
                                #elem_ty,
                                (),
                                ::wheels::layout::FixedValueElement,
                            > {
                                ::wheels::layout::ExtendableVec::new_fixed_value(
                                    self.bytes,
                                    #offset,
                                    #len_width_lit,
                                    #elem_size,
                                )
                                .expect("validated extendable Vec")
                            }
                        }),
                        FixedVecElementKind::FixedSizeLayout { .. } => Ok(quote! {
                            pub fn #field_ident(
                                &self,
                            ) -> ::wheels::layout::ExtendableVec<
                                'a,
                                #elem_ty,
                                (),
                                ::wheels::layout::FixedLayoutElement,
                            > {
                                ::wheels::layout::ExtendableVec::new_fixed_layout(
                                    self.bytes,
                                    #offset,
                                    #len_width_lit,
                                    #elem_size,
                                )
                                .expect("validated extendable Vec")
                            }
                        }),
                    }
                }
            }
        }
    }

    fn gen_view_mut_methods(
        &self,
        offset: proc_macro2::TokenStream,
        offset_value: Option<usize>,
        buffer_offset: BufferOffset,
        field_ident: &Ident,
    ) -> proc_macro2::TokenStream {
        let offset = quote!((#offset));
        let field_mut_ident = format_ident!("{}_mut", field_ident);

        let guard = quote!(S::RefMut<'a>);
        let flexible_slot =
            quote!(::wheels::layout::FlexibleOptionBytes::new(&mut self.borrowed, #offset));
        let (handle, field) = match self {
            Self::Value {
                value,
                optional: None,
            } => {
                let ty = value.ty();
                let size = usize_lit(value.size());
                let slot = quote!(&mut self.borrowed[#offset..#offset + #size]);
                if matches!(value, FixedValueKind::Bool { .. }) {
                    (
                        quote!(::wheels::layout::LayoutBoolMut<'_>),
                        quote!(::wheels::layout::LayoutBoolMut::new(&mut self.borrowed[#offset])),
                    )
                } else if can_mut_borrow_value(value, offset_value, buffer_offset) {
                    (quote!(&mut #ty), quote!(::bytemuck::from_bytes_mut(#slot)))
                } else {
                    (
                        quote!(::wheels::layout::LayoutCopyMut<'_, #ty>),
                        quote!(::wheels::layout::LayoutCopyMut::new(#slot)),
                    )
                }
            }
            Self::Value {
                value,
                optional: Some(optional),
            } => {
                let (slot_ty, slot) = match optional {
                    Optional::Fixed => {
                        let size = usize_lit(1 + value.size());
                        (
                            quote!(&mut [u8]),
                            quote!(&mut self.borrowed[#offset..#offset + #size]),
                        )
                    }
                    Optional::Flexible => (
                        quote!(::wheels::layout::FlexibleOptionBytes<'_, #guard>),
                        flexible_slot,
                    ),
                };
                if matches!(value, FixedValueKind::Bool { .. }) {
                    (
                        quote!(::wheels::layout::LayoutBoolOptionMut<#slot_ty>),
                        quote!(::wheels::layout::LayoutBoolOptionMut::new(#slot)),
                    )
                } else {
                    let ty = value.ty();
                    (
                        quote!(::wheels::layout::LayoutOptionMut<#slot_ty, #ty>),
                        quote!(::wheels::layout::LayoutOptionMut::new(#slot)),
                    )
                }
            }
            Self::NestedLayoutOption {
                ty,
                optional: Optional::Fixed,
            } => {
                let size = quote!(1 + <#ty as ::wheels::layout::FixedSizeLayout>::DATA_LEN);
                (
                    quote!(::wheels::layout::FixedLayoutOptionMut<'_, #ty>),
                    quote!(::wheels::layout::FixedLayoutOptionMut::new(&mut self.borrowed[#offset..#offset + #size])),
                )
            }
            Self::NestedLayoutOption {
                ty,
                optional: Optional::Flexible,
            } => (
                quote!(::wheels::layout::FlexibleLayoutOptionMut<'_, #guard, #ty>),
                quote!(::wheels::layout::FlexibleLayoutOptionMut::new(#flexible_slot)),
            ),
            Self::Vec { elem, capacity } => {
                let elem_ty = elem.ty();
                let elem_size = elem.size_expr();
                let marker = elem.extendable_marker();
                let len_width = capacity.len_width_lit();
                match capacity {
                    Capacity::Fixed { .. } => {
                        let max = capacity.max_capacity_lit();
                        (
                            quote!(::wheels::layout::FixedCapacityVec<'_, #elem_ty, #marker>),
                            quote!(::wheels::layout::FixedCapacityVec::new(&mut self.borrowed, #offset, #len_width, #elem_size, #max)),
                        )
                    }
                    Capacity::Extendable { .. } => (
                        quote!(::wheels::layout::ExtendableVec<'_, #elem_ty, #guard, #marker>),
                        quote!(::wheels::layout::ExtendableVec::new_storage::<{ #elem_size }>(&mut self.borrowed, #offset, #len_width)),
                    ),
                }
            }
        };
        quote! {
            pub fn #field_mut_ident(&mut self) -> #handle {
                #field
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
                "Vec fields in fixed_offset_layout require `#[capacity = N]` or `#[extendable = N]`",
            )
        })?;
        let capacity = match attribute {
            FieldAttribute::Capacity(capacity) => Capacity::Fixed { capacity },
            FieldAttribute::Extendable(len_width) => Capacity::Extendable { len_width },
            FieldAttribute::FlexibleOption => {
                return Err(syn::Error::new_spanned(
                    field,
                    "Vec fields in fixed_offset_layout require `#[capacity = N]` or `#[extendable = N]`",
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
                FieldAttribute::FlexibleOption => Ok(Optional::Flexible),
                FieldAttribute::Capacity(_) | FieldAttribute::Extendable(_) => {
                    Err(syn::Error::new_spanned(
                        field,
                        "#[extendable = N] cannot be applied on Option fields",
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

        return match parse_value_kind(inner, UNSUPPORTED_FIELD_MESSAGE) {
            Ok(value) => Ok(FixedFieldKind::Value {
                value,
                optional: Some(optional),
            }),
            Err(_) if is_last_field => Ok(FixedFieldKind::NestedLayoutOption {
                ty: inner.clone(),
                optional,
            }),
            Err(_) => Err(syn::Error::new_spanned(
                field,
                "Option<T> containing a user-defined layout must be the final field",
            )),
        };
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutArgs {
    buffer_offset: BufferOffset,
}

fn parse_args(attr: &str) -> syn::Result<LayoutArgs> {
    if attr.trim().is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "fixed_offset_layout requires `buffer_offset = 0..=7` or `buffer_offset = any`",
        ));
    }

    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse_str(attr)?;
    let mut buffer_offset = None;

    for meta in metas {
        match meta {
            Meta::NameValue(meta) if meta.path.is_ident("buffer_offset") => match meta.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Int(lit_int),
                    ..
                }) => {
                    let value: usize = lit_int.base10_parse()?;
                    if value <= 7 {
                        buffer_offset = Some(BufferOffset::Fixed(value));
                    } else {
                        return Err(syn::Error::new_spanned(
                            lit_int,
                            "buffer_offset must be in the range 0..=7",
                        ));
                    }
                }
                Expr::Path(value) => {
                    let Some(ident) = value.path.get_ident() else {
                        return Err(syn::Error::new_spanned(
                            value,
                            "buffer_offset must be an integer in 0..=7 or `any`",
                        ));
                    };

                    if ident == "any" {
                        buffer_offset = Some(BufferOffset::Any);
                    } else {
                        return Err(syn::Error::new_spanned(
                            ident,
                            "buffer_offset must be an integer in 0..=7 or `any`",
                        ));
                    }
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "buffer_offset must be an integer in 0..=7 or `any`",
                    ));
                }
            },
            _ => {
                return Err(syn::Error::new_spanned(
                    meta,
                    "fixed_offset_layout only supports `buffer_offset = 0..=7` and `buffer_offset = any`",
                ))
            }
        }
    }

    let Some(buffer_offset) = buffer_offset else {
        return Err(syn::Error::new(
            Span::call_site(),
            "fixed_offset_layout requires `buffer_offset = 0..=7` or `buffer_offset = any`",
        ));
    };

    Ok(LayoutArgs { buffer_offset })
}

#[derive(Copy, Clone)]
enum Capacity {
    Fixed { capacity: usize },

    Extendable { len_width: usize },
}

impl Capacity {
    fn comptime_capacity(self) -> Option<usize> {
        match self {
            Capacity::Fixed { capacity } => Some(capacity),
            Capacity::Extendable { len_width: _ } => None,
        }
    }

    fn comptime_capacity_lit(&self) -> Option<LitInt> {
        self.comptime_capacity().map(usize_lit)
    }

    fn max_capacity(self) -> usize {
        match self {
            Capacity::Fixed { capacity } => capacity,
            Capacity::Extendable { len_width } => max_len_for_width(len_width),
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
            Capacity::Extendable { len_width } => len_width,
        }
    }

    fn header_size(self) -> usize {
        match self {
            Self::Fixed { .. } => self.len_width(),
            Self::Extendable { len_width } => 2 * len_width,
        }
    }

    fn len_width_lit(&self) -> LitInt {
        usize_lit(self.len_width())
    }
}

fn max_len_for_width(len_width: usize) -> usize {
    match len_width {
        8 => MAX_SUPPORTED_VEC_LEN,
        _ => (1usize << (len_width * 8)) - 1,
    }
}

enum FieldAttribute {
    /// #[capacity = N]
    Capacity(usize),

    /// #[extendable = 1..=8]
    Extendable(usize),

    /// #[flexible]
    FlexibleOption,
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

            if !(1..=MAX_CAPACITY).contains(&cap) {
                let name = field.ident.as_ref().expect("named field");
                return Err(syn::Error::new(
                    lit_int.span(),
                    format!("field `{name}` requires a capacity in the range 1..={MAX_CAPACITY}"),
                ));
            }

            attributes.push(FieldAttribute::Capacity(cap));
        } else if attr.path().is_ident("extendable") {
            if !is_last_field {
                return Err(syn::Error::new_spanned(
                    field,
                    "#[extendable = 1..=8] is applicable only on the last field when it is a Vec type",
                ));
            }

            let syn::Meta::NameValue(meta) = &attr.meta else {
                return Err(syn::Error::new_spanned(
                    attr,
                    "extendable must use the form `#[extendable = 1..=8]`",
                ));
            };
            let Expr::Lit(ExprLit {
                lit: Lit::Int(lit_int),
                ..
            }) = &meta.value
            else {
                return Err(syn::Error::new_spanned(
                    attr,
                    "extendable must use the form `#[extendable = 1..=8]`",
                ));
            };

            let len_width: usize = lit_int.base10_parse()?;

            if !(1..=MAX_LEN_WIDTH).contains(&len_width) {
                return Err(syn::Error::new(
                    field.span(),
                    "extendable must be in the range 1..=8",
                ));
            }
            attributes.push(FieldAttribute::Extendable(len_width));
        } else if attr.path().is_ident("flexible") {
            if !is_last_field {
                return Err(syn::Error::new_spanned(
                    field,
                    "#[flexible] is applicable only on the last field when it is an Option type",
                ));
            }
            if !matches!(&attr.meta, syn::Meta::Path(_)) {
                return Err(syn::Error::new_spanned(
                    attr,
                    "`#[flexible = N]` is not supported by fixed_offset_layout; use `#[flexible]` for a final Option field or `#[extendable = N]` for a final extendable Vec field",
                ));
            }
            attributes.push(FieldAttribute::FlexibleOption);
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

fn can_mut_borrow_value(
    value: &FixedValueKind,
    offset: Option<usize>,
    buffer_offset: BufferOffset,
) -> bool {
    if matches!(value, FixedValueKind::Bool { .. }) {
        return false;
    }

    let align = value.align();
    if align <= 1 {
        return true;
    }
    if align > 8 {
        return false;
    }

    let (Some(offset), BufferOffset::Fixed(buffer_offset)) = (offset, buffer_offset) else {
        return false;
    };

    (buffer_offset + offset) % align == 0
}

fn nested_offset_assertion(
    child: &Type,
    field: &Ident,
    parent: BufferOffset,
    payload_offset: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let BufferOffset::Fixed(base) = parent else {
        return any_child_placement_assertion(child, field);
    };
    let doc = format!("Field `{field}`");
    let message = format!("field `{field}` places its child at an incompatible buffer offset");
    quote! {
        #[doc = #doc]
        const _: () = {
            let required = <#child as ::wheels::layout::LayoutPlacement>::BUFFER_OFFSET;
            let actual = (#base + #payload_offset) % 8;
            let compatible = {
                use ::core::option::Option::{None, Some};
                match required {
                    Some(offset) => actual == offset,
                    None => true,
                }
            };
            assert!(compatible, #message);
        };
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
    let len_width = usize_lit(len_width);
    quote!({
        let mut raw = [0u8; 8];
        raw[..#len_width].copy_from_slice(&self.bytes[#offset..#offset + #len_width]);
        let raw_len = u64::from_le_bytes(raw);
        <usize as core::convert::TryFrom<u64>>::try_from(raw_len).expect("validated len header")
    })
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

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `payload` needs 7 byte(s) of padding before it"));
        assert!(error.contains("starts at offset 8"));
    }

    #[test]
    fn fixed_offset_layout_uses_buffer_offset_for_padding() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                payload: [u64; 2],
            }
        };

        expand_fixed_offset_layout("buffer_offset = 7", &item).unwrap();
    }

    #[test]
    fn fixed_offset_layout_rejects_alignment_sensitive_field_with_any_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                payload: [u64; 2],
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = any", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `payload` cannot be borrowed with `buffer_offset = any`"));
    }

    #[test]
    fn fixed_offset_layout_accepts_copy_only_fields_with_any_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                amount: u64,
                counter: u32,
            }
        };

        expand_fixed_offset_layout("buffer_offset = any", &item).unwrap();
    }

    #[test]
    fn fixed_offset_layout_accepts_extendable_len_width_eight() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
                #[extendable = 8]
                values: Vec<u8>,
            }
        };

        expand_fixed_offset_layout("buffer_offset = 0", &item).unwrap();
    }

    #[test]
    fn fixed_offset_layout_rejects_invalid_extendable_len_width() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
                #[extendable = 9]
                values: Vec<u8>,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("extendable must be in the range 1..=8"));
    }

    #[test]
    fn fixed_offset_layout_rejects_flexible_len_width_for_vec() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
                #[flexible = 1]
                values: Vec<u8>,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`#[extendable = N]`"));
    }

    #[test]
    fn fixed_offset_layout_reports_padding_for_optional_payload() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                payload: Option<[u64; 2]>,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
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

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
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

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
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

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `big` cannot be borrowed with `buffer_offset = 0`"));
        assert!(error.contains("requires 16-byte alignment"));
        assert!(error.contains("8-byte aligned"));
    }

    #[test]
    fn fixed_offset_layout_rejects_vec_without_capacity() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                values: Vec<u16>,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "Vec fields in fixed_offset_layout require `#[capacity = N]` or `#[extendable = N]`"
        ));
    }

    #[test]
    fn fixed_offset_layout_rejects_zero_capacity() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[capacity = 0]
                items: Vec<u8>,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = any", &item).unwrap_err();
        assert_eq!(
            error.to_string(),
            "field `items` requires a capacity in the range 1..=65535"
        );
    }

    #[test]
    fn fixed_offset_layout_rejects_vec_string() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[capacity = 2]
                values: Vec<String>,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
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

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
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

        let error = expand_fixed_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("attributes are allowed on Vec or Option field only"),
            "error: {}",
            error
        );
    }

    #[test]
    fn fixed_offset_layout_requires_buffer_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_fixed_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "fixed_offset_layout requires `buffer_offset = 0..=7` or `buffer_offset = any`"
        ));
    }

    #[test]
    fn fixed_offset_layout_rejects_invalid_buffer_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_fixed_offset_layout("buffer_offset = 8", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("buffer_offset must be in the range 0..=7"));
    }

    #[test]
    fn fixed_offset_layout_rejects_unsupported_buffer_offset_identifiers() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        for args in ["buffer_offset = dynamic", "buffer_offset = unknown"] {
            let error = expand_fixed_offset_layout(args, &item)
                .unwrap_err()
                .to_string();
            assert!(error.contains("buffer_offset must be an integer in 0..=7 or `any`"));
        }
    }

    #[test]
    fn fixed_offset_layout_rejects_unknown_parameters() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_fixed_offset_layout("option = implicit", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "fixed_offset_layout only supports `buffer_offset = 0..=7` and `buffer_offset = any`"
        ));
    }
}
