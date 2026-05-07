//! Map a `normalized::Type<Identifier>` to the Rust `TokenStream` that
//! refers to it from generated code.

use anyhow::{bail, Result};
use move_binary_format::normalized::{Datatype, Type};
use move_core_types::{account_address::AccountAddress, identifier::Identifier};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// IOTA framework address (`0x2`).
pub const IOTA_ADDRESS: AccountAddress = {
    let mut bytes = [0u8; AccountAddress::LENGTH];
    bytes[AccountAddress::LENGTH - 1] = 0x02;
    AccountAddress::new(bytes)
};

/// Move stdlib address (`0x1`).
pub const STD_ADDRESS: AccountAddress = {
    let mut bytes = [0u8; AccountAddress::LENGTH];
    bytes[AccountAddress::LENGTH - 1] = 0x01;
    AccountAddress::new(bytes)
};

/// Resolution context: every Move type reference is interpreted relative to
/// "where I'm being emitted from".
pub struct TypeCtx<'a> {
    /// Address the package being generated lives at. Datatypes at this
    /// address are *internal* and emitted as `super::module::Name`.
    pub package_addr: AccountAddress,
    /// Module currently being emitted (so we can collapse `super::self::X`
    /// to bare `X`).
    pub current_module: &'a Identifier,
}

/// Resolve a Move type to the Rust tokens for the type position.
///
/// Generated modules `use move_bindgen_runtime::*;` and `use serde::{...};` at
/// the top, so we emit short names here (`UID`, `Vec<T>`, …) rather than fully
/// qualified paths.
pub fn rust_type(ty: &Type<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    Ok(match ty {
        Type::Bool => quote!(bool),
        Type::U8 => quote!(u8),
        Type::U16 => quote!(u16),
        Type::U32 => quote!(u32),
        Type::U64 => quote!(u64),
        Type::U128 => quote!(u128),
        Type::U256 => bail!("u256 not yet supported in generated code"),
        Type::Address => quote!(AccountAddress),
        Type::Signer => bail!("`signer` is not supported on IOTA"),
        Type::Vector(inner) => {
            let inner = rust_type(inner, ctx)?;
            quote!(Vec<#inner>)
        }
        Type::TypeParameter(idx) => {
            let ident = format_ident!("T{}", *idx);
            quote!(#ident)
        }
        Type::Reference(_, _) => {
            bail!("references are not allowed in field types or generated value positions")
        }
        Type::Datatype(dt) => rust_datatype(dt, ctx)?,
    })
}

fn rust_datatype(dt: &Datatype<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    let module_addr = dt.module.address;
    let module_name = dt.module.name.as_str();
    let type_name = dt.name.as_str();
    let args: Vec<TokenStream> = dt
        .type_arguments
        .iter()
        .map(|t| rust_type(t, ctx))
        .collect::<Result<_>>()?;
    let generics = if args.is_empty() {
        quote!()
    } else {
        quote!(< #( #args ),* >)
    };

    // Well-known framework types — short names from the runtime wildcard import.
    if module_addr == IOTA_ADDRESS && module_name == "object" {
        match type_name {
            "UID" => return Ok(quote!(UID)),
            "ID" => return Ok(quote!(ID)),
            _ => {}
        }
    }
    if module_addr == STD_ADDRESS {
        match (module_name, type_name) {
            ("option", "Option") => {
                let arg = args.into_iter().next().unwrap_or_else(|| quote!(()));
                return Ok(quote!(Option<#arg>));
            }
            ("string", "String") | ("ascii", "String") => {
                return Ok(quote!(String));
            }
            _ => {}
        }
    }

    if module_addr == ctx.package_addr {
        let type_ident = format_ident!("{type_name}");
        if dt.module.name == *ctx.current_module {
            return Ok(quote!(#type_ident #generics));
        }
        let mod_ident = format_ident!("{module_name}");
        return Ok(quote!(super::#mod_ident::#type_ident #generics));
    }

    bail!(
        "external dependency types are not yet supported in codegen: {}::{}::{}",
        module_addr.short_str_lossless(),
        module_name,
        type_name
    )
}

/// `T::type_tag()` for a type — used to fill in `MoveCall.type_arguments`.
/// Returns the `TypeTag`-building expression as a token stream.
#[allow(dead_code)] // wired up by the call-codegen phase
pub fn type_tag_expr(ty: &Type<Identifier>, ctx: &TypeCtx) -> Result<TokenStream> {
    Ok(match ty {
        Type::Bool => quote!(TypeTag::Bool),
        Type::U8 => quote!(TypeTag::U8),
        Type::U16 => quote!(TypeTag::U16),
        Type::U32 => quote!(TypeTag::U32),
        Type::U64 => quote!(TypeTag::U64),
        Type::U128 => quote!(TypeTag::U128),
        Type::U256 => bail!("u256 not yet supported"),
        Type::Address => quote!(TypeTag::Address),
        Type::Signer => bail!("`signer` is not supported on IOTA"),
        Type::Vector(inner) => {
            let inner = type_tag_expr(inner, ctx)?;
            quote!(TypeTag::Vector(Box::new(#inner)))
        }
        Type::TypeParameter(idx) => {
            let ident = format_ident!("T{}", *idx);
            quote!(<#ident as MoveType>::type_tag())
        }
        Type::Reference(_, _) => bail!("references have no TypeTag"),
        Type::Datatype(_) => {
            let rust = rust_type(ty, ctx)?;
            quote!(<#rust as MoveType>::type_tag())
        }
    })
}

/// True iff `T<idx>` appears anywhere inside `ty`.
pub fn type_param_is_used(ty: &Type<Identifier>, idx: u16) -> bool {
    match ty {
        Type::TypeParameter(i) => *i == idx,
        Type::Vector(inner) | Type::Reference(_, inner) => type_param_is_used(inner, idx),
        Type::Datatype(dt) => dt.type_arguments.iter().any(|t| type_param_is_used(t, idx)),
        _ => false,
    }
}
