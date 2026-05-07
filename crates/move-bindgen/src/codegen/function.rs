//! Codegen for per-function PTB call builders.
//!
//! Emits `pub fn <name>(b: &mut PtbBuilder, …args) -> Argument` for each
//! externally-callable function (public, public-entry, private-entry).
//! `&mut TxContext` is dropped from the signature — the Move VM injects it.

use anyhow::{bail, Result};
use move_binary_format::{
    file_format::Visibility,
    normalized::{Datatype, Function, Module, Type},
};
use move_core_types::identifier::Identifier;
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};

use crate::codegen::ty::{rust_type, TypeCtx, IOTA_ADDRESS, STD_ADDRESS};

/// Emit call builders for every externally-callable function in `module`.
pub fn emit_functions(module: &Module<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    let mut out = TokenStream::new();
    for (name, f) in &module.functions {
        if !is_externally_callable(f) {
            continue;
        }
        out.extend(emit_function(name, f, module, ctx)?);
    }
    Ok(out)
}

/// True if the function can be called from off-chain (i.e. as the top-level
/// `MoveCall` of a PTB).
fn is_externally_callable(f: &Function<Identifier>) -> bool {
    match f.visibility {
        Visibility::Public => true,
        Visibility::Friend => false, // package-internal in Move 2024; not callable externally
        Visibility::Private => f.is_entry, // entry-only fns are off-chain callable
    }
}

