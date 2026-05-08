//! Codegen for Move structs and enums: the Rust type, its `MoveType` impl,
//! and the per-datatype `ArgumentX` marker trait used by call builders.

use anyhow::Result;
use move_binary_format::{
    file_format::AbilitySet,
    normalized::{Enum, Field, Module, Struct, Type},
};
use move_core_types::identifier::Identifier;
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};

use crate::codegen::ty::{rust_type, type_param_is_used, TypeCtx};

/// Returns a `#[serde(with = "...")]` attribute for fields whose Move type
/// has no native serde-compatible Rust counterpart and needs a custom (de)
/// serialiser. Currently only `u256` (the `primitive_types::U256` re-export)
/// — its built-in `impl-serde` uses hex strings, which doesn't match Move's
/// 32-LE-bytes BCS encoding.
fn field_serde_attr(ty: &Type<Identifier>) -> TokenStream {
    match ty {
        Type::U256 => quote!(#[serde(with = "move_bindgen_runtime::u256_le")]),
        _ => TokenStream::new(),
    }
}

/// Emit all datatypes (structs + enums) defined in `module`.
pub fn emit_datatypes(module: &Module<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    let mut out = TokenStream::new();
    for s in module.structs.values() {
        out.extend(emit_struct(s, module, ctx)?);
    }
    for e in module.enums.values() {
        out.extend(emit_enum(e, module, ctx)?);
    }
    Ok(out)
}

// -----------------------------------------------------------------------------
// Struct
// -----------------------------------------------------------------------------

fn emit_struct(
    s: &Struct<Identifier>,
    module: &Module<Identifier>,
    ctx: &TypeCtx,
) -> Result<TokenStream> {
    let name = ident(s.name.as_str());
    let phantoms: Vec<bool> = s.type_parameters.iter().map(|p| p.is_phantom).collect();

    let used_in_fields = |i: u16| s.fields.iter().any(|f| type_param_is_used(&f.type_, i));
    let g = generics(&phantoms);

    let mut field_tokens = TokenStream::new();
    for f in &s.fields {
        let fname = field_ident(f.name.as_str());
        let fty = rust_type(&f.type_, ctx)?;
        let attr = field_serde_attr(&f.type_);
        field_tokens.extend(quote! { #attr pub #fname: #fty, });
    }
    // Any unused param needs PhantomData to keep Rust happy.
    for (i, &phantom) in phantoms.iter().enumerate() {
        if phantom || !used_in_fields(i as u16) {
            let pf = format_ident!("_phantom_t{}", i);
            let tn = format_ident!("T{}", i);
            field_tokens.extend(quote! {
                #[serde(skip)]
                pub #pf: PhantomData<#tn>,
            });
        }
    }

    let move_type = move_type_impl(&s.name, module, &g);
    let arg_trait = argument_trait(&s.name, &g, s.abilities);

    let decl = &g.decl;
    Ok(quote! {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        pub struct #name #decl {
            #field_tokens
        }
        #move_type
        #arg_trait
    })
}

// -----------------------------------------------------------------------------
// Enum
// -----------------------------------------------------------------------------

fn emit_enum(
    e: &Enum<Identifier>,
    module: &Module<Identifier>,
    ctx: &TypeCtx,
) -> Result<TokenStream> {
    let name = ident(e.name.as_str());
    let phantoms: Vec<bool> = e.type_parameters.iter().map(|p| p.is_phantom).collect();
    let g = generics(&phantoms);

    let used_anywhere = |i: u16| {
        e.variants
            .iter()
            .any(|v| v.fields.iter().any(|f| type_param_is_used(&f.type_, i)))
    };

    let mut variants = TokenStream::new();
    for v in &e.variants {
        let vname = ident(v.name.as_str());
        if v.fields.is_empty() {
            variants.extend(quote! { #vname, });
        } else if is_positional(&v.fields) {
            let parts: Vec<TokenStream> = v
                .fields
                .iter()
                .map(|f| {
                    let attr = field_serde_attr(&f.type_);
                    let ty = rust_type(&f.type_, ctx)?;
                    Ok::<_, anyhow::Error>(quote!(#attr #ty))
                })
                .collect::<Result<_>>()?;
            variants.extend(quote! { #vname( #( #parts ),* ), });
        } else {
            let mut named = TokenStream::new();
            for f in &v.fields {
                let fname = field_ident(f.name.as_str());
                let fty = rust_type(&f.type_, ctx)?;
                let attr = field_serde_attr(&f.type_);
                named.extend(quote! { #attr #fname: #fty, });
            }
            variants.extend(quote! { #vname { #named }, });
        }
    }
    // Phantom / unused params: synthesize a hidden variant carrying PhantomData<T>.
    for (i, &phantom) in phantoms.iter().enumerate() {
        if phantom || !used_anywhere(i as u16) {
            let vn = format_ident!("_Phantom{}", i);
            let tn = format_ident!("T{}", i);
            variants.extend(quote! {
                #[serde(skip)]
                #vn(PhantomData<#tn>),
            });
        }
    }

    let move_type = move_type_impl(&e.name, module, &g);
    let arg_trait = argument_trait(&e.name, &g, e.abilities);
    let decl = &g.decl;
    Ok(quote! {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        pub enum #name #decl {
            #variants
        }
        #move_type
        #arg_trait
    })
}

// -----------------------------------------------------------------------------
// Generics + MoveType impl
// -----------------------------------------------------------------------------

struct Generics {
    /// `<T0: Bounds, T1: Bounds>` for type/impl declarations.
    decl: TokenStream,
    /// `<T0, T1>` for path positions.
    args: TokenStream,
    /// Bare `[T0, T1]` (no angle brackets) — used to expand into impls.
    names: Vec<Ident>,
}

fn generics(phantoms: &[bool]) -> Generics {
    if phantoms.is_empty() {
        return Generics {
            decl: TokenStream::new(),
            args: TokenStream::new(),
            names: Vec::new(),
        };
    }
    let names: Vec<Ident> = (0..phantoms.len())
        .map(|i| format_ident!("T{}", i))
        .collect();
    let bounded = names.iter().map(|n| quote! { #n: MoveType });
    Generics {
        decl: quote!(< #( #bounded ),* >),
        args: quote!(< #( #names ),* >),
        names,
    }
}

fn move_type_impl(
    type_name: &Identifier,
    module: &Module<Identifier>,
    g: &Generics,
) -> TokenStream {
    let name = ident(type_name.as_str());
    let module_name = module.id.name.as_str();
    let type_name_s = type_name.as_str();

    let type_params = if g.names.is_empty() {
        quote!(Vec::new())
    } else {
        let parts = g
            .names
            .iter()
            .map(|n| quote! { <#n as MoveType>::type_tag() });
        quote!(vec![ #( #parts ),* ])
    };

    let decl = &g.decl;
    let args = &g.args;
    quote! {
        impl #decl MoveType for #name #args {
            fn type_tag() -> TypeTag {
                make_struct_tag(super::PACKAGE_ID, #module_name, #type_name_s, #type_params)
            }
        }
    }
}

// -----------------------------------------------------------------------------
// ArgumentX marker trait
// -----------------------------------------------------------------------------

/// Emit the per-datatype marker trait + impls.
///
/// - `key` types are objects: closed impls on `Argument`, `ObjectId` (cache-aware
///   override), `ObjectReference`, `Shared<ObjectId>`, `SharedMut<ObjectId>`,
///   `Receiving<ObjectId>`.
/// - non-`key` types are values: emit `MoveArg` (BCS via `bcs::to_bytes`) so
///   the SDK's blanket gives us `PTBArgument`, plus closed impls on `Argument`
///   and the type itself.
fn argument_trait(type_name: &Identifier, g: &Generics, abilities: AbilitySet) -> TokenStream {
    let name = ident(type_name.as_str());
    let trait_name = format_ident!("Argument{}", type_name.as_str());

    let decl = &g.decl; // <T0: MoveType, …>
    let args = &g.args; // <T0, …>

    if abilities.has_key() {
        // Object trait. The default `into_argument` body delegates to the
        // SDK; the `ObjectId` impl overrides it for cache-aware resolution
        // (with optional Fetcher fallback for unknown ids).
        quote! {
            pub trait #trait_name #decl: PTBArgument {
                #[allow(async_fn_in_trait)]
                async fn into_argument(self, b: &mut PtbBuilder) -> Argument
                where Self: Sized,
                {
                    b.inner.apply_argument(self)
                }
            }
            impl #decl #trait_name #args for Argument {}
            impl #decl #trait_name #args for ObjectId {
                async fn into_argument(self, b: &mut PtbBuilder) -> Argument {
                    b.resolve_object(self).await
                }
            }
            impl #decl #trait_name #args for ObjectReference {}
            impl #decl #trait_name #args for Shared<ObjectId> {}
            impl #decl #trait_name #args for SharedMut<ObjectId> {}
            impl #decl #trait_name #args for Receiving<ObjectId> {}
        }
    } else {
        // Value trait. `MoveArg` impl gives BCS-Pure encoding; PTBArgument is
        // then auto-impl'd via the SDK's blanket `impl<T: MoveArg> PTBArgument for T`.
        let move_arg_decl = if g.names.is_empty() {
            quote!()
        } else {
            // For generic value types we need a Serialize bound on each T_i so
            // bcs::to_bytes(&self) compiles.
            let parts = g
                .names
                .iter()
                .map(|n| quote! { #n: MoveType + ::serde::Serialize });
            quote!(< #( #parts ),* >)
        };
        let serialize_bound = if g.names.is_empty() {
            quote!()
        } else {
            quote!(where Self: ::serde::Serialize)
        };
        let _ = serialize_bound;

        quote! {
            impl #move_arg_decl MoveArg for #name #args {
                fn pure_bytes(self) -> PureBytes {
                    PureBytes(::bcs::to_bytes(&self).expect("bcs serialization failed"))
                }
            }

            pub trait #trait_name #decl: PTBArgument {
                #[allow(async_fn_in_trait)]
                async fn into_argument(self, b: &mut PtbBuilder) -> Argument
                where Self: Sized,
                {
                    b.inner.apply_argument(self)
                }
            }
            impl #move_arg_decl #trait_name #args for #name #args {}
            impl #decl #trait_name #args for Argument {}
        }
    }
}

// -----------------------------------------------------------------------------
// Misc
// -----------------------------------------------------------------------------

fn is_positional(fields: &[Field<Identifier>]) -> bool {
    !fields.is_empty()
        && fields
            .iter()
            .enumerate()
            .all(|(i, f)| f.name.as_str() == format!("pos{i}"))
}

fn ident(s: &str) -> Ident {
    Ident::new(s, Span::call_site())
}

fn field_ident(s: &str) -> Ident {
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
