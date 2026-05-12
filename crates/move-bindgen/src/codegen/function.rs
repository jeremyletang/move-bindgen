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
///
/// We skip `Visibility::Friend` deliberately: in bytecode, both Move 2024
/// `public(package)` and the older `public(friend)` lower to `Friend`, and
/// neither is reachable from a transaction submitted by an external caller.
/// Generating a builder for them would compile but always fail at execution.
fn is_externally_callable(f: &Function<Identifier>) -> bool {
    match f.visibility {
        Visibility::Public => true,
        Visibility::Friend => false,
        // `entry` fns are reachable as the entry point of a tx even when
        // their declared visibility is `private`.
        Visibility::Private => f.is_entry,
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
            .map(|n| quote! { <#n as MoveType>::type_tag(b) });
        quote!(vec![ #( #parts ),* ])
    };

    // 3. Value parameters — drop TxContext, then generate one Rust param +
    //    `into_argument*` site per remaining Move param.
    //
    // Reference kind: pick `_ref` / `_mut` only for object-shape params
    // (`&T` / `&mut T` where the inner is a `Datatype` or `TypeParameter`).
    // For primitives (`&u64` etc.) the Move PTB validator wouldn't accept
    // them as inputs anyway, so the bare `into_argument` path is fine.
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
        let into_call = match into_argument_method(p) {
            RefKind::Owned => quote!(into_argument),
            RefKind::Ref => quote!(into_argument_ref),
            RefKind::Mut => quote!(into_argument_mut),
        };
        arg_exprs.push(quote! {
            let #aname = #pname.#into_call(b).await;
        });
        arg_idx += 1;
    }
    let arg_idents: Vec<Ident> = (0..arg_idx).map(|i| format_ident!("a{i}")).collect();

    let module_name = module.id.name.as_str();
    let fn_name = name.as_str();
    let fn_ident = safe_ident(name.as_str());

    // Return shape:
    //   0 returns → fn returns `()`
    //   1 return  → fn returns `Argument`
    //   N returns → fn returns `(Argument; N)` (each a sub-handle into the result)
    let n_returns = f.return_.len();
    let return_types: Vec<String> = f.return_.iter().map(|t| t.to_string()).collect();
    let return_doc = match n_returns {
        0 => String::new(),
        1 => format!(" Returns: `{}`.", return_types[0]),
        _ => format!(" Returns: `({})`.", return_types.join(", ")),
    };

    let (return_arrow, body_tail): (TokenStream, TokenStream) = match n_returns {
        0 => (
            TokenStream::new(),
            quote! {
                b.move_call(
                    b.package_id::<super::Package>(),
                    #module_name,
                    #fn_name,
                    #type_tags_expr,
                    vec![ #( #arg_idents ),* ],
                );
            },
        ),
        1 => (
            quote!(-> Argument),
            quote! {
                b.move_call(
                    b.package_id::<super::Package>(),
                    #module_name,
                    #fn_name,
                    #type_tags_expr,
                    vec![ #( #arg_idents ),* ],
                )
            },
        ),
        n => {
            let arg_repeat = (0..n).map(|_| quote!(Argument));
            let count = n as u16;
            let indices = (0..n).map(syn::Index::from);
            (
                quote!(-> ( #( #arg_repeat ),* )),
                quote! {
                    let __r = b.move_call_n(
                        b.package_id::<super::Package>(),
                        #module_name,
                        #fn_name,
                        #type_tags_expr,
                        vec![ #( #arg_idents ),* ],
                        #count,
                    );
                    ( #( __r[#indices] ),* )
                },
            )
        }
    };

    let source_doc = ctx.docs.item(module_name, fn_name);
    let mut doc_attrs = TokenStream::new();
    if let Some(src) = source_doc {
        doc_attrs.extend(crate::codegen::outer_doc(src));
    }
    if !return_doc.is_empty() {
        if source_doc.is_some() {
            doc_attrs.extend(quote!(#[doc = ""]));
        }
        doc_attrs.extend(quote!(#[doc = #return_doc]));
    }

    Ok(quote! {
        #doc_attrs
        pub async fn #fn_ident #generics_decl (
            b: &mut PtbBuilder
            #params_decl
        ) #return_arrow {
            #( #arg_exprs )*
            #body_tail
        }
    })
}

/// How the Move parameter receives its argument — picks which trait
/// method codegen calls on the user's argument value.
enum RefKind {
    /// By-value (no leading `&` / `&mut`) — generic types or owned object
    /// types. Uses the trait's default `into_argument` body.
    Owned,
    /// `&T` — immutable reference. Routes bare `ObjectId` through
    /// `Shared(_)` for the correct on-chain shared-input lock.
    Ref,
    /// `&mut T` — mutable reference. Routes bare `ObjectId` through
    /// `SharedMut(_)`.
    Mut,
}

/// Pick the [`RefKind`] for a Move parameter. References on
/// non-object/non-generic types (e.g. `&u64`) collapse to `Owned`
/// because their bindings go through `Pure*` traits, which don't
/// expose the `_ref` / `_mut` variants.
fn into_argument_method(ty: &Type<Identifier>) -> RefKind {
    let (is_mut, inner) = match ty {
        Type::Reference(is_mut, inner) => (*is_mut, inner.as_ref()),
        _ => return RefKind::Owned,
    };
    let routes_through_arg_trait = matches!(inner, Type::Datatype(_) | Type::TypeParameter(_),);
    if !routes_through_arg_trait {
        return RefKind::Owned;
    }
    if is_mut {
        RefKind::Mut
    } else {
        RefKind::Ref
    }
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
        Type::U256 => Ok(quote!(impl PureU256)),
        Type::Address => Ok(quote!(impl PureAddress)),
        Type::Signer => bail!("`signer` is not supported on IOTA"),
        Type::Vector(inner) => {
            let inner_rust = rust_type(inner, ctx)?;
            Ok(quote!(impl PureVec<#inner_rust>))
        }
        Type::TypeParameter(_) => {
            // Generic value param — we don't know whether it's a value
            // or object. `ArgumentObject<()>` is permissive (accepts
            // `Argument` / `ObjectId` / `ObjectReference` / `Shared` /
            // `SharedMut` / `Receiving`) and has `into_argument` so the
            // body compiles. Loses per-type safety; revisit.
            Ok(quote!(impl ArgumentObject<()>))
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
    // Hand-mapped iota framework types — `ID` is `copy + drop + store`
    // and routes through the runtime's `PureID` for ergonomics. Anything
    // else under `iota::object` (UID etc.) gets the permissive
    // `PTBArgument` fallback: not directly useful from off-chain (you
    // can't construct a UID externally), but lets generated framework
    // bindings compile so cross-package type resolution works. Other
    // framework types fall through to the peer-map lookup below
    // (`iota_rs::module::ArgumentX`).
    if module_addr == IOTA_ADDRESS && module_name == "object" {
        match type_name {
            "ID" => return Ok(quote!(impl PureID)),
            _ => return Ok(quote!(impl ArgumentObject<()>)),
        }
    }

    // Same-package datatype — use the codegen'd ArgumentX trait.
    if module_addr == ctx.build_addr {
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

    // Cross-package: route through the peer crate's ArgumentX trait.
    if let Some(peer) = ctx.peers.lookup(&module_addr) {
        let crate_ident = format_ident!("{}", peer.crate_name.replace('-', "_"));
        let mod_ident = format_ident!("{module_name}");
        let trait_ident = format_ident!("Argument{type_name}");
        if dt.type_arguments.is_empty() {
            return Ok(quote!(impl ::#crate_ident::#mod_ident::#trait_ident));
        }
        let args: Vec<TokenStream> = dt
            .type_arguments
            .iter()
            .map(|t| rust_type(t, ctx))
            .collect::<Result<_>>()?;
        return Ok(quote!(impl ::#crate_ident::#mod_ident::#trait_ident < #( #args ),* >));
    }

    bail!(
        "type {}::{}::{} is not in the current package, not a known framework type, \
         and not registered as a peer in the config — add it to `[packages.*]`, \
         or to `framework_packages` if its types live in `move-bindgen-runtime`",
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
