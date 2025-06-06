pub(crate) mod api_error;
pub(crate) mod diesel;
pub(crate) mod generate_permissions;
pub(crate) mod generate_schema;
pub(crate) mod misc;
pub(crate) mod operation;
pub(crate) mod schema;
pub(crate) mod to_encryptable;
pub(crate) mod try_get_enum;

mod helpers;

use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

pub(crate) use self::{
    api_error::api_error_derive_inner,
    diesel::{diesel_enum_derive_inner, diesel_enum_text_derive_inner},
    generate_permissions::generate_permissions_inner,
    generate_schema::polymorphic_macro_derive_inner,
    schema::validate_schema_derive,
    to_encryptable::derive_to_encryption,
};

pub(crate) fn debug_as_display_inner(ast: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &ast.ident;
    let (impl_generics, ty_generics, where_clause) = ast.generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::core::fmt::Display for #name #ty_generics #where_clause {
            fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::result::Result<(), ::core::fmt::Error> {
                f.write_str(&format!("{:?}", self))
            }
        }
    })
}
