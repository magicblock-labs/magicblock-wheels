use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
};

use crate::common::{
    ensure_allow_dead_code, impl_where_clause, is_bool, is_string, option_inner, parse_value_kind,
    read_copy_expr, strip_field_attr, usize_lit, vec_inner, AccessMode, FixedValueKind,
};
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    parse::Parser, punctuated::Punctuated, spanned::Spanned, Expr, ExprLit, Fields, Ident,
    ItemStruct, Lit, Meta, Token,
};

const FIELD_ATTRIBUTES: &[&str] = &["capacity", "flexible"];
const LAYOUT_NAME: &str = "variable_offset_layout";
const UNSUPPORTED_FIELD_MESSAGE: &str =
    "variable_offset_layout fields must be bool, Pubkey, integer primitives, or fixed-size arrays of integer primitives";
const INTEGER_FIELD_MESSAGE: &str = "field must be an integer primitive or bool";

const MAX_LEN_WIDTH: usize = 8;

pub(crate) fn expand_variable_offset_layout(
    attr: &str,
    input: &ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let args = parse_args(attr)?;
    let mut emitted_input = input.clone();
    emitted_input
        .attrs
        .retain(|attr| !attr.path().is_ident("variable_offset_layout"));

    ensure_allow_dead_code(&mut emitted_input.attrs);

    let struct_name = &emitted_input.ident;
    let view_name = format_ident!("{}View", struct_name);
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
        BufferOffset::Unaligned => quote!(),
    };

    let Fields::Named(fields) = &mut emitted_input.fields else {
        return Err(syn::Error::new_spanned(
            &emitted_input.fields,
            "variable_offset_layout requires named fields",
        ));
    };

    let mut offset = 0usize;

    let mut where_bounds = Vec::<proc_macro2::TokenStream>::new();
    let mut view_methods = Vec::new();
    let mut validate_steps = Vec::new();
    let mut encoded_len_steps = Vec::new();
    let mut encode_steps = Vec::new();

    let mut min_datalen: usize = 0;
    let mut max_datalen: usize = 0;
    let mut min_datalen_expr = quote!();
    let mut max_datalen_expr = quote!();
    let mut update_maxmin_datalen =
        |(slot_min_len_expr, slot_min_len), (slot_max_len_expr, slot_max_len)| {
            min_datalen += slot_min_len;
            max_datalen += slot_max_len;
            if min_datalen_expr.is_empty() {
                min_datalen_expr = slot_min_len_expr;
                max_datalen_expr = slot_max_len_expr;
            } else {
                min_datalen_expr = quote!(#min_datalen_expr + #slot_min_len_expr);
                max_datalen_expr = quote!(#max_datalen_expr + #slot_max_len_expr);
            }
        };

    let mut field_offsets = vec![];
    let mut seen_variable_sized_field = false;

    let mut implicit_option_index = 0usize;
    let field_layouts = fields
        .named
        .iter()
        .map(|field| parse_field_layout(field, args.option_encoding, &mut implicit_option_index))
        .collect::<syn::Result<Vec<_>>>()?;

    validate_struct_options(struct_name, &fields.named, &field_layouts, &args)?;
    validate_borrowed_field_alignment(struct_name, &fields.named, &field_layouts, &args)?;

    for (index, (field, field_layout)) in fields
        .named
        .iter_mut()
        .zip(field_layouts.iter())
        .enumerate()
    {
        let field_ident = field.ident.as_ref().expect("named field");

        strip_field_attr(&mut field.attrs, FIELD_ATTRIBUTES);

        if seen_variable_sized_field {
            field_offsets.push(FieldOffset::VariableOffset {
                layout: field_layout.clone(),
            });
        } else {
            field_offsets.push(FieldOffset::FixedOffset {
                offset,
                layout: field_layout.clone(),
            });
        }

        match field_layout.slot_minmax_len() {
            Ok((slot_len_expr, slot_len)) => {
                offset += slot_len;
                update_maxmin_datalen((slot_len_expr.clone(), slot_len), (slot_len_expr, slot_len));
            }
            Err(((slot_min_len_expr, slot_min_len), (slot_max_len_expr, slot_max_len))) => {
                seen_variable_sized_field = true;
                update_maxmin_datalen(
                    (slot_min_len_expr, slot_min_len),
                    (slot_max_len_expr, slot_max_len),
                );
            }
        }

        validate_steps.push(field_layout.gen_validate_step(struct_name, field_ident));
        encoded_len_steps.push(field_layout.gen_encoded_len_step(field_ident));
        encode_steps.push(field_layout.gen_field_encode_step(field_ident));
        view_methods.push(field_layout.gen_view_methods(
            struct_name,
            field_ident,
            &field_offsets[..=index],
        )?);

        if let Some(bound) = field_layout.bound() {
            where_bounds.push(bound);
        }
    }

    let where_clause = impl_where_clause(&where_bounds);

    let implicit_len_helpers = implicit_len_helpers(min_datalen, &fields.named, &field_layouts)?;
    let implicit_len_validation =
        implicit_len_validation(struct_name, min_datalen, &fields.named, &field_layouts)?;

    let public_len_const = public_len_const(
        min_datalen_expr.clone(),
        min_datalen,
        max_datalen_expr.clone(),
        max_datalen,
        &field_layouts,
    )?;
    let data_len_validation =
        data_len_validation(struct_name, min_datalen, max_datalen, &field_layouts)?;
    // Implicit options infer presence from total length, so prefix decoding
    // remains exact-slice only until a framing policy is chosen.
    let prefix_len_validation = if args.option_encoding == StructOptionEncoding::Implicit {
        quote! {
            #data_len_validation
            #implicit_len_validation
        }
    } else {
        quote!()
    };
    let decodable_impl = if args.option_encoding == StructOptionEncoding::Implicit {
        quote! {
            impl ::wheels::layout::Decodable for #struct_name {
                type View<'a> = #view_name<'a>;

                fn decode<'a>(
                    bytes: &'a [u8]
                ) -> core::result::Result<Self::View<'a>, ::wheels::DataLayoutError> {
                    let encoded_len = Self::__validate_prefix(bytes)?;
                    if encoded_len != bytes.len() {
                        return Err(::wheels::DataLayoutError::InvalidDataLength);
                    }
                    Ok(#view_name { bytes })
                }
            }
        }
    } else {
        quote! {
            impl ::wheels::layout::PrefixDecodable for #struct_name {
                type View<'a> = #view_name<'a>;

                fn decode_prefix<'a>(
                    bytes: &'a [u8]
                ) -> core::result::Result<(Self::View<'a>, &'a [u8]), ::wheels::DataLayoutError> {
                    let encoded_len = Self::__validate_prefix(bytes)?;
                    let (bytes, remaining) = bytes.split_at(encoded_len);
                    Ok((#view_name { bytes }, remaining))
                }
            }
        }
    };

    Ok(quote! {
        #emitted_input

        impl #struct_name {
            const __MIN_DATA_LEN: usize = #min_datalen_expr;
            const __MAX_DATA_LEN: usize = #max_datalen_expr;

            #public_len_const

            fn __validate_prefix(
                bytes: &[u8],
            ) -> core::result::Result<usize, ::wheels::DataLayoutError> {
                #buffer_offset_validation

                #prefix_len_validation

                let mut offset = 0usize;
                #(#validate_steps)*

                Ok(offset)
            }

            fn __read_len_header_unchecked(
                bytes: &[u8],
                offset: usize,
                len_width: usize,
            ) -> usize {
                let mut raw = [0u8; 8];
                raw[..len_width].copy_from_slice(&bytes[offset..offset + len_width]);
                let raw_len = u64::from_le_bytes(raw);
                <usize as core::convert::TryFrom<u64>>::try_from(raw_len)
                    .expect("validated len header")
            }

            fn __read_len_header(
                bytes: &[u8],
                offset: usize,
                len_width: usize,
                capacity: usize,
                field_name: &str,
            ) -> core::result::Result<usize, ::wheels::DataLayoutError> {
                let mut raw = [0u8; 8];
                raw[..len_width].copy_from_slice(&bytes[offset..offset + len_width]);
                let raw_len = u64::from_le_bytes(raw);
                match <usize as core::convert::TryFrom<u64>>::try_from(raw_len) {
                    Ok(len) => {
                        if len > capacity {
                            ::pinocchio_log::log!(
                                "Length header for field {} encodes {} which exceeds capacity {}",
                                field_name,
                                len,
                                capacity,
                            );
                            Err(::wheels::DataLayoutError::LengthExceedsCapacity)
                        } else {
                            Ok(len)
                        }
                    }
                    Err(_) => {
                        ::pinocchio_log::log!(
                            "Length header for field {} encodes {} which exceeds this target's capacity",
                            field_name,
                            raw_len,
                        );
                        Err(::wheels::DataLayoutError::LengthExceedsCapacity)
                    }
                }
            }

            #implicit_len_helpers
        }

        impl ::wheels::layout::Encodable for #struct_name {

            fn encoded_len(
                &self,
            ) -> core::result::Result<usize, ::wheels::DataLayoutError> {
                let mut len = 0usize;
                #(#encoded_len_steps)*
                Ok(len)
            }

            fn encode_to<'a>(
                &self,
                bytes: &'a mut [u8],
            ) -> core::result::Result<&'a mut [u8], ::wheels::DataLayoutError> {
                let encoded_len = self.encoded_len()?;
                if bytes.len() < encoded_len {
                    ::pinocchio_log::log!(
                        "bytes [len={}] are too small to encode {} which needs {} bytes",
                        bytes.len(),
                        stringify!(#struct_name),
                        encoded_len,
                    );
                    return Err(::wheels::DataLayoutError::OutputBufferTooSmall);
                }
                let (bytes, remaining) = bytes.split_at_mut(encoded_len);

                let mut offset = 0usize;
                #(#encode_steps)*
                Ok(remaining)
            }
        }

        #decodable_impl

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

