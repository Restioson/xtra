use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_quote, DeriveInput, GenericParam, WherePredicate};

#[proc_macro_derive(Actor)]
pub fn actor_derive(input: TokenStream) -> TokenStream {
    let derive_input = syn::parse::<DeriveInput>(input).expect("macro to be used as custom-derive");

    actor_derive_impl(derive_input).unwrap_or_else(|e| e).into()
}

fn actor_derive_impl(
    input: DeriveInput,
) -> Result<proc_macro2::TokenStream, proc_macro2::TokenStream> {
    let send_and_static_bound = send_and_static_bounds(&input);

    let actor_ident = input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();

    let where_clause = match where_clause.cloned() {
        None => parse_quote! { where #(#send_and_static_bound),* },
        Some(mut existing) => {
            existing.predicates.extend(send_and_static_bound);

            existing
        }
    };

    Ok(quote! {
        #[xtra::prelude::async_trait]
        impl #impl_generics xtra::Actor for #actor_ident #type_generics #where_clause {
            type Stop = ();

            async fn stopped(self) { }
        }
    })
}

/// Generics a `: Send + 'static` predicate for each type parameter present in the generics.
fn send_and_static_bounds(input: &DeriveInput) -> Vec<WherePredicate> {
    input
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            GenericParam::Type(tp) => Some(&tp.ident),
            GenericParam::Lifetime(_) => None,
            GenericParam::Const(_) => None,
        })
        .map(|ident| {
            parse_quote! {
                #ident: Send + 'static
            }
        })
        .collect::<Vec<WherePredicate>>()
}
