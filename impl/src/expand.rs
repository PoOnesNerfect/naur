use crate::ast::{Enum, Field, Input, Struct};
use crate::attr::Trait;
use crate::generics::InferredBounds;
use crate::span::MemberSpan;
use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned, ToTokens};
use std::collections::BTreeSet as Set;
use syn::{
    Data, DeriveInput, GenericArgument, Member, PathArguments, Result, Token, Type, Visibility,
};

pub fn derive(node: &DeriveInput) -> Result<TokenStream> {
    let input = Input::from_syn(node)?;
    input.validate()?;
    Ok(match input {
        Input::Struct(input) => impl_struct(input),
        Input::Enum(input) => impl_enum(input),
    })
}

fn impl_struct(input: Struct) -> TokenStream {
    let ty = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let mut error_inferred_bounds = InferredBounds::new();

    let source_body = if let Some(transparent_attr) = &input.attrs.transparent {
        let only_field = &input.fields[0];
        if only_field.contains_generic {
            error_inferred_bounds.insert(only_field.ty, quote!(std::error::Error));
        }
        let member = &only_field.member;
        Some(quote_spanned! {transparent_attr.span=>
            std::error::Error::source(self.#member.as_dyn_error())
        })
    } else if let Some(source_field) = input.source_field() {
        let source = &source_field.member;
        if source_field.contains_generic {
            let ty = unoptional_type(source_field.ty);
            error_inferred_bounds.insert(ty, quote!(std::error::Error + 'static));
        }
        let asref = if type_is_option(source_field.ty) {
            Some(quote_spanned!(source.member_span()=> .as_ref()?))
        } else {
            None
        };
        let dyn_error = quote_spanned! {source_field.source_span()=>
            self.#source #asref.as_dyn_error()
        };
        Some(quote! {
            ::core::option::Option::Some(#dyn_error)
        })
    } else {
        None
    };
    let source_method = source_body.map(|body| {
        quote! {
            fn source(&self) -> ::core::option::Option<&(dyn std::error::Error + 'static)> {
                use thiserror::__private::AsDynError;
                #body
            }
        }
    });

    let provide_method = input.backtrace_field().map(|backtrace_field| {
        let request = quote!(request);
        let backtrace = &backtrace_field.member;
        let body = if let Some(source_field) = input.source_field() {
            let source = &source_field.member;
            let source_provide = if type_is_option(source_field.ty) {
                quote_spanned! {source.member_span()=>
                    if let ::core::option::Option::Some(source) = &self.#source {
                        source.thiserror_provide(#request);
                    }
                }
            } else {
                quote_spanned! {source.member_span()=>
                    self.#source.thiserror_provide(#request);
                }
            };
            let self_provide = if source == backtrace {
                None
            } else if type_is_option(backtrace_field.ty) {
                Some(quote! {
                    if let ::core::option::Option::Some(backtrace) = &self.#backtrace {
                        #request.provide_ref::<std::backtrace::Backtrace>(backtrace);
                    }
                })
            } else {
                Some(quote! {
                    #request.provide_ref::<std::backtrace::Backtrace>(&self.#backtrace);
                })
            };
            quote! {
                use thiserror::__private::ThiserrorProvide;
                #source_provide
                #self_provide
            }
        } else if type_is_option(backtrace_field.ty) {
            quote! {
                if let ::core::option::Option::Some(backtrace) = &self.#backtrace {
                    #request.provide_ref::<std::backtrace::Backtrace>(backtrace);
                }
            }
        } else {
            quote! {
                #request.provide_ref::<std::backtrace::Backtrace>(&self.#backtrace);
            }
        };
        quote! {
            fn provide<'_request>(&'_request self, #request: &mut std::error::Request<'_request>) {
                #body
            }
        }
    });

    let mut display_implied_bounds = Set::new();
    let display_body = if input.attrs.transparent.is_some() {
        let only_field = &input.fields[0].member;
        display_implied_bounds.insert((0, Trait::Display));
        Some(quote! {
            ::core::fmt::Display::fmt(&self.#only_field, __formatter)
        })
    } else if let Some(display) = &input.attrs.display {
        display_implied_bounds = display.implied_bounds.clone();
        let use_as_display = use_as_display(display.has_bonus_display);
        let pat = fields_pat(&input.fields);
        Some(quote! {
            #use_as_display
            #[allow(unused_variables, deprecated)]
            let Self #pat = self;
            #display
        })
    } else {
        None
    };
    let display_impl = display_body.map(|body| {
        let mut display_inferred_bounds = InferredBounds::new();
        for (field, bound) in display_implied_bounds {
            let field = &input.fields[field];
            if field.contains_generic {
                display_inferred_bounds.insert(field.ty, bound);
            }
        }
        let display_where_clause = display_inferred_bounds.augment_where_clause(input.generics);
        quote! {
            #[allow(unused_qualifications)]
            impl #impl_generics ::core::fmt::Display for #ty #ty_generics #display_where_clause {
                #[allow(clippy::used_underscore_binding)]
                fn fmt(&self, __formatter: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                    #body
                }
            }
        }
    });

    let from_impl = input.from_field().map(|from_field| {
        let backtrace_field = input.distinct_backtrace_field();
        let from = unoptional_type(from_field.ty);
        let body = from_initializer(from_field, backtrace_field);
        quote! {
            #[allow(unused_qualifications)]
            impl #impl_generics ::core::convert::From<#from> for #ty #ty_generics #where_clause {
                #[allow(deprecated)]
                fn from(source: #from) -> Self {
                    #ty #body
                }
            }
        }
    });

    let variant_traits_impl = if let Some(source) = input.source_field() {
        let trait_name = format_ident!("{}Throws", input.ident);
        let method_name = {
            let mut snake = String::new();
            for (i, ch) in input.ident.to_string().char_indices() {
                if i > 0 && ch.is_uppercase() {
                    snake.push('_');
                }
                snake.push(ch.to_ascii_lowercase());
            }
            snake = snake.trim_end_matches("_error").to_owned();
            snake
        };
        let throw_method = format_ident!("throw_{}", method_name);
        let with_method = format_ident!("throw_{}_with", method_name);

        let generics = {
            use proc_macro2::{Ident, Span};

            let mut generics = input.generics.clone();
            generics.params.push(syn::GenericParam::Type(
                Ident::new("__RETURN", Span::call_site()).into(),
            ));
            generics
        };
        let (thiserror_impl_generics, thiserror_ty_generics, _) = generics.split_for_impl();

        let is_source = |field: &Field<'_>| {
            if field.attrs.from.is_some() || field.attrs.source.is_some() {
                return true;
            }
            match &field.member {
                Member::Named(ident) if ident == "source" && source.member == field.member => true,
                _ => false,
            }
        };

        let (params, fields, types) = {
            use syn::{punctuated::Punctuated, token::Comma, Ident};

            let mut params = Punctuated::<TokenStream, Comma>::new();
            let mut fields = Punctuated::<Ident, Comma>::new();
            let mut types = Punctuated::<&Type, Comma>::new();

            for (i, field) in input.fields.iter().filter(|f| !is_source(f)).enumerate() {
                let field_ty = field.ty;

                let field_name = if let Some(field_name) = field.original.ident.as_ref() {
                    field_name.clone()
                } else {
                    format_ident!("_{}", i)
                };

                params.push(quote! {
                    #field_name : #field_ty
                });
                fields.push(field_name);
                types.push(field_ty);
            }

            (params, fields, types)
        };

        let source_ty = source.ty;

        let new_struct = if let Some(source_field) = source.original.ident.as_ref() {
            quote! {
                #ty {
                    #source_field : e,
                    #fields
                }
            }
        } else {
            quote! {
                #ty (e, #fields)
            }
        };

        let with_method_decl = (!params.is_empty()).then(|| quote!{
            fn #with_method<F: FnOnce() -> (#types)> (self, f: F) -> Result<__RETURN, #ty #ty_generics> #where_clause;
        });
        let with_method_impl = (!params.is_empty()).then(|| quote!{
            fn #with_method<F: FnOnce() -> (#types)> (self, f: F) -> Result<__RETURN, #ty #ty_generics> #where_clause {
                self.map_err(|e| {
                    let (#fields) = f();
                    #new_struct
                })
            }
        });

        Some(quote! {
            trait #trait_name #thiserror_impl_generics {
                fn #throw_method (self, #params) -> Result<__RETURN, #ty #ty_generics> #where_clause;
                #with_method_decl
            }
            impl #thiserror_impl_generics #trait_name #thiserror_ty_generics for Result<__RETURN, #source_ty> #where_clause {
                fn #throw_method (self, #params) -> Result<__RETURN, #ty #ty_generics> #where_clause {
                    self.map_err(|e| {
                        #new_struct
                    })
                }
                #with_method_impl
            }
        })
    } else {
        None
    };

    let error_trait = spanned_error_trait(input.original);
    if input.generics.type_params().next().is_some() {
        let self_token = <Token![Self]>::default();
        error_inferred_bounds.insert(self_token, Trait::Debug);
        error_inferred_bounds.insert(self_token, Trait::Display);
    }
    let error_where_clause = error_inferred_bounds.augment_where_clause(input.generics);

    quote! {
        #[allow(unused_qualifications)]
        impl #impl_generics #error_trait for #ty #ty_generics #error_where_clause {
            #source_method
            #provide_method
        }
        #display_impl
        #from_impl
        #variant_traits_impl
    }
}

