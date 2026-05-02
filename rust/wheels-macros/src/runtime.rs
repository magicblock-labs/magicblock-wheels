use proc_macro2::Span;
use quote::quote;
use syn::Ident;

pub(crate) fn runtime_crate(name: &str) -> proc_macro2::TokenStream {
    match proc_macro_crate::crate_name(name) {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(crate),
        Ok(proc_macro_crate::FoundCrate::Name(found)) => {
            let ident = Ident::new(&found.replace('-', "_"), Span::call_site());
            quote!(::#ident)
        }
        Err(_) => {
            let ident = Ident::new(&name.replace('-', "_"), Span::call_site());
            quote!(::#ident)
        }
    }
}
