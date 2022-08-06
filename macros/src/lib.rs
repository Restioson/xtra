use proc_macro::TokenStream;
use quote::quote;
use syn::DeriveInput;

#[proc_macro_derive(Actor)]
pub fn actor_derive(input: TokenStream) -> TokenStream {
    let derive_input = syn::parse::<DeriveInput>(input).expect("macro to be used as custom-derive");

    actor_derive_impl(derive_input).unwrap_or_else(|e| e).into()
}

fn actor_derive_impl(
    input: DeriveInput,
) -> Result<proc_macro2::TokenStream, proc_macro2::TokenStream> {
    let actor_ident = input.ident;

    Ok(quote! {
        #[xtra::prelude::async_trait]
        impl xtra::Actor for #actor_ident {
            type Stop = ();

            async fn stopped(self) { }
        }
    })
}