fn impl_enum(input: Enum) -> TokenStream {
    let ty = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let mut error_inferred_bounds = InferredBounds::new();

    let source_method = if input.has_source() {
        let arms = input.variants.iter().map(|variant| {
            let ident = &variant.ident;
            if let Some(transparent_attr) = &variant.attrs.transparent {
                let only_field = &variant.fields[0];
                if only_field.contains_generic {
                    error_inferred_bounds.insert(only_field.ty, quote!(std::error::Error));
                }
                let member = &only_field.member;
                let source = quote_spanned! {transparent_attr.span=>
                    std::error::Error::source(transparent.as_dyn_error())
                };
                quote! {
                    #ty::#ident {#member: transparent} => #source,
                }
            } else if let Some(source_field) = variant.source_field() {
                let source = &source_field.member;
                if source_field.contains_generic {
                    let ty = unoptional_type(source_field.ty);
                    error_inferred_bounds.insert(ty, quote!(std::error::Error + 'static));
                }
                let asref = if type_is_option(source_field.ty) {
                    Some(quote_spanned!(source.member_span()=> .as_ref()?))
                } else {
                    None
                };
                let varsource = quote!(source);
                let dyn_error = quote_spanned! {source_field.source_span()=>
                    #varsource #asref.as_dyn_error()
                };
                quote! {
                    #ty::#ident {#source: #varsource, ..} => ::core::option::Option::Some(#dyn_error),
                }
            } else {
                quote! {
                    #ty::#ident {..} => ::core::option::Option::None,
                }
            }
        });
        Some(quote! {
            fn source(&self) -> ::core::option::Option<&(dyn std::error::Error + 'static)> {
                use thiserror::__private::AsDynError;
                #[allow(deprecated)]
                match self {
                    #(#arms)*
                }
            }
        })
    } else {
        None
    };

    let provide_method = if input.has_backtrace() {
        let request = quote!(request);
        let arms = input.variants.iter().map(|variant| {
            let ident = &variant.ident;
            match (variant.backtrace_field(), variant.source_field()) {
                (Some(backtrace_field), Some(source_field))
                    if backtrace_field.attrs.backtrace.is_none() =>
                {
                    let backtrace = &backtrace_field.member;
                    let source = &source_field.member;
                    let varsource = quote!(source);
                    let source_provide = if type_is_option(source_field.ty) {
                        quote_spanned! {source.member_span()=>
                            if let ::core::option::Option::Some(source) = #varsource {
                                source.thiserror_provide(#request);
                            }
                        }
                    } else {
                        quote_spanned! {source.member_span()=>
                            #varsource.thiserror_provide(#request);
                        }
                    };
                    let self_provide = if type_is_option(backtrace_field.ty) {
                        quote! {
                            if let ::core::option::Option::Some(backtrace) = backtrace {
                                #request.provide_ref::<std::backtrace::Backtrace>(backtrace);
                            }
                        }
                    } else {
                        quote! {
                            #request.provide_ref::<std::backtrace::Backtrace>(backtrace);
                        }
                    };
                    quote! {
                        #ty::#ident {
                            #backtrace: backtrace,
                            #source: #varsource,
                            ..
                        } => {
                            use thiserror::__private::ThiserrorProvide;
                            #source_provide
                            #self_provide
                        }
                    }
                }
                (Some(backtrace_field), Some(source_field))
                    if backtrace_field.member == source_field.member =>
                {
                    let backtrace = &backtrace_field.member;
                    let varsource = quote!(source);
                    let source_provide = if type_is_option(source_field.ty) {
                        quote_spanned! {backtrace.member_span()=>
                            if let ::core::option::Option::Some(source) = #varsource {
                                source.thiserror_provide(#request);
                            }
                        }
                    } else {
                        quote_spanned! {backtrace.member_span()=>
                            #varsource.thiserror_provide(#request);
                        }
                    };
                    quote! {
                        #ty::#ident {#backtrace: #varsource, ..} => {
                            use thiserror::__private::ThiserrorProvide;
                            #source_provide
                        }
                    }
                }
                (Some(backtrace_field), _) => {
                    let backtrace = &backtrace_field.member;
                    let body = if type_is_option(backtrace_field.ty) {
                        quote! {
                            if let ::core::option::Option::Some(backtrace) = backtrace {
                                #request.provide_ref::<std::backtrace::Backtrace>(backtrace);
                            }
                        }
                    } else {
                        quote! {
                            #request.provide_ref::<std::backtrace::Backtrace>(backtrace);
                        }
                    };
                    quote! {
                        #ty::#ident {#backtrace: backtrace, ..} => {
                            #body
                        }
                    }
                }
                (None, _) => quote! {
                    #ty::#ident {..} => {}
                },
            }
        });
        Some(quote! {
            fn provide<'_request>(&'_request self, #request: &mut std::error::Request<'_request>) {
                #[allow(deprecated)]
                match self {
                    #(#arms)*
                }
            }
        })
    } else {
        None
    };

    let display_impl = if input.has_display() {
        let mut display_inferred_bounds = InferredBounds::new();
        let has_bonus_display = input.variants.iter().any(|v| {
            v.attrs
                .display
                .as_ref()
                .map_or(false, |display| display.has_bonus_display)
        });
        let use_as_display = use_as_display(has_bonus_display);
        let void_deref = if input.variants.is_empty() {
            Some(quote!(*))
        } else {
            None
        };
        let arms = input.variants.iter().map(|variant| {
            let mut display_implied_bounds = Set::new();
            let display = match &variant.attrs.display {
                Some(display) => {
                    display_implied_bounds = display.implied_bounds.clone();
                    display.to_token_stream()
                }
                None => {
                    let only_field = match &variant.fields[0].member {
                        Member::Named(ident) => ident.clone(),
                        Member::Unnamed(index) => format_ident!("_{}", index),
                    };
                    display_implied_bounds.insert((0, Trait::Display));
                    quote!(::core::fmt::Display::fmt(#only_field, __formatter))
                }
            };
            for (field, bound) in display_implied_bounds {
                let field = &variant.fields[field];
                if field.contains_generic {
                    display_inferred_bounds.insert(field.ty, bound);
                }
            }
            let ident = &variant.ident;
            let pat = fields_pat(&variant.fields);
            quote! {
                #ty::#ident #pat => #display
            }
        });
        let arms = arms.collect::<Vec<_>>();
        let display_where_clause = display_inferred_bounds.augment_where_clause(input.generics);
        Some(quote! {
            #[allow(unused_qualifications)]
            impl #impl_generics ::core::fmt::Display for #ty #ty_generics #display_where_clause {
                fn fmt(&self, __formatter: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                    #use_as_display
                    #[allow(unused_variables, deprecated, clippy::used_underscore_binding)]
                    match #void_deref self {
                        #(#arms,)*
                    }
                }
            }
        })
    } else {
        None
    };

    let from_impls = input.variants.iter().filter_map(|variant| {
        let from_field = variant.from_field()?;
        let backtrace_field = variant.distinct_backtrace_field();
        let variant = &variant.ident;
        let from = unoptional_type(from_field.ty);
        let body = from_initializer(from_field, backtrace_field);
        Some(quote! {
            #[allow(unused_qualifications)]
            impl #impl_generics ::core::convert::From<#from> for #ty #ty_generics #where_clause {
                #[allow(deprecated)]
                fn from(source: #from) -> Self {
                    #ty::#variant #body
                }
            }
        })
    });

    let error_trait = spanned_error_trait(input.original);
    if input.generics.type_params().next().is_some() {
        let self_token = <Token![Self]>::default();
        error_inferred_bounds.insert(self_token, Trait::Debug);
        error_inferred_bounds.insert(self_token, Trait::Display);
    }
    let error_where_clause = error_inferred_bounds.augment_where_clause(input.generics);

    let variant_traits_impl: Vec<Option<TokenStream>> = {
        let generics = {
            use proc_macro2::{Ident, Span};

            let mut generics = input.generics.clone();
            generics.params.push(syn::GenericParam::Type(
                Ident::new("__RETURN", Span::call_site()).into(),
            ));
            generics
        };
        let (thiserror_impl_generics, thiserror_ty_generics, _) = generics.split_for_impl();

        input.variants.iter().map(|variant|{
            if let Some(source) = variant.source_field() {
                let variant_ident = &variant.ident;
                let trait_name = format_ident!("{}{}Throws", input.ident, variant_ident);
                let method_name = {
                    let mut snake = String::new();
                    for (i, ch) in variant_ident.to_string().char_indices() {
                        if i > 0 && ch.is_uppercase() {
                            snake.push('_');
                        }
                        snake.push(ch.to_ascii_lowercase());
                    }
                    snake = snake.trim_end_matches("_error").to_owned();
                    snake
                };
                let throw_method = format_ident!("throw_{}", method_name);
                let with_method = format_ident!("throw_{}_with", method_name);

                let is_source = |field: &Field<'_>| {
                    if field.attrs.from.is_some() || field.attrs.source.is_some() {
                        return true;
                    }
                    match &field.member {
                        Member::Named(ident) if ident == "source" && source.member == field.member => true,
                        _ => false,
                    }
                };

                let (params, fields, types) = {
                    use syn::{punctuated::Punctuated, token::Comma, Ident};

                    let mut params = Punctuated::<TokenStream, Comma>::new();
                    let mut fields = Punctuated::<Ident, Comma>::new();
                    let mut types = Punctuated::<&Type, Comma>::new();

                    for (i, field) in variant.fields.iter().filter(|f| !is_source(f)).enumerate() {
                        let field_ty = field.ty;

                        let field_name = if let Some(field_name) = field.original.ident.as_ref() {
                            field_name.clone()
                        } else {
                            format_ident!("_{}", i)
                        };

                        params.push(quote! {
                            #field_name : #field_ty
                        });
                        fields.push(field_name);
                        types.push(field_ty);
                    }

                    (params, fields, types)
                };

                let source_ty = source.ty;

                let new_struct = if let Some(source_field) = source.original.ident.as_ref() {
                    quote! {
                        #ty :: #variant_ident {
                            #source_field : e,
                            #fields
                        }
                    }
                } else {
                    quote! {
                        #ty :: #variant_ident (e, #fields)
                    }
                };

                let with_method_decl = (!params.is_empty()).then(|| quote!{
                    fn #with_method<F: FnOnce() -> (#types)> (self, f: F) -> Result<__RETURN, #ty #ty_generics> #where_clause;
                });
                let with_method_impl = (!params.is_empty()).then(|| quote!{
                    fn #with_method<F: FnOnce() -> (#types)> (self, f: F) -> Result<__RETURN, #ty #ty_generics> #where_clause {
                        self.map_err(|e| {
                            let (#fields) = f();
                            #new_struct
                        })
                    }
                });

                Some(quote! {
                    trait #trait_name #thiserror_impl_generics {
                        fn #throw_method (self, #params) -> Result<__RETURN, #ty #ty_generics> #where_clause;
                        #with_method_decl
                    }
                    impl #thiserror_impl_generics #trait_name #thiserror_ty_generics for Result<__RETURN, #source_ty> #where_clause {
                        fn #throw_method (self, #params) -> Result<__RETURN, #ty #ty_generics> #where_clause {
                            self.map_err(|e| {
                                #new_struct
                            })
                        }
                        #with_method_impl
                    }
                })
            } else {
                None
            }
        }).collect()
    };

    quote! {
        #[allow(unused_qualifications)]
        impl #impl_generics #error_trait for #ty #ty_generics #error_where_clause {
            #source_method
            #provide_method
        }
        #display_impl
        #(#from_impls)*
        #(#variant_traits_impl)*
    }
}

