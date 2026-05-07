//! Codegen for Move structs and enums, including their `MoveType` impls.

use anyhow::Result;
use move_binary_format::normalized::{Enum, Field, Module, Struct};
use move_core_types::identifier::Identifier;
use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};

use crate::codegen::ty::{rust_type, type_param_is_used, TypeCtx};

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
        field_tokens.extend(quote! { pub #fname: #fty, });
    }
    // Any unused param needs PhantomData to keep Rust happy.
    for (i, &phantom) in phantoms.iter().enumerate() {
        if phantom || !used_in_fields(i as u16) {
            let pf = format_ident!("_phantom_t{}", i);
            let tn = format_ident!("T{}", i);
            field_tokens.extend(quote! {
                #[serde(skip)]
                pub #pf: ::std::marker::PhantomData<#tn>,
            });
        }
    }

    let move_type = move_type_impl(&s.name, module, &g);

    let decl = &g.decl;
    Ok(quote! {
        #[derive(::std::clone::Clone, ::std::fmt::Debug, ::serde::Serialize, ::serde::Deserialize)]
        pub struct #name #decl {
            #field_tokens
        }
        #move_type
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
            let tys: Vec<TokenStream> = v
                .fields
                .iter()
                .map(|f| rust_type(&f.type_, ctx))
                .collect::<Result<_>>()?;
            variants.extend(quote! { #vname( #( #tys ),* ), });
        } else {
            let mut named = TokenStream::new();
            for f in &v.fields {
                let fname = field_ident(f.name.as_str());
                let fty = rust_type(&f.type_, ctx)?;
                named.extend(quote! { #fname: #fty, });
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
                #vn(::std::marker::PhantomData<#tn>),
            });
        }
    }

    let move_type = move_type_impl(&e.name, module, &g);
    let decl = &g.decl;
    Ok(quote! {
        #[derive(::std::clone::Clone, ::std::fmt::Debug, ::serde::Serialize, ::serde::Deserialize)]
        pub enum #name #decl {
            #variants
        }
        #move_type
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
    let bounded = names.iter().map(|n| {
        quote! {
            #n: ::move_bindgen_runtime::MoveType
        }
    });
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
        quote!(::std::vec::Vec::new())
    } else {
        let parts = g
            .names
            .iter()
            .map(|n| quote! { <#n as ::move_bindgen_runtime::MoveType>::type_tag() });
        quote!(::std::vec![ #( #parts ),* ])
    };

    let decl = &g.decl;
    let args = &g.args;
    quote! {
        impl #decl ::move_bindgen_runtime::MoveType for #name #args {
            fn type_tag() -> ::move_bindgen_runtime::TypeTag {
                ::move_bindgen_runtime::TypeTag::Struct(::std::boxed::Box::new(
                    ::move_bindgen_runtime::StructTag {
                        address: super::PACKAGE_ID,
                        module: ::move_core_types::identifier::Identifier::new(#module_name)
                            .expect("static module name is a valid Move identifier"),
                        name: ::move_core_types::identifier::Identifier::new(#type_name_s)
                            .expect("static type name is a valid Move identifier"),
                        type_params: #type_params,
                    }
                ))
            }
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
