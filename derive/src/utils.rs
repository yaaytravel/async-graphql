use std::collections::HashSet;

use darling::FromMeta;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{
    Attribute, Error, Expr, ExprLit, ExprPath, FnArg, Ident, ImplItemFn, Lifetime, Lit, LitStr,
    Meta, Pat, PatIdent, Type, TypeGroup, TypeParamBound, TypeReference, parse_quote,
    visit::Visit,
    visit_mut::{self, VisitMut},
};
use thiserror::Error;

use crate::args::{self, Deprecation, TypeDirectiveLocation, Visible};

#[derive(Error, Debug)]
pub enum GeneratorError {
    #[error("{0}")]
    Syn(#[from] syn::Error),

    #[error("{0}")]
    Darling(#[from] darling::Error),
}

impl GeneratorError {
    pub fn write_errors(self) -> TokenStream {
        match self {
            GeneratorError::Syn(err) => err.to_compile_error(),
            GeneratorError::Darling(err) => err.write_errors(),
        }
    }
}

pub type GeneratorResult<T> = std::result::Result<T, GeneratorError>;

pub fn get_crate_path(crate_path: &Option<syn::Path>, internal: bool) -> syn::Path {
    if internal {
        parse_quote! { crate }
    } else if let Some(path) = crate_path {
        path.clone()
    } else {
        let name = match crate_name("async-graphql") {
            Ok(FoundCrate::Name(name)) => name,
            Ok(FoundCrate::Itself) | Err(_) => "async_graphql".to_string(),
        };
        let ident = Ident::new(&name, Span::call_site());
        parse_quote! { ::#ident }
    }
}

pub fn generate_guards(
    crate_name: &syn::Path,
    expr: &Expr,
    map_err: TokenStream,
) -> GeneratorResult<TokenStream> {
    let code = quote! {{
        use #crate_name::GuardExt;
        #expr
    }};
    Ok(quote! {
        #crate_name::Guard::check(&#code, &ctx).await #map_err ?;
    })
}

pub fn get_rustdoc(attrs: &[Attribute]) -> GeneratorResult<Option<TokenStream>> {
    let mut full_docs: Vec<TokenStream> = vec![];
    let mut combined_docs_literal = String::new();
    for attr in attrs {
        if let Meta::NameValue(nv) = &attr.meta
            && nv.path.is_ident("doc")
        {
            match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(doc), ..
                }) => {
                    let doc = doc.value();
                    let doc_str = doc.trim();
                    combined_docs_literal += "\n";
                    combined_docs_literal += doc_str;
                }
                Expr::Macro(include_macro) => {
                    if !combined_docs_literal.is_empty() {
                        combined_docs_literal += "\n";
                        let lit = LitStr::new(&combined_docs_literal, Span::call_site());
                        full_docs.push(quote!( #lit ));
                        combined_docs_literal.clear();
                    }
                    full_docs.push(quote!( #include_macro ));
                }
                _ => (),
            }
        }
    }

    if !combined_docs_literal.is_empty() {
        let lit = LitStr::new(&combined_docs_literal, Span::call_site());
        full_docs.push(quote!( #lit ));
        combined_docs_literal.clear();
    }

    Ok(if full_docs.is_empty() {
        None
    } else {
        Some(quote!(::core::primitive::str::trim(
            ::std::concat!( #( #full_docs ),* )
        )))
    })
}

fn generate_default_value(lit: &Lit) -> GeneratorResult<TokenStream> {
    match lit {
        Lit::Str(value) =>{
            let value = value.value();
            Ok(quote!({ ::std::borrow::ToOwned::to_owned(#value) }))
        }
        Lit::Int(value) => {
            let value = value.base10_parse::<i32>()?;
            Ok(quote!({ ::std::convert::TryInto::try_into(#value).unwrap_or_default() }))
        }
        Lit::Float(value) => {
            let value = value.base10_parse::<f64>()?;
            Ok(quote!({ ::std::convert::TryInto::try_into(#value).unwrap_or_default() }))
        }
        Lit::Bool(value) => {
            let value = value.value;
            Ok(quote!({ #value }))
        }
        _ => Err(Error::new_spanned(
            lit,
            "The default value type only be string, integer, float and boolean, other types should use default_with",
        ).into()),
    }
}

fn generate_default_with(lit: &LitStr) -> GeneratorResult<TokenStream> {
    let str = lit.value();
    let tokens: TokenStream = str
        .parse()
        .map_err(|err| GeneratorError::Syn(syn::Error::from(err)))?;
    Ok(quote! { (#tokens) })
}

pub fn generate_default(
    default: &Option<args::DefaultValue>,
    default_with: &Option<LitStr>,
) -> GeneratorResult<Option<TokenStream>> {
    match (default, default_with) {
        (Some(args::DefaultValue::Default), _) => {
            Ok(Some(quote! { ::std::default::Default::default() }))
        }
        (Some(args::DefaultValue::Value(lit)), _) => Ok(Some(generate_default_value(lit)?)),
        (None, Some(lit)) => Ok(Some(generate_default_with(lit)?)),
        (None, None) => Ok(None),
    }
}

pub fn get_cfg_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|attr| !attr.path().segments.is_empty() && attr.path().segments[0].ident == "cfg")
        .cloned()
        .collect()
}

pub fn parse_graphql_attrs<T: FromMeta + Default>(
    attrs: &[Attribute],
) -> GeneratorResult<Option<T>> {
    for attr in attrs {
        if attr.path().is_ident("graphql") {
            return Ok(Some(T::from_meta(&attr.meta)?));
        }
    }
    Ok(None)
}

pub fn remove_graphql_attrs(attrs: &mut Vec<Attribute>) {
    if let Some((idx, _)) = attrs
        .iter()
        .enumerate()
        .find(|(_, a)| a.path().is_ident("graphql"))
    {
        attrs.remove(idx);
    }
}

pub fn get_type_path_and_name(ty: &Type) -> GeneratorResult<(&Type, String)> {
    match ty {
        Type::Path(path) => Ok((
            ty,
            path.path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap(),
        )),
        Type::Group(TypeGroup { elem, .. }) => get_type_path_and_name(elem),
        Type::TraitObject(trait_object) => Ok((
            ty,
            trait_object
                .bounds
                .iter()
                .find_map(|bound| match bound {
                    TypeParamBound::Trait(t) => {
                        Some(t.path.segments.last().map(|s| s.ident.to_string()).unwrap())
                    }
                    _ => None,
                })
                .unwrap(),
        )),
        _ => Err(Error::new_spanned(ty, "Invalid type").into()),
    }
}

pub fn visible_fn(visible: &Option<Visible>) -> TokenStream {
    match visible {
        None | Some(Visible::None) => quote! { ::std::option::Option::None },
        Some(Visible::HiddenAlways) => quote! { ::std::option::Option::Some(|_| false) },
        Some(Visible::FnName(name)) => {
            quote! { ::std::option::Option::Some(#name) }
        }
    }
}

pub fn parse_complexity_expr(expr: Expr) -> GeneratorResult<(HashSet<String>, Expr)> {
    #[derive(Default)]
    struct VisitComplexityExpr {
        variables: HashSet<String>,
    }

    impl<'a> Visit<'a> for VisitComplexityExpr {
        fn visit_expr_path(&mut self, i: &'a ExprPath) {
            if let Some(ident) = i.path.get_ident()
                && ident != "child_complexity"
            {
                self.variables.insert(ident.to_string());
            }
        }
    }

    let mut visit = VisitComplexityExpr::default();
    visit.visit_expr(&expr);
    Ok((visit.variables, expr))
}

pub fn gen_deprecation(deprecation: &Deprecation, crate_name: &syn::Path) -> TokenStream {
    match deprecation {
        Deprecation::NoDeprecated => {
            quote! { #crate_name::registry::Deprecation::NoDeprecated }
        }
        Deprecation::Deprecated {
            reason: Some(reason),
        } => {
            quote! { #crate_name::registry::Deprecation::Deprecated { reason: ::std::option::Option::Some(::std::string::ToString::to_string(#reason)) } }
        }
        Deprecation::Deprecated { reason: None } => {
            quote! { #crate_name::registry::Deprecation::Deprecated { reason: ::std::option::Option::None } }
        }
    }
}

pub fn extract_input_args<T: FromMeta + Default>(
    crate_name: &syn::Path,
    method: &mut ImplItemFn,
) -> GeneratorResult<Vec<(PatIdent, Type, T)>> {
    let mut args = Vec::new();
    let mut create_ctx = true;

    if method.sig.inputs.is_empty() {
        return Err(Error::new_spanned(
            &method.sig,
            "The self receiver must be the first parameter.",
        )
        .into());
    }

    for (idx, arg) in method.sig.inputs.iter_mut().enumerate() {
        if let FnArg::Receiver(receiver) = arg {
            if idx != 0 {
                return Err(Error::new_spanned(
                    receiver,
                    "The self receiver must be the first parameter.",
                )
                .into());
            }
        } else if let FnArg::Typed(pat) = arg {
            if idx == 0 {
                return Err(Error::new_spanned(
                    pat,
                    "The self receiver must be the first parameter.",
                )
                .into());
            }

            match (&*pat.pat, &*pat.ty) {
                (Pat::Ident(arg_ident), Type::Reference(TypeReference { elem, .. })) => {
                    if let Type::Path(path) = elem.as_ref() {
                        if idx != 1 || path.path.segments.last().unwrap().ident != "Context" {
                            args.push((
                                arg_ident.clone(),
                                pat.ty.as_ref().clone(),
                                parse_graphql_attrs::<T>(&pat.attrs)?.unwrap_or_default(),
                            ));
                        } else {
                            create_ctx = false;
                        }
                    }
                }
                (Pat::Ident(arg_ident), ty) => {
                    args.push((
                        arg_ident.clone(),
                        ty.clone(),
                        parse_graphql_attrs::<T>(&pat.attrs)?.unwrap_or_default(),
                    ));
                    remove_graphql_attrs(&mut pat.attrs);
                }
                _ => {
                    return Err(Error::new_spanned(arg, "Invalid argument type.").into());
                }
            }
        }
    }

    if create_ctx {
        let arg = syn::parse2::<FnArg>(quote! { _: &#crate_name::Context<'_> }).unwrap();
        method.sig.inputs.insert(1, arg);
    }

    Ok(args)
}

pub struct RemoveLifetime;

impl VisitMut for RemoveLifetime {
    fn visit_lifetime_mut(&mut self, i: &mut Lifetime) {
        i.ident = Ident::new("_", Span::call_site());
        visit_mut::visit_lifetime_mut(self, i);
    }
}

pub fn gen_directive_calls(
    crate_name: &syn::Path,
    directive_calls: &[Expr],
    location: TypeDirectiveLocation,
) -> Vec<TokenStream> {
    directive_calls
        .iter()
        .map(|directive| {
            let directive_path = extract_directive_call_path(directive).expect(
                "Directive invocation expression format must be [<directive_path>::]<directive_name>::apply(<args>)",
            );
            let identifier = location.location_trait_identifier();
            quote!({
                <#directive_path as #crate_name::registry::location_traits::#identifier>::check();
                <#directive_path as #crate_name::TypeDirective>::register(&#directive_path, registry);
                #directive
            })
        })
        .collect::<Vec<_>>()
}

fn extract_directive_call_path(directive: &Expr) -> Option<syn::Path> {
    if let Expr::Call(expr) = directive
        && let Expr::Path(ref expr) = *expr.func
    {
        let mut path = expr.path.clone();
        if path.segments.pop()?.value().ident != "apply" {
            return None;
        }

        path.segments.pop_punct()?;

        return Some(path);
    }

    None
}

pub fn gen_boxed_trait(crate_name: &syn::Path) -> TokenStream {
    if cfg!(feature = "boxed-trait") {
        quote! {
            #[#crate_name::async_trait::async_trait]
        }
    } else {
        quote! {}
    }
}

/// Generates the expression that resolves a field's value with the field's
/// selection set. It calls `OutputType::resolve` directly rather than a
/// generic library helper, so no helper is instantiated once per field or
/// nested once per type along a chain of object types.
///
/// `set_error_path` stamps resolution errors with this field's path, as
/// `resolve_simple_field_value` does for non-async fields.
pub fn gen_resolve_field_value(
    crate_name: &syn::Path,
    value: TokenStream,
    set_error_path: bool,
) -> TokenStream {
    let map_err = set_error_path.then(|| quote!(.map_err(|err| ctx.set_error_path(err))));
    quote! {
        #crate_name::OutputType::resolve(
            #value,
            &ctx.with_selection_set(&ctx.item.node.selection_set),
            ctx.item,
        )
        .await
        .map(::std::option::Option::Some)
        #map_err
    }
}

/// Generates an `ErasedContainer` impl for `&Self` and the `OutputType::resolve`
/// body that resolves through it.
///
/// Because the impl is generated per type, resolving one object type and then
/// the next one goes through no generic library code. rustc counts the
/// recursion depth of each generic function separately, so a generic step
/// shared by every type would otherwise nest once per type along a chain of
/// object types and hit the recursion limit.
pub fn gen_erased_container(
    crate_name: &syn::Path,
    self_ty: &TokenStream,
    generics: &syn::Generics,
    serial: bool,
) -> (TokenStream, TokenStream) {
    let mut generics = generics.clone();
    generics.params.insert(0, parse_quote!('__erased));
    let (impl_generics, _, where_clause) = generics.split_for_impl();
    let erased_impl = quote! {
        #[doc(hidden)]
        #[allow(clippy::all, clippy::pedantic)]
        impl #impl_generics #crate_name::resolver_utils::ErasedContainer<'__erased> for &'__erased #self_ty #where_clause {
            fn erased_resolve_field(
                &self,
                ctx: #crate_name::Context<'__erased>,
                hooks: bool,
            ) -> #crate_name::resolver_utils::BoxFieldResult<'__erased> {
                let root = *self;
                if hooks {
                    // Polls the resolver through a future type local to this impl:
                    // `Pin<&mut F>`'s own `Future` impl is one generic shared by every
                    // type, and would nest once per type like any other.
                    struct HookedField<'f, F>(::std::pin::Pin<&'f mut F>);

                    impl<F: ::std::future::Future> ::std::future::Future for HookedField<'_, F> {
                        type Output = F::Output;

                        fn poll(
                            mut self: ::std::pin::Pin<&mut Self>,
                            cx: &mut ::std::task::Context<'_>,
                        ) -> ::std::task::Poll<Self::Output> {
                            self.0.as_mut().poll(cx)
                        }
                    }

                    ::std::boxed::Box::pin(async move {
                        let resolve_fut = #crate_name::resolver_utils::ContainerType::resolve_field(root, &ctx);
                        let mut resolve_fut = HookedField(::std::pin::pin!(resolve_fut));
                        #crate_name::resolver_utils::resolve_field_with_hooks(
                            &ctx,
                            <#self_ty as #crate_name::OutputType>::type_name(),
                            &mut resolve_fut,
                        )
                        .await
                    })
                } else {
                    ::std::boxed::Box::pin(async move {
                        #crate_name::resolver_utils::ContainerType::resolve_field(root, &ctx).await
                    })
                }
            }

            fn erased_type_name(&self) -> ::std::borrow::Cow<'static, ::std::primitive::str> {
                <#self_ty as #crate_name::OutputType>::type_name()
            }

            fn erased_introspection_type_name(&self) -> ::std::borrow::Cow<'static, ::std::primitive::str> {
                #crate_name::OutputType::introspection_type_name(*self)
            }

            fn erased_collect_all_fields(
                &self,
                ctx: &#crate_name::ContextSelectionSet<'__erased>,
                fields: &mut #crate_name::resolver_utils::Fields<'__erased>,
            ) -> #crate_name::ServerResult<()> {
                #crate_name::resolver_utils::ContainerType::collect_all_fields(*self, ctx, fields)
            }
        }
    };
    let parallel = !serial;
    let resolve = quote! {
        #crate_name::resolver_utils::resolve_container_dyn(ctx, &self, #parallel).await
    };
    (erased_impl, resolve)
}

/// Whether `ty` is a trait object such as `dyn Trait`, looking through
/// parentheses and invisible groups.
pub fn is_trait_object(ty: &Type) -> bool {
    match ty {
        Type::TraitObject(_) => true,
        Type::Paren(paren) => is_trait_object(&paren.elem),
        Type::Group(group) => is_trait_object(&group.elem),
        _ => false,
    }
}