fn fields_pat(fields: &[Field]) -> TokenStream {
    let mut members = fields.iter().map(|field| &field.member).peekable();
    match members.peek() {
        Some(Member::Named(_)) => quote!({ #(#members),* }),
        Some(Member::Unnamed(_)) => {
            let vars = members.map(|member| match member {
                Member::Unnamed(member) => format_ident!("_{}", member),
                Member::Named(_) => unreachable!(),
            });
            quote!((#(#vars),*))
        }
        None => quote!({}),
    }
}

fn use_as_display(needs_as_display: bool) -> Option<TokenStream> {
    if needs_as_display {
        Some(quote! {
            use thiserror::__private::AsDisplay as _;
        })
    } else {
        None
    }
}

fn from_initializer(from_field: &Field, backtrace_field: Option<&Field>) -> TokenStream {
    let from_member = &from_field.member;
    let some_source = if type_is_option(from_field.ty) {
        quote!(::core::option::Option::Some(source))
    } else {
        quote!(source)
    };
    let backtrace = backtrace_field.map(|backtrace_field| {
        let backtrace_member = &backtrace_field.member;
        if type_is_option(backtrace_field.ty) {
            quote! {
                #backtrace_member: ::core::option::Option::Some(std::backtrace::Backtrace::capture()),
            }
        } else {
            quote! {
                #backtrace_member: ::core::convert::From::from(std::backtrace::Backtrace::capture()),
            }
        }
    });
    quote!({
        #from_member: #some_source,
        #backtrace
    })
}

fn type_is_option(ty: &Type) -> bool {
    type_parameter_of_option(ty).is_some()
}

fn unoptional_type(ty: &Type) -> TokenStream {
    let unoptional = type_parameter_of_option(ty).unwrap_or(ty);
    quote!(#unoptional)
}

fn type_parameter_of_option(ty: &Type) -> Option<&Type> {
    let path = match ty {
        Type::Path(ty) => &ty.path,
        _ => return None,
    };

    let last = path.segments.last().unwrap();
    if last.ident != "Option" {
        return None;
    }

    let bracketed = match &last.arguments {
        PathArguments::AngleBracketed(bracketed) => bracketed,
        _ => return None,
    };

    if bracketed.args.len() != 1 {
        return None;
    }

    match &bracketed.args[0] {
        GenericArgument::Type(arg) => Some(arg),
        _ => None,
    }
}

fn spanned_error_trait(input: &DeriveInput) -> TokenStream {
    let vis_span = match &input.vis {
        Visibility::Public(vis) => Some(vis.span),
        Visibility::Restricted(vis) => Some(vis.pub_token.span),
        Visibility::Inherited => None,
    };
    let data_span = match &input.data {
        Data::Struct(data) => data.struct_token.span,
        Data::Enum(data) => data.enum_token.span,
        Data::Union(data) => data.union_token.span,
    };
    let first_span = vis_span.unwrap_or(data_span);
    let last_span = input.ident.span();
    let path = quote_spanned!(first_span=> std::error::);
    let error = quote_spanned!(last_span=> Error);
    quote!(#path #error)
}