fn emit_function(
    name: &Identifier,
    f: &Function<Identifier>,
    module: &Module<Identifier>,
    ctx: &TypeCtx,
) -> Result<TokenStream> {
    // 1. Type parameters → Rust generics with `T_i: MoveType`.
    let n_type_params = f.type_parameters.len();
    let type_param_idents: Vec<Ident> = (0..n_type_params).map(|i| format_ident!("T{i}")).collect();
    let generics_decl = if n_type_params == 0 {
        TokenStream::new()
    } else {
        let parts = type_param_idents.iter().map(|n| quote! { #n: MoveType });
        quote!(< #( #parts ),* >)
    };

    // 2. Type-arguments slot of the MoveCall — built from `T_i::type_tag()`.
    let type_tags_expr = if n_type_params == 0 {
        quote!(Vec::new())
    } else {
        let parts = type_param_idents
            .iter()
            .map(|n| quote! { <#n as MoveType>::type_tag() });
        quote!(vec![ #( #parts ),* ])
    };

    // 3. Value parameters — drop TxContext, then generate one Rust param +
    //    `into_argument` site per remaining Move param.
    let mut params_decl = TokenStream::new();
    let mut arg_exprs: Vec<TokenStream> = Vec::new();
    let mut arg_idx = 0usize;
    for p in f.parameters.iter() {
        if is_tx_context(p) {
            continue;
        }
        let bound = param_bound(p, ctx)?;
        let pname = format_ident!("arg{arg_idx}");
        let aname = format_ident!("a{arg_idx}");
        params_decl.extend(quote! { , #pname: #bound });
        arg_exprs.push(quote! {
            let #aname = #pname.into_argument(b);
        });
        arg_idx += 1;
    }
    let arg_idents: Vec<Ident> = (0..arg_idx).map(|i| format_ident!("a{i}")).collect();

    let module_name = module.id.name.as_str();
    let fn_name = name.as_str();
    let fn_ident = safe_ident(name.as_str());

    Ok(quote! {
        pub fn #fn_ident #generics_decl (
            b: &mut PtbBuilder
            #params_decl
        ) -> Argument {
            #( #arg_exprs )*
            b.move_call(
                super::PACKAGE_ID,
                #module_name,
                #fn_name,
                #type_tags_expr,
                vec![ #( #arg_idents ),* ],
            )
        }
    })
}

/// Map a Move parameter type to its Rust trait bound (`impl SomeTrait`).
///
/// For references (`&T`/`&mut T`), the bound is determined by the inner type
/// — references are a Move-side concept that don't show up in the Rust API.
fn param_bound(ty: &Type<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    let ty = match ty {
        Type::Reference(_, inner) => inner.as_ref(),
        other => other,
    };
    match ty {
        Type::Bool => Ok(quote!(impl PureBool)),
        Type::U8 => Ok(quote!(impl PureU8)),
        Type::U16 => Ok(quote!(impl PureU16)),
        Type::U32 => Ok(quote!(impl PureU32)),
        Type::U64 => Ok(quote!(impl PureU64)),
        Type::U128 => Ok(quote!(impl PureU128)),
        Type::U256 => bail!("u256 parameters not yet supported"),
        Type::Address => Ok(quote!(impl PureAddress)),
        Type::Signer => bail!("`signer` is not supported on IOTA"),
        Type::Vector(inner) => {
            let inner_rust = rust_type(inner, ctx)?;
            Ok(quote!(impl PureVec<#inner_rust>))
        }
        Type::TypeParameter(_) => {
            // Generic value param — we don't know whether it's a value or
            // object, so fall back to the SDK's permissive `PTBArgument`.
            // Loses compile-time per-type safety here; revisit.
            Ok(quote!(impl PTBArgument))
        }
        Type::Reference(_, _) => unreachable!("already unwrapped above"),
        Type::Datatype(dt) => datatype_bound(dt, ctx),
    }
}

/// Bound for a `Datatype` in parameter position.
fn datatype_bound(dt: &Datatype<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    let module_addr = dt.module.address;
    let module_name = dt.module.name.as_str();
    let type_name = dt.name.as_str();

    // Well-known stdlib types map to runtime Pure* traits.
    if module_addr == STD_ADDRESS {
        match (module_name, type_name) {
            ("option", "Option") => {
                let inner = dt
                    .type_arguments
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("Option without type argument"))?;
                let inner_rust = rust_type(inner, ctx)?;
                return Ok(quote!(impl PureOption<#inner_rust>));
            }
            ("string", "String") | ("ascii", "String") => {
                return Ok(quote!(impl PureString));
            }
            _ => {}
        }
    }
    // Well-known iota framework types: ID/UID would land here. We don't
    // currently emit `ArgumentID`/`ArgumentUID` traits, so error out for
    // now — these rarely appear as direct call params anyway.
    if module_addr == IOTA_ADDRESS && module_name == "object" {
        bail!("iota::object::{type_name} is not yet supported as a call-builder parameter");
    }

    // Same-package datatype — use the codegen'd ArgumentX trait.
    if module_addr == ctx.package_addr {
        let trait_ident = format_ident!("Argument{type_name}");
        if dt.type_arguments.is_empty() {
            return if dt.module.name == *ctx.current_module {
                Ok(quote!(impl #trait_ident))
            } else {
                let mod_ident = format_ident!("{module_name}");
                Ok(quote!(impl super::#mod_ident::#trait_ident))
            };
        }
        let args: Vec<TokenStream> = dt
            .type_arguments
            .iter()
            .map(|t| rust_type(t, ctx))
            .collect::<Result<_>>()?;
        return if dt.module.name == *ctx.current_module {
            Ok(quote!(impl #trait_ident < #( #args ),* >))
        } else {
            let mod_ident = format_ident!("{module_name}");
            Ok(quote!(impl super::#mod_ident::#trait_ident < #( #args ),* >))
        };
    }

    bail!(
        "external dependency types are not yet supported as call-builder parameters: {}::{}::{}",
        module_addr.short_str_lossless(),
        module_name,
        type_name
    )
}

fn is_tx_context(ty: &Type<Identifier>) -> bool {
    let inner = match ty {
        Type::Reference(_, inner) => inner.as_ref(),
        other => other,
    };
    if let Type::Datatype(dt) = inner {
        return dt.module.address == IOTA_ADDRESS
            && dt.module.name.as_str() == "tx_context"
            && dt.name.as_str() == "TxContext";
    }
    false
}

fn safe_ident(s: &str) -> Ident {
    if is_rust_keyword(s) {
        Ident::new_raw(s, Span::call_site())
    } else {
        Ident::new(s, Span::call_site())
    }
}

fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "async"
            | "await"
            | "box"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "do"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "try"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "yield"
    )
}
