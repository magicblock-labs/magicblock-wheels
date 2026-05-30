use proc_macro2::Span;
use quote::{quote, ToTokens};
use syn::{Attribute, Expr, ExprLit, GenericArgument, Lit, LitInt, PathArguments, Type, TypePath};

pub(crate) enum AccessMode {
    Copy,
    Ref,
}

#[derive(Clone)]
pub(crate) enum FixedValueKind {
    Bool { ty: Type },
    Integer { ty: Type, size: usize, align: usize },
    Pubkey { ty: Type },
    Array { ty: Type, size: usize, align: usize },
}

impl FixedValueKind {
    pub(crate) fn ty(&self) -> &Type {
        match self {
            Self::Bool { ty }
            | Self::Integer { ty, .. }
            | Self::Pubkey { ty }
            | Self::Array { ty, .. } => ty,
        }
    }

    pub(crate) fn size(&self) -> usize {
        match self {
            Self::Bool { .. } => 1,
            Self::Pubkey { .. } => 32,
            Self::Integer { size, .. } | Self::Array { size, .. } => *size,
        }
    }

    pub(crate) fn align(&self) -> usize {
        match self {
            Self::Bool { .. } | Self::Pubkey { .. } => 1,
            Self::Integer { align, .. } | Self::Array { align, .. } => *align,
        }
    }

    pub(crate) fn access_mode(&self) -> AccessMode {
        if self.size() > 8 {
            AccessMode::Ref
        } else {
            AccessMode::Copy
        }
    }

    pub(crate) fn needs_pod_bound(&self) -> bool {
        matches!(self, Self::Pubkey { .. } | Self::Array { .. })
    }

    pub(crate) fn size_expr(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Pubkey { .. } => quote!(32usize),
            _ => {
                let ty = self.ty();
                quote!(core::mem::size_of::<#ty>())
            }
        }
    }
}

pub(crate) fn parse_value_kind(
    ty: &Type,
    unsupported_field_message: &str,
) -> syn::Result<FixedValueKind> {
    if is_bool(ty) {
        return Ok(FixedValueKind::Bool { ty: ty.clone() });
    }

    if is_pubkey(ty) {
        return Ok(FixedValueKind::Pubkey { ty: ty.clone() });
    }

    if let Some((size, align)) = integer_size_and_align(ty) {
        return Ok(FixedValueKind::Integer {
            ty: ty.clone(),
            size,
            align,
        });
    }

    let (size, align) = fixed_array_size_and_align(ty, unsupported_field_message)?;
    Ok(FixedValueKind::Array {
        ty: ty.clone(),
        size,
        align,
    })
}

pub(crate) fn ensure_allow_dead_code(attrs: &mut Vec<Attribute>) {
    if attrs.iter().any(is_allow_dead_code) {
        return;
    }

    attrs.push(syn::parse_quote!(#[allow(dead_code)]));
}

fn is_allow_dead_code(attr: &Attribute) -> bool {
    if !attr.path().is_ident("allow") {
        return false;
    }

    let mut found = false;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("dead_code") {
            found = true;
        }
        Ok(())
    });
    found
}

pub(crate) fn impl_where_clause(bounds: &[proc_macro2::TokenStream]) -> proc_macro2::TokenStream {
    if bounds.is_empty() {
        quote!()
    } else {
        quote!(where #(#bounds,)*)
    }
}

pub(crate) fn strip_field_attr(attrs: &mut Vec<Attribute>, field_attributes: &[&str]) {
    attrs.retain(|attr| {
        !field_attributes
            .iter()
            .any(|attribute| attr.path().is_ident(attribute))
    });
}

pub(crate) fn option_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return None;
    };
    let segment = path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    Some(inner)
}

pub(crate) fn vec_inner<'a>(ty: &'a Type, layout_name: &str) -> syn::Result<Option<&'a Type>> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return Ok(None);
    };
    let Some(segment) = path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != "Vec" {
        return Ok(None);
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            &segment.arguments,
            format!("Vec requires exactly one type argument in {layout_name}: Vec<T>"),
        ));
    };
    if args.args.len() != 1 {
        return Err(syn::Error::new_spanned(
            args,
            format!("Vec requires exactly one type argument in {layout_name}: Vec<T>"),
        ));
    }

    let Some(GenericArgument::Type(elem_ty)) = args.args.first() else {
        return Err(syn::Error::new_spanned(args, "Vec element must be a type"));
    };

    Ok(Some(elem_ty))
}

