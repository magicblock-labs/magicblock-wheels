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

    let mut offset = 0usize;
    let mut offsets = vec![0];

    let mut total_len_expr = quote!();
    let mut fields_encode_expr = quote!();

    let mut where_bounds = Vec::<proc_macro2::TokenStream>::new();
    let mut view_methods = Vec::new();

    let mut validate_steps = Vec::new();
    let mut layout_error: Option<syn::Error> = None;

    let mut flexible_field = None;
    let field_count = fields.named.len();
    for (index, field) in fields.named.iter_mut().enumerate() {
        let field_ident = field.ident.as_ref().expect("named field");

        let is_last_field = index + 1 == field_count;

        let layout = parse_field_layout(field, is_last_field)?;

        strip_field_attr(&mut field.attrs, FIELD_ATTRIBUTES);

        match layout.check_ref_alignment(offset, field_ident) {
            Ok(Some(issue)) => {
                if let Some(existing) = &mut layout_error {
                    existing.combine(issue.error);
                } else {
                    layout_error = Some(issue.error);
                }
                offset += issue.padding;
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

        validate_steps.push(layout.gen_validate_vec_len(offset, field_ident));
        view_methods.push(layout.gen_view_methods(offset, field_ident)?);

        fields_encode_expr = layout.gen_field_encode(fields_encode_expr, offset, field_ident);

        if let Some(bound) = layout.bound() {
            where_bounds.push(bound);
        }

        match layout.slot_min_len_expr() {
            Ok((slot_len_expr, slot_len)) => {
                offset += slot_len; // layout.slot_min_len();
                offsets.push(offset);
                total_len_expr = if total_len_expr.is_empty() {
                    quote!(#slot_len_expr)
                } else {
                    quote!(#total_len_expr + #slot_len_expr)
                };
            }
            Err(len_width) => {
                assert!(is_last_field, "field must the last item");

                // the last offset becomes datalen which in this case is MIN_DATA_LEN
                offsets.push(*offsets.last().unwrap());

                flexible_field = Some((field_ident, len_width));
                // total_len_expr = if total_len_expr.is_empty() {
                //     quote!(#len_width)
                // } else {
                //     quote!(#total_len_expr + #len_width)
                // };
            }
        }
    }

    let field_count_expr = {
        let field_count = usize_lit(field_count);
        quote!(#field_count)
    };
    let (offsets_expr, datalen) = {
        // I could use offsets directly but that generates the code littered
        // with usize suffixes, e.g:
        //
        //  const OFFSETS: [usize; 6usize] = [0usize, 4usize, 12usize, 45usize, 18usize];
        //
        // I hate this, which is why I'm converting Vec<usize> into Vec<LitInt>.
        //
        let datalen = offsets.pop().unwrap();
        let offsets: Vec<_> = offsets.into_iter().map(usize_lit).collect();

        // the last one isn't offset to any member, but
        // actually represents the size of the struct

        (quote! { [#(#offsets),*] }, datalen)
    };

    if let Some(err) = layout_error {
        return Err(err);
    }

    let where_clause = impl_where_clause(&where_bounds);

    let msg = format!("Sum of encodable-sizes must be {}.", datalen);

    let (datalen_vars, datalen_check, check_logfmt, encoding_buf_var, encoding_ret_ty) =
        if let Some((flexible_field, comptime_optlen)) = flexible_field {
            let max_datalen = usize_lit(datalen + comptime_optlen.max_datalen());
            let logfmt = format!(
                "bytes [len={{}}] cannot be deserialized to {} which needs at least {} or at most {} ytes",
                 struct_name, datalen, max_datalen
            );
            (
                quote! {
                    pub const MIN_DATA_LEN: usize = #total_len_expr;
                    pub const MAX_DATA_LEN: usize = #max_datalen;
                },
                quote!(bytes.len() < Self::MIN_DATA_LEN || bytes.len() > Self::MAX_DATA_LEN),
                logfmt,
                match comptime_optlen {
                    ComptimeOptionalLen::ArrayLen {
                        len_width,
                        elem_size,
                    } => quote!(
                        ::alloc::vec![0; #datalen + if self.#flexible_field.is_empty() { 0 } else { #len_width + self.#flexible_field.len() * #elem_size }]
                    ),
                    ComptimeOptionalLen::Option { value_size } => quote! {
                        ::alloc::vec![0; #datalen + self.#flexible_field.as_ref().map(|_| 1 + #value_size).unwrap_or(0)]
                    },
                },
                quote!(::alloc::vec::Vec<u8>),
            )
        } else {
            let logfmt = format!(
                "bytes [len={{}}] cannot be deserialized to {} which needs exactly {} bytes",
                struct_name, datalen
            );
            (
                quote!(pub const DATA_LEN: usize = #total_len_expr;),
                quote!(bytes.len() != Self::DATA_LEN),
                logfmt,
                quote!([0; #datalen]),
                quote!([u8; #datalen]),
            )
        };

    Ok(quote! {
        #emitted_input

        impl #struct_name {
            #[doc = #msg]
            #datalen_vars

            #[doc = "Byte offsets marking the start of each field"]
            pub const OFFSETS: [usize; #field_count_expr] = #offsets_expr;

            pub fn decode(
                bytes: &[u8],
            ) -> core::result::Result<#view_name<'_>, ::pinocchio::error::ProgramError> {
                Self::__validate_bytes(bytes)?;
                Ok(#view_name { bytes })
            }

            pub fn encode_to(&self, bytes: &mut [u8]) -> core::result::Result<(), ::pinocchio::error::ProgramError> {
                #fields_encode_expr;
                Ok(())
            }

            pub fn encode(&self) -> core::result::Result<#encoding_ret_ty, ::pinocchio::error::ProgramError> {
                let mut bytes = #encoding_buf_var;
                self.encode_to(&mut bytes)?;
                Ok(bytes)
            }

            fn __validate_bytes(
                bytes: &[u8],
            ) -> core::result::Result<(), ::pinocchio::error::ProgramError> {
                if #datalen_check {
                    // ::pinocchio_log::log!("bytes [len={}] cannot be deserialized to {} which needs exactly {} bytes", bytes.len(), stringify!(#struct_name), Self::DATA_LEN);
                    ::pinocchio_log::log!(#check_logfmt, bytes.len());
                    return Err(
                        ::pinocchio::error::ProgramError::InvalidInstructionData,
                    );
                } else if bytes.as_ptr().align_offset(8) != 0 {
                    ::pinocchio_log::log!("bytes [align_offset={}] cannot be deserialized to {} which requires 8-byte alignment", bytes.as_ptr().align_offset(8), stringify!(#struct_name));
                    return Err(
                        ::pinocchio::error::ProgramError::InvalidInstructionData,
                    );
                }

                #(#validate_steps)*

                Ok(())
            }

            fn __validate_option(
                bytes: &[u8],
                offset: usize,
                field_name: &'static str,
            ) -> core::result::Result<(), ::pinocchio::error::ProgramError> {
                match bytes[offset] {
                    0 | 1 => {}

                    tag => {
                        ::pinocchio_log::log!("Invalid Option tag for field {}::{} : tag = {} (which should be either 0 or 1)", stringify!(#struct_name), field_name, tag);
                        return Err(::pinocchio::error::ProgramError::InvalidInstructionData);
                    }
                }
                Ok(())
            }

            fn __validate_vec_len(
                bytes: &[u8],
                offset: usize,
                capacity: usize,
                len_width: usize,
                field_name: &'static str,
            ) -> core::result::Result<(), ::pinocchio::error::ProgramError> {
                let len = match len_width {
                    1 =>  bytes[offset] as usize,
                    2 => {
                        let raw: [u8; 2] =bytes[offset..offset + 2].try_into().expect("validated len");
                        u16::from_le_bytes(raw) as usize
                    },
                    _ => {
                        unreachable!()
                    }
                };
                if len > capacity {
                    ::pinocchio_log::log!("Invalid Vec length for field {}::{} : capacity = {}, len = {}", stringify!(#struct_name), field_name, capacity, len);
                    return Err(::pinocchio::error::ProgramError::InvalidInstructionData);
                }
                Ok(())
            }
        }

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
        elem: FixedValueKind,
        capacity: Capacity,
    },
}

#[derive(Clone, Copy)]
enum ComptimeOptionalLen {
    ArrayLen { len_width: usize, elem_size: usize },
    Option { value_size: usize },
}

impl ComptimeOptionalLen {
    // max datalen in bytes
    fn max_datalen(self) -> usize {
        match self {
            ComptimeOptionalLen::ArrayLen {
                len_width,
                elem_size,
            } => len_width + (2usize.pow(len_width as u32 * 8) - 1) * elem_size,
            ComptimeOptionalLen::Option { value_size } => 1 + value_size,
        }
    }
}

struct PaddingIssue {
    padding: usize,
    error: syn::Error,
}

impl FixedFieldKind {
    fn slot_min_len_expr(&self) -> Result<(proc_macro2::TokenStream, usize), ComptimeOptionalLen> {
        match self {
            Self::Value { value, optional } => {
                let value_size_expr = value.size_expr();
                match optional {
                    Some(Optional::Fixed) => Ok((quote!((1 + #value_size_expr)), 1 + value.size())),
                    Some(Optional::Flexible) => Err(ComptimeOptionalLen::Option {
                        value_size: value.size(),
                    }),
                    None => Ok((value_size_expr, value.size())),
                }
            }
            Self::Vec { elem, capacity } => {
                let elem_ty = elem.ty();
                let len_width_lit = capacity.len_width_lit();
                capacity
                    .comptime_capacity_lit()
                    .map(|cap| {
                        (
                            quote!((#len_width_lit + core::mem::size_of::<#elem_ty>() * #cap)),
                            capacity.len_width()
                                + elem.size() * capacity.comptime_capacity().unwrap(),
                        )
                    })
                    .ok_or(ComptimeOptionalLen::ArrayLen {
                        len_width: capacity.len_width(),
                        elem_size: elem.size(),
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

    fn gen_validate_vec_len(&self, offset: usize, field_ident: &Ident) -> proc_macro2::TokenStream {
        let field_name = field_ident.to_string();
        let offset_lit = usize_lit(offset);
        let offset_expr = quote!(#offset_lit);
        match self {
            Self::Value { optional, .. } => {
                if optional.is_some() {
                    quote! {
                        Self::__validate_option(bytes, #offset_expr, #field_name)?;
                    }
                } else {
                    quote! {}
                }
            }
            Self::Vec { capacity, .. } => {
                let capacity_lit = capacity
                    .comptime_capacity_lit()
                    .unwrap_or(usize_lit(MAX_CAPACITY));
                let len_width_lit = capacity.len_width_lit();
                quote! {
                    Self::__validate_vec_len(bytes, #offset_expr, #capacity_lit, #len_width_lit, #field_name)?;
                }
            }
        }
    }

    fn gen_field_encode(
        &self,
        fields_encode_expr: proc_macro2::TokenStream,
        offset: usize,
        field_ident: &Ident,
    ) -> proc_macro2::TokenStream {
        let offset = usize_lit(offset);
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
                let elem_size = usize_lit(elem.size());
                let len_width_ty = capacity.len_width_ty();
                let len_width = capacity.len_width_lit();
                if let Some(cap) = capacity.comptime_capacity_lit() {
                    quote! {
                        #fields_encode_expr

                        if self.#field_ident.len() > #cap {
                            return Err(::pinocchio::error::ProgramError::InvalidRealloc);
                        }

                        bytes[#offset..#offset + #len_width].copy_from_slice(::bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
                        bytes[#offset + #len_width..#offset + #len_width + self.#field_ident.len() * #elem_size].copy_from_slice(::bytemuck::cast_slice(&self.#field_ident.as_slice()));
                        if self.#field_ident.len() < #cap {
                             bytes[#offset + #len_width + self.#field_ident.len() * #elem_size..#offset + #len_width + #cap * #elem_size].fill(0);
                        }
                    }
                } else {
                    // it must be the last field of Vec type with #[capacity = flexible]
                    let max_capacity = usize_lit(MAX_CAPACITY);
                    quote! {
                        #fields_encode_expr

                        if self.#field_ident.len() > #max_capacity {
                            return Err(::pinocchio::error::ProgramError::InvalidRealloc);
                        } else if !self.#field_ident.is_empty() {
                            bytes[#offset..#offset + #len_width].copy_from_slice(::bytemuck::bytes_of(&(self.#field_ident.len() as #len_width_ty)));
                            bytes[#offset + #len_width..#offset + #len_width + self.#field_ident.len() * #elem_size].copy_from_slice(::bytemuck::cast_slice(&self.#field_ident.as_slice()));
                        } else {
                            // Note that it is an empty-vector scenario in which case we do not have any 'buffer' to write anything (even zeroes) to
                        }
                    }
                }
            }
        }
    }

    fn gen_view_methods(
        &self,
        offset: usize,
        field_ident: &Ident,
    ) -> syn::Result<proc_macro2::TokenStream> {
        match self {
            Self::Value { value, optional } => {
                let ty = value.ty();
                let access_mode = value.access_mode();
                let getter_body = getter_tokens(value, offset)?;

                match (optional.is_some(), access_mode) {
                    (false, AccessMode::Copy) => Ok(quote! {
                        pub fn #field_ident(&self) -> #ty {
                            #getter_body
                        }
                    }),
                    (false, AccessMode::Ref) => Ok(quote! {
                        pub fn #field_ident(&self) -> &#ty {
                            #getter_body
                        }
                    }),
                    (true, AccessMode::Copy) => {
                        let value_body = getter_tokens(value, offset + 1)?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<#ty> {
                                (self.bytes[(#offset)] != 0).then(||#value_body)
                            }
                        })
                    }
                    (true, AccessMode::Ref) => {
                        let value_body = getter_tokens(value, offset + 1)?;
                        Ok(quote! {
                            pub fn #field_ident(&self) -> core::option::Option<&#ty> {
                                (self.bytes[(#offset)] != 0).then(||#value_body)
                            }
                        })
                    }
                }
            }
            Self::Vec { elem, capacity } => {
                let elem_ty = elem.ty();
                let len_expr = read_len_expr(offset, capacity.len_width());
                let offset = usize_lit(offset);
                let len_width_lit = capacity.len_width_lit();
                if let Some(cap) = capacity.comptime_capacity_lit() {
                    let capacity_name = format_ident!("{}_capacity", accessor_ident(field_ident));
                    Ok(quote! {
                        pub fn #field_ident(&self) -> &[#elem_ty] {
                            let len = #len_expr;
                            let start = #offset + #len_width_lit;
                            let end = start + (len * core::mem::size_of::<#elem_ty>());
                            ::bytemuck::cast_slice::<u8, #elem_ty>(&self.bytes[start..end])
                        }

                        pub const fn #capacity_name(&self) -> usize {
                            #cap
                        }
                    })
                } else {
                    Ok(quote! {
                        pub fn #field_ident(&self) -> &[#elem_ty] {
                            let len = #len_expr;
                            let start = #offset + #len_width_lit;
                            let end = start + (len * core::mem::size_of::<#elem_ty>());
                            ::bytemuck::cast_slice::<u8, #elem_ty>(&self.bytes[start..end])
                        }
                    })
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
        return Ok(FixedFieldKind::Vec {
            elem: parse_value_kind(elem_ty, UNSUPPORTED_FIELD_MESSAGE)?,
            capacity,
        });
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

fn bytes_slice_expr(offset: usize, len: usize) -> proc_macro2::TokenStream {
    let offset = usize_lit(offset);
    let len = usize_lit(len);
    quote!(&self.bytes[#offset..#offset + #len])
}

fn getter_tokens(value: &FixedValueKind, offset: usize) -> syn::Result<proc_macro2::TokenStream> {
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

fn read_len_expr(offset: usize, len_width: usize) -> proc_macro2::TokenStream {
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