#[derive(Clone)]
enum FieldOffset {
    // A field gets this value only if there is no variable-len field before it.
    // Once a variable-len field is seen, all other fields will be VariableOffset.
    FixedOffset { offset: usize, layout: FieldLayout },

    // Note that it depends on the order of fields, e.g
    // if a u64 appears after Vec<u8> (or Option<u16>) field, then it will
    // be treated as VariableOffset as the offset for this field is fixed anymore.
    VariableOffset { layout: FieldLayout },
}

impl FieldOffset {
    fn fixed_offset(&self) -> Option<usize> {
        let FieldOffset::FixedOffset { offset, .. } = self else {
            return None;
        };
        Some(*offset)
    }

    fn layout(&self) -> &FieldLayout {
        match self {
            FieldOffset::FixedOffset { layout, .. } => layout,
            FieldOffset::VariableOffset { layout } => layout,
        }
    }
}

impl Debug for FieldOffset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldOffset::FixedOffset { offset, .. } => {
                write!(f, "FieldOffset {{offset: {} }}", offset)
            }
            FieldOffset::VariableOffset { .. } => write!(f, "VariableOffset"),
        }
    }
}

#[derive(Clone)]
enum FieldLayout {
    Value {
        value: FixedValueKind,
        optional: OptionalKind,
    },
    Vec {
        elem: FixedValueKind,
        flexible: Flexible,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OptionalKind {
    No,
    Tagged,
    Implicit(usize),
}

type SlotLen = (proc_macro2::TokenStream, usize);
type SlotMinMaxLen = Result<SlotLen, (SlotLen, SlotLen)>;

impl FieldLayout {
    fn is_fixed(&self) -> bool {
        match self {
            FieldLayout::Value { optional, .. } => *optional == OptionalKind::No,
            FieldLayout::Vec { .. } => false,
        }
    }

    fn slot_minmax_len(&self) -> SlotMinMaxLen {
        match self {
            Self::Value { value, optional } => {
                let value_size_expr = value.size_expr();
                match optional {
                    OptionalKind::No => Ok((value_size_expr, value.size())),
                    OptionalKind::Tagged => Err((
                        (quote!(1), 1),
                        (quote!((1 + #value_size_expr)), 1 + value.size()),
                    )),
                    OptionalKind::Implicit(_) => {
                        Err(((quote!(0), 0), (value_size_expr, value.size())))
                    }
                }
            }
            Self::Vec { elem, flexible } => {
                let elem_ty = elem.ty();
                let len_width_lit = usize_lit(flexible.len_width);
                let capacity_lit = usize_lit(flexible.capacity());
                Err((
                    (quote!(#len_width_lit), flexible.len_width),
                    (
                        quote!(#len_width_lit + core::mem::size_of::<#elem_ty>() * #capacity_lit),
                        flexible.len_width + elem.size() * flexible.capacity(),
                    ),
                ))
            }
        }
    }

    fn gen_encoded_len_step(&self, field_ident: &Ident) -> proc_macro2::TokenStream {
        match self {
            Self::Value { value, optional } => {
                let value_size = usize_lit(value.size());
                match optional {
                    OptionalKind::No => quote! {
                        len += #value_size;
                    },
                    OptionalKind::Tagged => quote! {
                        len += if self.#field_ident.is_some() { 1 + #value_size } else { 1 };
                    },
                    OptionalKind::Implicit(_) => quote! {
                        len += if self.#field_ident.is_some() { #value_size } else { 0 };
                    },
                }
            }
            Self::Vec { elem, flexible } => {
                let elem_size = usize_lit(elem.size());
                let len_width = usize_lit(flexible.len_width);
                let capacity = usize_lit(flexible.capacity());
                quote! {
                    let field_len = self.#field_ident.len();
                    if field_len > #capacity {
                        ::pinocchio_log::log!(
                            "Cannot encode field {}: len {} exceeds max {}",
                            stringify!(#field_ident),
                            field_len,
                            #capacity,
                        );
                        return Err(::wheels::DataLayoutError::LengthExceedsCapacity);
                    }
                    len += #len_width + field_len * #elem_size;
                }
            }
        }
    }

    fn gen_validate_step(
        &self,
        struct_name: &Ident,
        field_ident: &Ident,
    ) -> proc_macro2::TokenStream {
        let field_name = field_ident.to_string();

        match self {
            Self::Value { value, optional } => {
                let value_size = usize_lit(value.size());
                let value_align = usize_lit(value.align());
                let alignment_check = if matches!(value.access_mode(), AccessMode::Ref) {
                    quote! {
                        if data_offset % #value_align != 0 {
                            ::pinocchio_log::log!(
                                "Invalid alignment for field {}: payload starts at offset {}, expected {}-byte alignment",
                                #field_name,
                                data_offset,
                                #value_align,
                            );
                            return Err(::wheels::DataLayoutError::InvalidFieldAlignment);
                        }
                    }
                } else {
                    quote!()
                };

                match optional {
                    OptionalKind::No => quote! {
                        let data_offset = offset;
                        #alignment_check
                        let end = data_offset + #value_size;
                        if end > bytes.len() {
                            ::pinocchio_log::log!(
                                "Truncated payload for field {}: need {} bytes, have {}",
                                #field_name,
                                end,
                                bytes.len(),
                            );
                            return Err(::wheels::DataLayoutError::TruncatedPayload);
                        }
                        offset = end;
                    },
                    OptionalKind::Tagged => quote! {
                        if offset >= bytes.len() {
                            ::pinocchio_log::log!(
                                "Missing Option tag for field {} at offset {}",
                                #field_name,
                                offset,
                            );
                            return Err(::wheels::DataLayoutError::MissingOptionTag);
                        }

                        match bytes[offset] {
                            0 => {
                                offset += 1;
                            }
                            1 => {
                                let data_offset = offset + 1;
                                #alignment_check
                                let end = data_offset + #value_size;
                                if end > bytes.len() {
                                    ::pinocchio_log::log!(
                                        "Truncated payload for field {}: need {} bytes, have {}",
                                        #field_name,
                                        end,
                                        bytes.len(),
                                    );
                                    return Err(::wheels::DataLayoutError::TruncatedPayload);
                                }
                                offset = end;
                            }
                            tag => {
                                ::pinocchio_log::log!(
                                    "Invalid Option tag for field {}: tag = {}",
                                    #field_name,
                                    tag,
                                );
                                return Err(::wheels::DataLayoutError::InvalidOptionTag);
                            }
                        }
                    },
                    OptionalKind::Implicit(bit_index) => {
                        let bit_index = usize_lit(*bit_index);
                        quote! {
                            if #struct_name::__implicit_option_present_for_len(bytes.len(), #bit_index) {
                                let data_offset = offset;
                                #alignment_check
                                let end = data_offset + #value_size;
                                if end > bytes.len() {
                                    ::pinocchio_log::log!(
                                        "Truncated payload for field {}: need {} bytes, have {}",
                                        #field_name,
                                        end,
                                        bytes.len(),
                                    );
                                    return Err(::wheels::DataLayoutError::TruncatedPayload);
                                }
                                offset = end;
                            }
                        }
                    }
                }
            }
            Self::Vec { elem, flexible } => {
                let elem_size = usize_lit(elem.size());
                let elem_align = usize_lit(elem.align());
                let len_width_lit = usize_lit(flexible.len_width);
                let len_expr = checked_read_len_expr(
                    quote!(bytes),
                    quote!(offset),
                    flexible.len_width,
                    flexible.capacity(),
                    &field_name,
                );
                let alignment_check = if elem.align() > 1 {
                    quote! {
                        if len != 0 && data_offset % #elem_align != 0 {
                            ::pinocchio_log::log!(
                                "Invalid alignment for field {}: element data starts at offset {}, expected {}-byte alignment",
                                #field_name,
                                data_offset,
                                #elem_align,
                            );
                            return Err(::wheels::DataLayoutError::InvalidFieldAlignment);
                        }
                    }
                } else {
                    quote!()
                };

                quote! {
                    let data_offset = offset + #len_width_lit;
                    if data_offset > bytes.len() {
                        ::pinocchio_log::log!(
                            "Missing length header for field {} at offset {}",
                            #field_name,
                            offset,
                        );
                        return Err(::wheels::DataLayoutError::MissingLengthHeader);
                    }

                    let len = #len_expr;
                    #alignment_check

                    let end = data_offset + len * #elem_size;
                    if end > bytes.len() {
                        ::pinocchio_log::log!(
                            "Truncated Vec payload for field {}: need {} bytes, have {}",
                            #field_name,
                            end,
                            bytes.len(),
                        );
                        return Err(::wheels::DataLayoutError::TruncatedVectorPayload);
                    }

                    offset = end;
                }
            }
        }
    }

    fn gen_field_encode_step(&self, field_ident: &Ident) -> proc_macro2::TokenStream {
        match self {
            Self::Value { value, optional } => {
                let value_size = usize_lit(value.size());
                match optional {
                    OptionalKind::No => match value {
                        FixedValueKind::Bool { .. } => quote! {
                            bytes[offset] = u8::from(self.#field_ident);
                            offset += 1;
                        },
                        FixedValueKind::Pubkey { .. } => quote! {
                            bytes[offset..offset + #value_size]
                                .copy_from_slice(core::convert::AsRef::<[u8]>::as_ref(&self.#field_ident));
                            offset += #value_size;
                        },
                        _ => quote! {
                            bytes[offset..offset + #value_size]
                                .copy_from_slice(::bytemuck::bytes_of(&self.#field_ident));
                            offset += #value_size;
                        },
                    },
                    OptionalKind::Tagged => match value {
                        FixedValueKind::Bool { .. } => quote! {
                            if let core::option::Option::Some(value) = &self.#field_ident {
                                bytes[offset] = 1;
                                bytes[offset + 1] = u8::from(*value);
                                offset += 2;
                            } else {
                                bytes[offset] = 0;
                                offset += 1;
                            }
                        },
                        FixedValueKind::Pubkey { .. } => quote! {
                            if let core::option::Option::Some(value) = &self.#field_ident {
                                bytes[offset] = 1;
                                bytes[offset + 1..offset + 1 + #value_size]
                                    .copy_from_slice(core::convert::AsRef::<[u8]>::as_ref(value));
                                offset += 1 + #value_size;
                            } else {
                                bytes[offset] = 0;
                                offset += 1;
                            }
                        },
                        _ => quote! {
                            if let core::option::Option::Some(value) = &self.#field_ident {
                                bytes[offset] = 1;
                                bytes[offset + 1..offset + 1 + #value_size]
                                    .copy_from_slice(::bytemuck::bytes_of(value));
                                offset += 1 + #value_size;
                            } else {
                                bytes[offset] = 0;
                                offset += 1;
                            }
                        },
                    },
                    OptionalKind::Implicit(_) => match value {
                        FixedValueKind::Bool { .. } => quote! {
                            if let core::option::Option::Some(value) = &self.#field_ident {
                                bytes[offset] = u8::from(*value);
                                offset += 1;
                            }
                        },
                        FixedValueKind::Pubkey { .. } => quote! {
                            if let core::option::Option::Some(value) = &self.#field_ident {
                                bytes[offset..offset + #value_size]
                                    .copy_from_slice(core::convert::AsRef::<[u8]>::as_ref(value));
                                offset += #value_size;
                            }
                        },
                        _ => quote! {
                            if let core::option::Option::Some(value) = &self.#field_ident {
                                bytes[offset..offset + #value_size]
                                    .copy_from_slice(::bytemuck::bytes_of(value));
                                offset += #value_size;
                            }
                        },
                    },
                }
            }
            Self::Vec { elem, flexible } => {
                let elem_size = usize_lit(elem.size());
                let len_width = usize_lit(flexible.len_width);
                quote! {
                    let field_len = self.#field_ident.len();
                    let len_header = (field_len as u64).to_le_bytes();
                    bytes[offset..offset + #len_width]
                        .copy_from_slice(&len_header[..#len_width]);
                    let start = offset + #len_width;
                    let end = start + field_len * #elem_size;
                    if field_len != 0 {
                        bytes[start..end]
                            .copy_from_slice(::bytemuck::cast_slice(self.#field_ident.as_slice()));
                    }
                    offset = end;
                }
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
                } else {
                    None
                }
            }
        }
    }

    // fn check_ref_alignment(
    //     &self,
    //     offset: usize,
    //     field_ident: &Ident,
    // ) -> syn::Result<Option<PaddingIssue>> {
    //     match self {
    //         Self::Value { value, optional } => {
    //             if matches!(value.access_mode(), AccessMode::Copy) {
    //                 return Ok(None);
    //             }

    //             let align = value.align();
    //             if align > 8 {
    //                 return Err(syn::Error::new(
    //                     field_ident.span(),
    //                     format!(
    //                         "field `{}` cannot be borrowed by variable_offset_layout: size is {} byte(s) but alignment is {} byte(s), and fixed_layout only assumes the input buffer is 8-byte aligned",
    //                         field_ident,
    //                         value.size(),
    //                         align,
    //                     ),
    //                 ));
    //             }

    //             let payload_offset = offset + usize::from(optional.is_some());
    //             let misalignment = payload_offset % align;
    //             if misalignment == 0 {
    //                 return Ok(None);
    //             }

    //             let padding = align - misalignment;
    //             let message = if optional.is_some() {
    //                 format!(
    //                     "field `{}` needs {} byte(s) of padding before it: its Option payload would start at offset {}, but borrowed values of this field must be {}-byte aligned. Insert `_pad: [u8; {}]` before `{}` so the payload starts at offset {}",
    //                     field_ident,
    //                     padding,
    //                     payload_offset,
    //                     align,
    //                     padding,
    //                     field_ident,
    //                     payload_offset + padding,
    //                 )
    //             } else {
    //                 format!(
    //                     "field `{}` needs {} byte(s) of padding before it: it would start at offset {}, but borrowed values of this field must be {}-byte aligned. Insert `_pad: [u8; {}]` before `{}` so it starts at offset {}",
    //                     field_ident,
    //                     padding,
    //                     offset,
    //                     align,
    //                     padding,
    //                     field_ident,
    //                     offset + padding,
    //                 )
    //             };
    //             Ok(Some(PaddingIssue {
    //                 padding,
    //                 error: syn::Error::new(field_ident.span(), message),
    //             }))
    //         }
    //         Self::Vec { elem, capacity } => {
    //             let align = elem.align();
    //             if align > 8 {
    //                 return Err(syn::Error::new(
    //                     field_ident.span(),
    //                     format!(
    //                         "field `{}` cannot expose a slice view in variable_offset_layout: each Vec element is {} byte(s) but alignment is {} byte(s), and fixed_layout only assumes the input buffer is 8-byte aligned, so it cannot support type which requires alignment greater than 8",
    //                         field_ident,
    //                         elem.size(),
    //                         align,
    //                     ),
    //                 ));
    //             }

    //             let len_width = capacity.len_width();
    //             let first_elem_offset = offset + len_width;
    //             let misalignment = first_elem_offset % align;
    //             if misalignment == 0 {
    //                 return Ok(None);
    //             }

    //             let padding = align - misalignment;
    //             Ok(Some(PaddingIssue {
    //                 padding,
    //                 error: syn::Error::new(
    //                     field_ident.span(),
    //                     format!(
    //                     "field `{}` needs {} byte(s) of padding before it: its Vec elements start after a {}-byte length prefix, so element 0 would start at offset {}, but slice views require {}-byte alignment. Insert `_pad: [u8; {}]` before `{}` so element 0 starts at offset {}",
    //                     field_ident,
    //                     padding,
    //                     len_width,
    //                     first_elem_offset,
    //                     align,
    //                     padding,
    //                     field_ident,
    //                     first_elem_offset + padding,
    //                     ),
    //                 ),
    //             }))
    //         }
    //     }
    // }

    // fn gen_validate_vec_len(&self, offset: usize, field_ident: &Ident) -> proc_macro2::TokenStream {
    //     let field_name = field_ident.to_string();
    //     let offset_lit = usize_lit(offset);
    //     let offset_expr = quote!(#offset_lit);
    //     match self {
    //         Self::Value { optional, .. } => {
    //             if optional.is_some() {
    //                 quote! {
    //                     Self::__validate_option(bytes, #offset_expr, #field_name)?;
    //                 }
    //             } else {
    //                 quote! {}
    //             }
    //         }
    //         Self::Vec { capacity, .. } => {
    //             let capacity_lit = capacity
    //                 .comptime_capacity_lit()
    //                 .unwrap_or(usize_lit(MAX_CAPACITY));
    //             let len_width_lit = capacity.len_width_lit();
    //             let expect_msg = format!(
    //                 "validate encoded-len [len_width={}] for field '{}'",
    //                 capacity.len_width(),
    //                 field_name
    //             );
    //             quote! {
    //                 Self::__validate_vec_len(bytes, #offset_expr, #capacity_lit, #len_width_lit, #field_name, #expect_msg)?;
    //             }
    //         }
    //     }
    // }

    // fn gen_field_encode(
    //     &self,
    //     fields_encode_expr: proc_macro2::TokenStream,
    //     offset: usize,
    //     field_ident: &Ident,
    // ) -> proc_macro2::TokenStream {
    //     let offset = usize_lit(offset);
    //     match self {
    //         Self::Value { value, optional } => {
    //             let len = usize_lit(value.size());
    //             match optional {
    //                 Some(Optional::Fixed) => {
    //                     quote! {
    //                         #fields_encode_expr

    //                         if let Some(value) = &self.#field_ident {
    //                             bytes[#offset] = 1;
    //                             bytes[#offset + 1 .. #offset + 1 + #len].copy_from_slice(bytemuck::bytes_of(value));
    //                         } else {
    //                             bytes[#offset] = 0;
    //                             bytes[#offset + 1 .. #offset + 1 + #len].fill(0);
    //                         }
    //                     }
    //                 }
    //                 Some(Optional::Flexible) => {
    //                     quote! {
    //                         #fields_encode_expr

    //                         if let Some(value) = &self.#field_ident {
    //                             bytes[#offset] = 1;
    //                             bytes[#offset + 1 .. #offset + 1 + #len].copy_from_slice(bytemuck::bytes_of(value));
    //                         }
    //                     }
    //                 }
    //                 None => {
    //                     quote! {
    //                         #fields_encode_expr

    //                         bytes[#offset..#offset + #len].copy_from_slice(bytemuck::bytes_of(&self.#field_ident));
    //                     }
    //                 }
    //             }
    //         }
    //         Self::Vec { elem, capacity } => {
    //             let elem_size = usize_lit(elem.size());
    //             let len_width_ty = capacity.len_width_ty();
    //             let len_width = capacity.len_width_lit();
    //             if let Some(cap) = capacity.comptime_capacity_lit() {
    //                 quote! {
    //                     #fields_encode_expr

    //                     if self.#field_ident.len() > #cap {
    //                         return Err(pinocchio::error::ProgramError::InvalidRealloc);
    //                     }

    //                     bytes[#offset..#offset + #len_width].copy_from_slice(bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
    //                     bytes[#offset + #len_width..#offset + #len_width + self.#field_ident.len() * #elem_size].copy_from_slice(bytemuck::cast_slice(&self.#field_ident.as_slice()));
    //                     if self.#field_ident.len() < #cap {
    //                          bytes[#offset + #len_width + self.#field_ident.len() * #elem_size..#offset + #len_width + #cap * #elem_size].fill(0);
    //                     }
    //                 }
    //             } else {
    //                 // it must be the last field of Vec type with #[capacity = flexible]
    //                 let max_capacity = usize_lit(MAX_CAPACITY);
    //                 quote! {
    //                     #fields_encode_expr

    //                     if self.#field_ident.len() > #max_capacity {
    //                         return Err(pinocchio::error::ProgramError::InvalidRealloc);
    //                     } else if !self.#field_ident.is_empty() {
    //                         bytes[#offset..#offset + #len_width].copy_from_slice(bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
    //                         bytes[#offset + #len_width..#offset + #len_width + self.#field_ident.len() * #elem_size].copy_from_slice(bytemuck::cast_slice(&self.#field_ident.as_slice()));
    //                     } else {
    //                         // Note that it is an empty-vector scenario in which case we do not have any 'buffer' to write anything (even zeroes) to
    //                     }
    //                 }
    //             }
    //         }
    //     }
    // }

    fn gen_view_methods(
        &self,
        struct_name: &Ident,
        field_ident: &Ident,
        field_offsets: &[FieldOffset],
    ) -> syn::Result<proc_macro2::TokenStream> {
        match self {
            Self::Value { value, optional } => {
                let ty = value.ty();
                let value_size = usize_lit(value.size());
                let access_mode = value.access_mode();
                let offset_expr = find_data_offset(struct_name, field_offsets);
                let data_offset_expr = match optional {
                    OptionalKind::No | OptionalKind::Implicit(_) => quote!(offset),
                    OptionalKind::Tagged => quote!(offset + 1),
                };
                let slice_expr =
                    quote!(&self.bytes[#data_offset_expr..#data_offset_expr+#value_size]);

                let return_expr = match access_mode {
                    AccessMode::Copy => {
                        read_copy_expr(value, slice_expr, LAYOUT_NAME, INTEGER_FIELD_MESSAGE)?
                    }
                    AccessMode::Ref => quote!(::bytemuck::from_bytes::<#ty>(#slice_expr)),
                };

                match (*optional, access_mode) {
                    (OptionalKind::No, AccessMode::Copy) => Ok(quote! {
                        pub fn #field_ident(&self) -> #ty {
                            let offset = #offset_expr;
                            #return_expr
                        }
                    }),
                    (OptionalKind::No, AccessMode::Ref) => Ok(quote! {
                        pub fn #field_ident(&self) -> &#ty {
                            let offset = #offset_expr;
                            #return_expr
                        }
                    }),
                    (OptionalKind::Tagged, AccessMode::Copy) => Ok(quote! {
                        pub fn #field_ident(&self) -> core::option::Option<#ty> {
                            let offset = #offset_expr;
                            (self.bytes[offset] != 0).then(|| #return_expr)
                        }
                    }),
                    (OptionalKind::Tagged, AccessMode::Ref) => Ok(quote! {
                        pub fn #field_ident(&self) -> core::option::Option<&#ty> {
                            let offset = #offset_expr;
                            (self.bytes[offset] != 0).then(|| #return_expr)
                        }
                    }),
                    (OptionalKind::Implicit(bit_index), AccessMode::Copy) => {
                        let bit_index = usize_lit(bit_index);
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<#ty> {
                                let offset = #offset_expr;
                                #struct_name::__implicit_option_present_for_len(self.bytes.len(), #bit_index)
                                    .then(|| #return_expr)
                            }
                        })
                    }
                    (OptionalKind::Implicit(bit_index), AccessMode::Ref) => {
                        let bit_index = usize_lit(bit_index);
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<&#ty> {
                                let offset = #offset_expr;
                                #struct_name::__implicit_option_present_for_len(self.bytes.len(), #bit_index)
                                    .then(|| #return_expr)
                            }
                        })
                    }
                }
            }
            Self::Vec { elem, flexible } => {
                let elem_ty = elem.ty();
                let elem_size = usize_lit(elem.size());
                let len_width_lit = usize_lit(flexible.len_width);
                let len_expr = validated_len_expr(
                    struct_name,
                    quote!(self.bytes),
                    quote!(offset),
                    flexible.len_width,
                );

                let offset_expr = find_data_offset(struct_name, field_offsets);
                Ok(quote! {
                    pub fn #field_ident(&self) -> &[#elem_ty] {
                        let offset = #offset_expr;
                        let len = #len_expr;
                        if len == 0 {
                            return &[];
                        }
                        let start = offset + #len_width_lit;
                        let end = start + len * #elem_size;
                        ::bytemuck::cast_slice::<u8, #elem_ty>(&self.bytes[start..end])
                    }
                })
            }
        }
    }
}

fn parse_field_layout(
    field: &syn::Field,
    option_encoding: StructOptionEncoding,
    implicit_option_index: &mut usize,
) -> syn::Result<FieldLayout> {
    let ty = &field.ty;
    let attribute = parse_field_attr(field)?;

    if let Some(elem_ty) = vec_inner(ty, LAYOUT_NAME)? {
        if is_bool(elem_ty) {
            return Err(syn::Error::new_spanned(
                field,
                "Vec<bool> is not supported by variable_offset_layout",
            ));
        }
        let attribute = attribute.ok_or_else(|| {
            syn::Error::new_spanned(
                field,
                "Vec fields in variable_offset_layout require `#[flexible = N]`",
            )
        })?;
        let flexible = match attribute {
            FieldAttribute::Flexible(len_width) => Flexible { len_width },
        };
        return Ok(FieldLayout::Vec {
            elem: parse_value_kind(elem_ty, UNSUPPORTED_FIELD_MESSAGE)?,
            flexible,
        });
    }

    if attribute.is_some() {
        return Err(syn::Error::new_spanned(
            field,
            "`#[flexible = N]` is only applicable on Vec fields",
        ));
    }

    if let Some(inner) = option_inner(ty) {
        if vec_inner(inner, LAYOUT_NAME)?.is_some() {
            return Err(syn::Error::new_spanned(
                field,
                "Option<Vec<T>> is not supported in variable_offset_layout",
            ));
        }
        if is_string(inner) {
            return Err(syn::Error::new_spanned(
                field,
                "String is not supported by variable_offset_layout",
            ));
        }

        return Ok(FieldLayout::Value {
            value: parse_value_kind(inner, UNSUPPORTED_FIELD_MESSAGE)?,
            optional: match option_encoding {
                StructOptionEncoding::Tagged => OptionalKind::Tagged,
                StructOptionEncoding::Implicit => {
                    let index = *implicit_option_index;
                    *implicit_option_index += 1;
                    OptionalKind::Implicit(index)
                }
            },
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
            "String is not supported by variable_offset_layout",
        ));
    }

    Ok(FieldLayout::Value {
        value: parse_value_kind(ty, UNSUPPORTED_FIELD_MESSAGE)?,
        optional: OptionalKind::No,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutArgs {
    option_encoding: StructOptionEncoding,
    buffer_offset: BufferOffset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferOffset {
    Fixed(usize),
    Unaligned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StructOptionEncoding {
    Tagged,
    Implicit,
}

fn parse_args(attr: &str) -> syn::Result<LayoutArgs> {
    if attr.trim().is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "variable_offset_layout requires `buffer_offset = 0..=7` or `buffer_offset = unaligned`",
        ));
    }

    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse_str(attr)?;
    let mut option_encoding = StructOptionEncoding::Tagged;
    let mut buffer_offset = None;

    for meta in metas {
        match meta {
            Meta::NameValue(meta) if meta.path.is_ident("option") => {
                let Expr::Path(value) = meta.value else {
                    return Err(syn::Error::new_spanned(
                        meta,
                        "option must use the form `option = implicit`",
                    ));
                };

                let Some(ident) = value.path.get_ident() else {
                    return Err(syn::Error::new_spanned(
                        value,
                        "option must use the form `option = implicit`",
                    ));
                };

                match ident.to_string().as_str() {
                    "implicit" => option_encoding = StructOptionEncoding::Implicit,
                    _ => return Err(syn::Error::new_spanned(ident, "option must be `implicit`")),
                }
            }
            Meta::NameValue(meta) if meta.path.is_ident("buffer_offset") => {
                match meta.value {
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
                                "buffer_offset must be an integer in 0..=7 or `unaligned`",
                            ));
                        };

                        if ident == "unaligned" {
                            buffer_offset = Some(BufferOffset::Unaligned);
                        } else {
                            return Err(syn::Error::new_spanned(
                                ident,
                                "buffer_offset must be an integer in 0..=7 or `unaligned`",
                            ));
                        }
                    }
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "buffer_offset must be an integer in 0..=7 or `unaligned`",
                        ));
                    }
                }
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    meta,
                    "variable_offset_layout only supports `option = implicit`, `buffer_offset = 0..=7`, and `buffer_offset = unaligned`",
                ))
            }
        }
    }

    let Some(buffer_offset) = buffer_offset else {
        return Err(syn::Error::new(
            Span::call_site(),
            "variable_offset_layout requires `buffer_offset = 0..=7` or `buffer_offset = unaligned`",
        ));
    };

    Ok(LayoutArgs {
        option_encoding,
        buffer_offset,
    })
}

fn validate_struct_options(
    struct_name: &Ident,
    fields: &syn::punctuated::Punctuated<syn::Field, Token![,]>,
    layouts: &[FieldLayout],
    args: &LayoutArgs,
) -> syn::Result<()> {
    if args.option_encoding != StructOptionEncoding::Implicit {
        return Ok(());
    }

    if let Some(field) = fields
        .iter()
        .zip(layouts.iter())
        .find_map(|(field, layout)| matches!(layout, FieldLayout::Vec { .. }).then_some(field))
    {
        return Err(syn::Error::new_spanned(
            field,
            "variable_offset_layout(option = implicit) does not support Vec fields",
        ));
    }

    let implicit_option_count = layouts
        .iter()
        .filter(|layout| {
            matches!(
                layout,
                FieldLayout::Value {
                    optional: OptionalKind::Implicit(_),
                    ..
                }
            )
        })
        .count();

    if implicit_option_count == 0 {
        return Err(syn::Error::new_spanned(
            struct_name,
            "variable_offset_layout(option = implicit) requires at least one Option<T> field",
        ));
    }

    let _ = implicit_len_map(fields, layouts, 0)?;

    Ok(())
}

fn implicit_len_map(
    fields: &syn::punctuated::Punctuated<syn::Field, Token![,]>,
    layouts: &[FieldLayout],
    base_len: usize,
) -> syn::Result<Vec<(usize, u128)>> {
    let implicit_payloads = fields
        .iter()
        .zip(layouts.iter())
        .filter_map(|(field, layout)| match layout {
            FieldLayout::Value {
                value,
                optional: OptionalKind::Implicit(bit_index),
            } => Some((field, *bit_index, value.size())),
            _ => None,
        })
        .collect::<Vec<_>>();

    if implicit_payloads.is_empty() {
        return Ok(Vec::new());
    }

    if implicit_payloads.len() > 128 {
        return Err(syn::Error::new_spanned(
            fields,
            "variable_offset_layout(option = implicit) supports at most 128 Option<T> fields",
        ));
    }

    let mut sums = BTreeMap::from([(0usize, 0u128)]);

    for (field, bit_index, payload_size) in implicit_payloads {
        let existing = sums.clone();
        for (sum, mask) in existing {
            let next_sum = sum + payload_size;
            let next_mask = mask | (1u128 << bit_index);
            if sums.insert(next_sum, next_mask).is_some() {
                let field_ident = field.ident.as_ref().expect("named field");
                return Err(syn::Error::new(
                    field_ident.span(),
                    format!(
                        "variable_offset_layout(option = implicit) requires Option<T> payload sizes to have unique subset sums: field `{}` creates duplicate extra payload length {}",
                        field_ident,
                        next_sum,
                    ),
                ));
            }
        }
    }

    Ok(sums
        .into_iter()
        .map(|(extra_len, mask)| (base_len + extra_len, mask))
        .collect())
}

fn implicit_len_helpers(
    min_datalen: usize,
    fields: &syn::punctuated::Punctuated<syn::Field, Token![,]>,
    layouts: &[FieldLayout],
) -> syn::Result<proc_macro2::TokenStream> {
    let len_map = implicit_len_map(fields, layouts, min_datalen)?;
    if len_map.is_empty() {
        return Ok(quote!());
    }

    let len_arms = len_map.iter().map(|(len, mask)| {
        let len = usize_lit(*len);
        let mask = syn::LitInt::new(&format!("{}u128", mask), Span::call_site());
        quote!(#len => core::option::Option::Some(#mask),)
    });

    Ok(quote! {
        fn __implicit_option_mask_for_len(len: usize) -> core::option::Option<u128> {
            match len {
                #(#len_arms)*
                _ => core::option::Option::None,
            }
        }

        fn __implicit_option_present_for_len(len: usize, bit_index: usize) -> bool {
            let mask = Self::__implicit_option_mask_for_len(len)
                .expect("validated implicit option length");
            (mask & (1u128 << bit_index)) != 0
        }
    })
}

fn implicit_len_validation(
    struct_name: &Ident,
    min_datalen: usize,
    fields: &syn::punctuated::Punctuated<syn::Field, Token![,]>,
    layouts: &[FieldLayout],
) -> syn::Result<proc_macro2::TokenStream> {
    let len_map = implicit_len_map(fields, layouts, min_datalen)?;
    if len_map.is_empty() {
        return Ok(quote!());
    }

    let valid_lens = format!(
        "{:?}",
        len_map.iter().map(|(len, _)| *len).collect::<Vec<_>>()
    );

    Ok(quote! {
        if #struct_name::__implicit_option_mask_for_len(bytes.len()).is_none() {
            ::pinocchio_log::log!(
                "Invalid implicit Option encoding for {}: len {} is not one of {}",
                stringify!(#struct_name),
                bytes.len(),
                #valid_lens,
            );
            return Err(::wheels::DataLayoutError::InvalidImplicitOptionEncoding);
        }
    })
}

fn exact_data_lens(layouts: &[FieldLayout]) -> Option<Vec<usize>> {
    let mut lengths = BTreeSet::from([0usize]);
    for layout in layouts {
        let slot_lengths: BTreeSet<usize> = match layout {
            FieldLayout::Value { value, optional } => match optional {
                OptionalKind::No => BTreeSet::from([value.size()]),
                OptionalKind::Tagged => BTreeSet::from([1, 1 + value.size()]),
                OptionalKind::Implicit(_) => BTreeSet::from([0, value.size()]),
            },
            FieldLayout::Vec { .. } => return None,
        };

        let mut next = BTreeSet::new();
        for total in &lengths {
            for slot_len in &slot_lengths {
                next.insert(total + slot_len);
            }
        }
        lengths = next;
    }

    Some(lengths.into_iter().collect())
}

fn public_len_const(
    min_datalen_expr: proc_macro2::TokenStream,
    min_datalen: usize,
    max_datalen_expr: proc_macro2::TokenStream,
    max_datalen: usize,
    layouts: &[FieldLayout],
) -> syn::Result<proc_macro2::TokenStream> {
    if let Some(exact_lens) = exact_data_lens(layouts) {
        if exact_lens.len() == 1 {
            let data_len = exact_lens[0];
            let doc = format!("Exact size of encoded data = {}", data_len);
            return Ok(quote! {
                #[doc = #doc]
                pub const DATA_LEN: usize = #min_datalen_expr;
            });
        }

        let doc = format!("Exact valid encoded sizes = {:?}", exact_lens);
        let len_count = usize_lit(exact_lens.len());
        let len_lits = exact_lens.iter().map(|len| usize_lit(*len));
        return Ok(quote! {
            #[doc = #doc]
            pub const DATA_LENS: [usize; #len_count] = [#(#len_lits),*];
        });
    }

    let doc = format!(
        "Valid encoded size range = ({}, {})",
        min_datalen, max_datalen
    );
    Ok(quote! {
        #[doc = #doc]
        pub const DATA_LEN_RANGE: (usize, usize) = (#min_datalen_expr, #max_datalen_expr);
    })
}

fn data_len_validation(
    struct_name: &Ident,
    min_datalen: usize,
    max_datalen: usize,
    layouts: &[FieldLayout],
) -> syn::Result<proc_macro2::TokenStream> {
    if let Some(exact_lens) = exact_data_lens(layouts) {
        if exact_lens.len() == 1 {
            let msg = format!(
                "bytes [len={{}}] cannot be deserialized to {} which needs exactly {} bytes",
                struct_name, exact_lens[0]
            );
            return Ok(quote! {
                if bytes.len() != Self::DATA_LEN {
                    ::pinocchio_log::log!(#msg, bytes.len());
                    return Err(::wheels::DataLayoutError::InvalidDataLength);
                }
            });
        }

        let valid_lens = format!("{:?}", exact_lens);
        let len_patterns = exact_lens.iter().map(|len| usize_lit(*len));
        return Ok(quote! {
            if !matches!(bytes.len(), #(#len_patterns)|*) {
                ::pinocchio_log::log!(
                    "bytes [len={}] cannot be deserialized to {} which needs one of {} bytes",
                    bytes.len(),
                    stringify!(#struct_name),
                    #valid_lens,
                );
                return Err(::wheels::DataLayoutError::InvalidDataLength);
            }
        });
    }

    let msg = format!(
        "bytes [len={{}}] cannot be deserialized to {} which needs at least {} and at most {} bytes",
        struct_name, min_datalen, max_datalen
    );
    Ok(quote! {
        if bytes.len() < Self::__MIN_DATA_LEN || bytes.len() > Self::__MAX_DATA_LEN {
            ::pinocchio_log::log!(
                #msg,
                bytes.len(),
            );
            return Err(::wheels::DataLayoutError::InvalidDataLength);
        }
    })
}

fn validate_borrowed_field_alignment(
    _struct_name: &Ident,
    fields: &syn::punctuated::Punctuated<syn::Field, Token![,]>,
    layouts: &[FieldLayout],
    args: &LayoutArgs,
) -> syn::Result<()> {
    for (index, (field, layout)) in fields.iter().zip(layouts.iter()).enumerate() {
        let Some(requirement) = borrowed_requirement(layout) else {
            continue;
        };

        let field_ident = field.ident.as_ref().expect("named field");
        let align = requirement.align();
        let BufferOffset::Fixed(buffer_offset) = args.buffer_offset else {
            if align > 1 {
                return Err(syn::Error::new(
                    field_ident.span(),
                    requirement.unaligned_offset_message(field_ident),
                ));
            }
            continue;
        };

        if align > 8 {
            return Err(syn::Error::new(
                field_ident.span(),
                requirement.insufficient_base_alignment_message(field_ident, buffer_offset, align),
            ));
        }

        let start_residues = possible_start_residues(&layouts[..index], align);
        let aligned_residues = shift_residues(
            &start_residues,
            buffer_offset + requirement.payload_shift(),
            align,
        );
        if aligned_residues.len() == 1 && aligned_residues.contains(&0) {
            continue;
        }

        if let Some(start_offset) = exact_start_offset(&layouts[..index]) {
            return Err(syn::Error::new(
                field_ident.span(),
                requirement.fixed_offset_alignment_message(
                    field_ident,
                    buffer_offset,
                    start_offset,
                ),
            ));
        }

        if aligned_residues.len() == 1 {
            let residue = *aligned_residues.first().expect("single residue");
            return Err(syn::Error::new(
                field_ident.span(),
                requirement.stable_but_misaligned_message(field_ident, buffer_offset, residue),
            ));
        }

        return Err(syn::Error::new(
            field_ident.span(),
            requirement.variable_offset_alignment_message(field_ident, buffer_offset),
        ));
    }

    Ok(())
}

#[derive(Clone, Copy)]
enum BorrowedRequirement {
    Value {
        align: usize,
        optional_kind: OptionalKind,
    },
    Vec {
        align: usize,
        len_width: usize,
    },
}

impl BorrowedRequirement {
    fn align(self) -> usize {
        match self {
            Self::Value { align, .. } | Self::Vec { align, .. } => align,
        }
    }

    fn payload_shift(self) -> usize {
        match self {
            Self::Value {
                optional_kind: OptionalKind::Tagged,
                ..
            } => 1,
            Self::Value { .. } => 0,
            Self::Vec { len_width, .. } => len_width,
        }
    }

    fn insufficient_base_alignment_message(
        self,
        field_ident: &Ident,
        buffer_offset: usize,
        align: usize,
    ) -> String {
        match self {
            Self::Value { .. } => format!(
                "field `{}` cannot be borrowed with `buffer_offset = {}`: it requires {}-byte alignment, but variable_offset_layout only assumes the original input buffer is 8-byte aligned",
                field_ident, buffer_offset, align
            ),
            Self::Vec { .. } => format!(
                "field `{}` cannot expose a slice view with `buffer_offset = {}`: its elements require {}-byte alignment, but variable_offset_layout only assumes the original input buffer is 8-byte aligned",
                field_ident, buffer_offset, align
            ),
        }
    }

    fn unaligned_offset_message(self, field_ident: &Ident) -> String {
        let align = self.align();
        match self {
            Self::Value { .. } => format!(
                "field `{}` cannot be borrowed with `buffer_offset = unaligned`: it requires {}-byte alignment, but the input slice may start at any address",
                field_ident, align
            ),
            Self::Vec { .. } => format!(
                "field `{}` cannot expose a slice view with `buffer_offset = unaligned`: its elements require {}-byte alignment, but the input slice may start at any address",
                field_ident, align
            ),
        }
    }

    fn fixed_offset_alignment_message(
        self,
        field_ident: &Ident,
        buffer_offset: usize,
        start_offset: usize,
    ) -> String {
        let payload_offset = start_offset + self.payload_shift();
        let align = self.align();
        match self {
            Self::Value {
                optional_kind: OptionalKind::Tagged,
                ..
            } => format!(
                "field `{}` cannot be borrowed with `buffer_offset = {}`: its Option payload would start at offset {}, so its actual address would be {} mod {}, but borrowed values of this field must be {}-byte aligned",
                field_ident,
                buffer_offset,
                payload_offset,
                (buffer_offset + payload_offset) % align,
                align,
                align
            ),
            Self::Value { .. } => format!(
                "field `{}` cannot be borrowed with `buffer_offset = {}`: it would start at offset {}, so its actual address would be {} mod {}, but borrowed values of this field must be {}-byte aligned",
                field_ident,
                buffer_offset,
                payload_offset,
                (buffer_offset + payload_offset) % align,
                align,
                align
            ),
            Self::Vec { len_width, .. } => format!(
                "field `{}` cannot expose a slice view with `buffer_offset = {}`: its Vec elements start after a {}-byte length prefix, so element 0 would start at offset {}, and its actual address would be {} mod {}, but slice views require {}-byte alignment",
                field_ident,
                buffer_offset,
                len_width,
                payload_offset,
                (buffer_offset + payload_offset) % align,
                align,
                align
            ),
        }
    }

    fn stable_but_misaligned_message(
        self,
        field_ident: &Ident,
        buffer_offset: usize,
        residue: usize,
    ) -> String {
        let align = self.align();
        match self {
            Self::Value { .. } => format!(
                "field `{}` cannot be borrowed with `buffer_offset = {}`: its actual address would always be congruent to {} mod {}, so {}-byte alignment cannot be guaranteed",
                field_ident, buffer_offset, residue, align, align
            ),
            Self::Vec { .. } => format!(
                "field `{}` cannot expose a slice view with `buffer_offset = {}`: element 0 would always be congruent to {} mod {}, so {}-byte alignment cannot be guaranteed",
                field_ident, buffer_offset, residue, align, align
            ),
        }
    }

    fn variable_offset_alignment_message(
        self,
        field_ident: &Ident,
        buffer_offset: usize,
    ) -> String {
        let align = self.align();
        match self {
            Self::Value { .. } => format!(
                "field `{}` cannot be borrowed with `buffer_offset = {}`: earlier variable-sized fields make its actual address vary, so {}-byte alignment cannot be guaranteed",
                field_ident, buffer_offset, align
            ),
            Self::Vec { .. } => format!(
                "field `{}` cannot expose a slice view with `buffer_offset = {}`: earlier variable-sized fields make element 0's actual address vary, so {}-byte alignment cannot be guaranteed",
                field_ident, buffer_offset, align
            ),
        }
    }
}

fn borrowed_requirement(layout: &FieldLayout) -> Option<BorrowedRequirement> {
    match layout {
        FieldLayout::Value { value, optional } => match value.access_mode() {
            AccessMode::Copy => None,
            AccessMode::Ref => Some(BorrowedRequirement::Value {
                align: value.align(),
                optional_kind: *optional,
            }),
        },
        FieldLayout::Vec { elem, flexible } => {
            (elem.align() > 1).then_some(BorrowedRequirement::Vec {
                align: elem.align(),
                len_width: flexible.len_width,
            })
        }
    }
}

fn exact_start_offset(layouts: &[FieldLayout]) -> Option<usize> {
    let mut offset = 0usize;
    for layout in layouts {
        match layout.slot_minmax_len() {
            Ok((_, len)) => offset += len,
            Err(_) => return None,
        }
    }
    Some(offset)
}

fn possible_start_residues(layouts: &[FieldLayout], modulus: usize) -> BTreeSet<usize> {
    let mut residues = BTreeSet::from([0usize]);
    for layout in layouts {
        let len_residues = possible_len_residues(layout, modulus);
        let mut next = BTreeSet::new();
        for start in &residues {
            for len in &len_residues {
                next.insert((start + len) % modulus);
            }
        }
        residues = next;
    }
    residues
}

fn shift_residues(residues: &BTreeSet<usize>, shift: usize, modulus: usize) -> BTreeSet<usize> {
    residues
        .iter()
        .map(|residue| (residue + shift) % modulus)
        .collect()
}

fn possible_len_residues(layout: &FieldLayout, modulus: usize) -> BTreeSet<usize> {
    if modulus == 1 {
        return BTreeSet::from([0]);
    }

    match layout {
        FieldLayout::Value { value, optional } => match optional {
            OptionalKind::No => BTreeSet::from([value.size() % modulus]),
            OptionalKind::Tagged => BTreeSet::from([1 % modulus, (1 + value.size()) % modulus]),
            OptionalKind::Implicit(_) => BTreeSet::from([0, value.size() % modulus]),
        },
        FieldLayout::Vec { elem, flexible } => {
            let capacity = flexible.capacity();
            let period = modulus / gcd(modulus, elem.size());
            let max_k = capacity.min(period.saturating_sub(1));
            let mut residues = BTreeSet::new();
            for k in 0..=max_k {
                residues.insert((flexible.len_width + k * elem.size()) % modulus);
            }
            residues
        }
    }
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

#[derive(Clone, Copy, Debug)]
struct Flexible {
    len_width: usize,
}

impl Flexible {
    fn capacity(self) -> usize {
        match self.len_width {
            8 => u32::MAX as usize,
            _ => (1usize << (self.len_width * 8)) - 1,
        }
    }
}

enum FieldAttribute {
    ///
    /// forms:
    ///
    ///   #[flexible = 1..=8]
    ///     applicable on Vec only
    ///     Some(usize) represents it's a Vec and its length is encoded as len_width  bytes
    ///
    Flexible(usize),
}

fn parse_field_attr(field: &syn::Field) -> syn::Result<Option<FieldAttribute>> {
    let mut attributes = vec![];
    for attr in &field.attrs {
        if attr.path().is_ident("flexible") {
            match &attr.meta {
                syn::Meta::NameValue(meta) => {
                    let Expr::Lit(ExprLit {
                        lit: Lit::Int(lit_int),
                        ..
                    }) = &meta.value
                    else {
                        return Err(syn::Error::new_spanned(
                            attr,
                            "flexible must use the form `#[flexible = 1..=8]` on a Vec field",
                        ));
                    };

                    let len_width: usize = lit_int.base10_parse()?;

                    if len_width == 0 || len_width > MAX_LEN_WIDTH {
                        return Err(syn::Error::new(
                            field.span(),
                            "flexible must be in the range 1..=8",
                        ));
                    }
                    attributes.push(FieldAttribute::Flexible(len_width));
                }
                _meta => {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "flexible must use the form `#[flexible = 1..=8]` on a Vec field",
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

fn checked_read_len_expr(
    bytes_expr: proc_macro2::TokenStream,
    offset_expr: proc_macro2::TokenStream,
    len_width: usize,
    capacity: usize,
    field_name: &str,
) -> proc_macro2::TokenStream {
    let len_width_lit = usize_lit(len_width);
    let capacity_lit = usize_lit(capacity);
    quote! {
        Self::__read_len_header(#bytes_expr, #offset_expr, #len_width_lit, #capacity_lit, #field_name)?
    }
}

fn validated_len_expr(
    struct_name: &Ident,
    bytes_expr: proc_macro2::TokenStream,
    offset_expr: proc_macro2::TokenStream,
    len_width: usize,
) -> proc_macro2::TokenStream {
    let len_width_lit = usize_lit(len_width);
    quote! {
        #struct_name::__read_len_header_unchecked(#bytes_expr, #offset_expr, #len_width_lit)
    }
}

fn find_data_offset(
    struct_name: &Ident,
    field_offsets: &[FieldOffset],
) -> proc_macro2::TokenStream {
    match field_offsets.last().unwrap() {
        FieldOffset::FixedOffset { offset, .. } => {
            let offset = usize_lit(*offset);
            quote!(#offset)
        }
        FieldOffset::VariableOffset { .. } => {
            let (index, offset, layout) = field_offsets
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, field_offset)| {
                    field_offset
                        .fixed_offset()
                        .map(|offset| (index, offset, field_offset.layout()))
                })
                .unwrap();

            assert!(!layout.is_fixed());

            // | F | F | V | V | V | V |
            //
            let current_offset_expr = field_offsets[index..field_offsets.len() - 1]
                .iter()
                .fold(quote!(#offset), |expr, field_offset| {
                    match field_offset {
                    FieldOffset::FixedOffset { layout, .. } => {
                        // unreachable!("getter_body: FixedOffset is impossible as this point only VariableOffset is expected"),
                        // acutally only the first entry has FixedOffset with layout.is_fixed() == false
                        match layout {
                            FieldLayout::Value { value, optional } => {
                                let fixed_lit = value.size();
                                match optional {
                                    OptionalKind::No => quote! {#expr + #fixed_lit},
                                    OptionalKind::Tagged => quote! {
                                        #expr + if self.bytes[#expr] == 0 { 1 } else { 1 + #fixed_lit }
                                    },
                                    OptionalKind::Implicit(bit_index) => {
                                        let bit_index = usize_lit(*bit_index);
                                        quote! {
                                        #expr + if #struct_name::__implicit_option_present_for_len(self.bytes.len(), #bit_index) { #fixed_lit } else { 0 }
                                    }
                                    },
                                }
                        }
                        FieldLayout::Vec { elem, flexible } => {
                            let elem_size = usize_lit(elem.size());
                            let len_width = usize_lit(flexible.len_width);
                            let len_expr = validated_len_expr(
                                struct_name,
                                quote!(self.bytes),
                                quote!(#expr),
                                flexible.len_width,
                            );
                            quote! {
                                #expr + (#len_width + (#len_expr) * #elem_size)
                            }
                            },
                        }
                    },
                    FieldOffset::VariableOffset { layout } => match layout {
                        FieldLayout::Value { value, optional } => {
                            let fixed_lit = value.size();
                            match optional {
                                OptionalKind::No => quote! {#expr + #fixed_lit},
                                OptionalKind::Tagged => quote! {
                                    #expr + if self.bytes[#expr] == 0 { 1 } else { 1 + #fixed_lit }
                                },
                                OptionalKind::Implicit(bit_index) => {
                                    let bit_index = usize_lit(*bit_index);
                                    quote! {
                                    #expr + if #struct_name::__implicit_option_present_for_len(self.bytes.len(), #bit_index) { #fixed_lit } else { 0 }
                                }
                                },
                            }
                        }
                        FieldLayout::Vec { elem, flexible } => {
                            let elem_size = usize_lit(elem.size());
                            let len_width = usize_lit(flexible.len_width);
                            let len_expr = validated_len_expr(
                                struct_name,
                                quote!(self.bytes),
                                quote!(#expr),
                                flexible.len_width,
                            );
                            quote! {
                                #expr + (#len_width + (#len_expr) * #elem_size)
                            }
                        },
                    },
                }
                });

            quote!(#current_offset_expr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::expand_variable_offset_layout;
    use syn::parse_quote;
    //
    //     #[test]
    //     fn variable_offset_layout_reports_padding_for_large_field() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 flag: u8,
    //                 payload: [u64; 2],
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(error.contains("field `payload` needs 7 byte(s) of padding before it"));
    //         assert!(error.contains("starts at offset 8"));
    //     }
    //
    //     #[test]
    //     fn variable_offset_layout_reports_padding_for_optional_payload() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 flag: u8,
    //                 payload: Option<[u64; 2]>,
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(error.contains("field `payload` needs 6 byte(s) of padding before it"));
    //         assert!(error.contains("Option payload would start at offset 2"));
    //         assert!(error.contains("payload starts at offset 8"));
    //     }
    //
    //     #[test]
    //     fn variable_offset_layout_reports_padding_for_vec_elements() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 flag: u8,
    //                 padding: [u8; 7],
    //                 #[capacity = 2]
    //                 values: Vec<[u64; 2]>,
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(error.contains("field `values` needs 7 byte(s) of padding before it"));
    //         assert!(error.contains("element 0 would start at offset 9"));
    //         assert!(error.contains("element 0 starts at offset 16"));
    //     }
    //
    //     #[test]
    //     fn variable_offset_layout_assumes_earlier_padding_errors_are_fixed() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 flag: u8,
    //                 payload: [u64; 2],
    //                 _pad1: [u8; 7],
    //                 #[capacity = 2]
    //                 values: Vec<[u64; 2]>,
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(error.contains("field `payload` needs 7 byte(s) of padding before it"));
    //         assert!(!error.contains("field `values`"));
    //     }
    //
    //     #[test]
    //     fn variable_offset_layout_rejects_alignment_above_eight() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 big: u128,
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(error.contains("field `big` cannot be borrowed by variable_offset_layout"));
    //         assert!(error.contains("alignment is 16 byte(s)"));
    //         assert!(error.contains("8-byte aligned"));
    //     }
    //
    //     #[test]
    //     fn variable_offset_layout_rejects_vec_without_capacity() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 values: Vec<u16>,
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(error.contains("Vec fields in variable_offset_layout require `#[capacity = N]`"));
    //     }
    //
    //     #[test]
    //     fn variable_offset_layout_rejects_capacity_on_non_vec() {
    //         let item: syn::ItemStruct = parse_quote! {
    //             struct Args {
    //                 #[capacity = 2]
    //                 value: u16,
    //             }
    //         };
    //
    //         let error = expand_variable_offset_layout("", &item)
    //             .unwrap_err()
    //             .to_string();
    //         assert!(
    //             error.contains("attributes are allowed on Vec or Option field only"),
    //             "error: {}",
    //             error
    //         );
    //     }
    //
    #[test]
    fn variable_offset_layout_rejects_implicit_without_option() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0, option = implicit", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "variable_offset_layout(option = implicit) requires at least one Option<T> field"
        ));
    }

    #[test]
    fn variable_offset_layout_rejects_implicit_with_vec() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: Option<u16>,
                #[flexible = 1]
                payload: Vec<u8>,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0, option = implicit", &item)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("variable_offset_layout(option = implicit) does not support Vec fields")
        );
    }

    #[test]
    fn variable_offset_layout_rejects_implicit_with_ambiguous_subset_sums() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                first: Option<u8>,
                second: Option<u16>,
                third: Option<[u8; 3]>,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0, option = implicit", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "variable_offset_layout(option = implicit) requires Option<T> payload sizes to have unique subset sums"
        ));
    }

    #[test]
    fn variable_offset_layout_rejects_unknown_parameters() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_variable_offset_layout("foo = bar", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "variable_offset_layout only supports `option = implicit`, `buffer_offset = 0..=7`, and `buffer_offset = unaligned`"
        ));
    }

    #[test]
    fn variable_offset_layout_requires_buffer_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_variable_offset_layout("", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "variable_offset_layout requires `buffer_offset = 0..=7` or `buffer_offset = unaligned`"
        ));
    }

    #[test]
    fn variable_offset_layout_rejects_invalid_buffer_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 8", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("buffer_offset must be in the range 0..=7"));
    }

    #[test]
    fn variable_offset_layout_accepts_unaligned_buffer_offset() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u64,
                key: Pubkey,
                #[flexible = 1]
                payload: Vec<u8>,
            }
        };

        expand_variable_offset_layout("buffer_offset = unaligned", &item).unwrap();
    }

    #[test]
    fn variable_offset_layout_rejects_unknown_buffer_offset_identifier() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                value: u16,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = dynamic", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("buffer_offset must be an integer in 0..=7 or `unaligned`"));
    }

    #[test]
    fn variable_offset_layout_rejects_unaligned_borrowed_field_requiring_alignment() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                values: [u64; 2],
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = unaligned", &item)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("field `values` cannot be borrowed with `buffer_offset = unaligned`")
        );
        assert!(error.contains("requires 8-byte alignment"));
    }

    #[test]
    fn variable_offset_layout_rejects_unaligned_vec_slice_requiring_alignment() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[flexible = 1]
                values: Vec<u16>,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = unaligned", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains(
            "field `values` cannot expose a slice view with `buffer_offset = unaligned`"
        ));
        assert!(error.contains("require 2-byte alignment"));
    }

    #[test]
    fn variable_offset_layout_accepts_flexible_len_width_eight() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                tag: u8,
                #[flexible = 8]
                payload: Vec<u8>,
            }
        };

        expand_variable_offset_layout("buffer_offset = 0", &item).unwrap();
    }

    #[test]
    fn variable_offset_layout_rejects_invalid_flexible_len_width() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[flexible = 9]
                payload: Vec<u8>,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("flexible must be in the range 1..=8"));
    }

    #[test]
    fn variable_offset_layout_rejects_borrowed_field_when_buffer_offset_prevents_alignment() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                values: [u64; 2],
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 1", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `values` cannot be borrowed with `buffer_offset = 1`"));
        assert!(error.contains("must be 8-byte aligned"));
    }

    #[test]
    fn variable_offset_layout_rejects_fixed_misaligned_vec_slice() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                flag: u8,
                #[flexible = 1]
                values: Vec<u64>,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `values` cannot expose a slice view"));
        assert!(error.contains("element 0 would start at offset 2"));
    }

    #[test]
    fn variable_offset_layout_rejects_vec_bool() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                #[flexible = 1]
                values: Vec<bool>,
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Vec<bool> is not supported by variable_offset_layout"));
    }

    #[test]
    fn variable_offset_layout_rejects_borrowed_field_after_unstable_variable_field() {
        let item: syn::ItemStruct = parse_quote! {
            struct Args {
                tag: u8,
                #[flexible = 1]
                prefix: Vec<u8>,
                values: [u64; 2],
            }
        };

        let error = expand_variable_offset_layout("buffer_offset = 0", &item)
            .unwrap_err()
            .to_string();
        assert!(error.contains("field `values` cannot be borrowed"));
        assert!(error.contains("earlier variable-sized fields make its actual address vary"));
    }
}