fn integer_primitive_name(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() || type_path.path.segments.len() != 1 {
        return None;
    }
    let ident = type_path.path.segments[0].ident.to_string();
    match ident.as_str() {
        "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" | "i128" | "u128" | "isize"
        | "usize" => Some(ident),
        _ => None,
    }
}

pub(crate) fn is_string(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path.qself.is_none()
        && type_path.path.segments.len() == 1
        && type_path.path.segments[0].ident == "String"
}

pub(crate) fn is_bool(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path.qself.is_none()
        && type_path.path.segments.len() == 1
        && type_path.path.segments[0].ident == "bool"
}

pub(crate) fn is_pubkey(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };

    let mut segments = path.segments.iter();
    let Some(first) = segments.next() else {
        return false;
    };
    let second = segments.next();
    if segments.next().is_some() {
        return false;
    }

    if let Some(second) = second {
        return (first.ident == "wheels" && second.ident == "Pubkey")
            || (first.ident == "pinocchio" && second.ident == "Address");
    }

    first.ident == "Pubkey" || first.ident == "Address"
}

pub(crate) fn usize_lit(value: usize) -> LitInt {
    LitInt::new(&value.to_string(), Span::call_site())
}

fn parse_integer_expr(
    ty: &Type,
    bytes_expr: proc_macro2::TokenStream,
    error_message: &str,
) -> syn::Result<proc_macro2::TokenStream> {
    let Some(name) = integer_primitive_name(ty) else {
        return Err(syn::Error::new_spanned(ty, error_message));
    };
    let ty_tokens = ty.to_token_stream();
    Ok(match name.as_str() {
        "i8" => quote!(i8::from_le_bytes([(#bytes_expr)[0]])),
        "u8" => quote!(u8::from_le_bytes([(#bytes_expr)[0]])),
        _ => quote!({
            let raw: [u8; core::mem::size_of::<#ty_tokens>()] = (#bytes_expr)
                .try_into()
                .expect("validated slice length");
            <#ty_tokens>::from_le_bytes(raw)
        }),
    })
}

fn integer_size_and_align(ty: &Type) -> Option<(usize, usize)> {
    match integer_primitive_name(ty).as_deref() {
        Some("i8" | "u8") => Some((1, 1)),
        Some("i16" | "u16") => Some((2, 2)),
        Some("i32" | "u32") => Some((4, 4)),
        Some("i64" | "u64") => Some((8, 8)),
        Some("i128" | "u128") => Some((16, 16)),
        Some("isize" | "usize") => {
            let size = core::mem::size_of::<usize>();
            Some((size, size))
        }
        _ => None,
    }
}

fn fixed_array_size_and_align(
    ty: &Type,
    unsupported_field_message: &str,
) -> syn::Result<(usize, usize)> {
    let Type::Array(array) = ty else {
        return Err(syn::Error::new_spanned(ty, unsupported_field_message));
    };

    let Expr::Lit(ExprLit {
        lit: Lit::Int(len_lit),
        ..
    }) = &array.len
    else {
        return Err(syn::Error::new_spanned(
            &array.len,
            "array length must be an integer literal",
        ));
    };

    let len = len_lit.base10_parse::<usize>()?;
    let (elem_size, elem_align) = if let Some(size_align) = integer_size_and_align(&array.elem) {
        size_align
    } else {
        fixed_array_size_and_align(&array.elem, unsupported_field_message)?
    };

    Ok((len * elem_size, elem_align))
}

pub(crate) fn read_copy_expr(
    value: &FixedValueKind,
    bytes_expr: proc_macro2::TokenStream,
    layout_name: &str,
    integer_error_message: &str,
) -> syn::Result<proc_macro2::TokenStream> {
    match value {
        FixedValueKind::Bool { .. } => Ok(quote!((#bytes_expr)[0] != 0)),
        FixedValueKind::Integer { ty, .. } => {
            parse_integer_expr(ty, bytes_expr, integer_error_message)
        }
        FixedValueKind::Array { ty, .. } => Ok(quote! {
            unsafe { core::ptr::read_unaligned((#bytes_expr).as_ptr().cast::<#ty>()) }
        }),
        FixedValueKind::Pubkey { .. } => Err(syn::Error::new_spanned(
            value.ty(),
            format!("Pubkey fields are borrowed by {layout_name}"),
        )),
    }
}
