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
    /// Build-time address of the package being generated. Set by
    /// `additional_named_addresses` at build, encoded in every
    /// generated module's bytecode. Datatypes at this address are
    /// *internal* and emitted as `super::module::Name`. Not the same
    /// thing as the on-chain `published_at` — that one is resolved at
    /// runtime via `b.package_id::<super::Package>()`.
    pub build_addr: AccountAddress,
    /// Module currently being emitted (so we can collapse `super::self::X`
    /// to bare `X`).
    pub current_module: &'a Identifier,
    /// Source-level doc comments, queried by codegen to attach `#[doc =
    /// "..."]` to generated items.
    pub docs: &'a crate::DocMap,
    /// Peer-package address map. Cross-package datatype refs whose
    /// address is registered here are emitted as `peer_crate::module::Type`.
    /// Empty in single-crate mode (any non-self / non-framework address
    /// then errors at codegen).
    pub peers: &'a crate::PeerMap,
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
        Type::U256 => quote!(U256),
        Type::Address => quote!(Address),
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

    // Hand-mapped iota framework types — small set that the runtime
    // provides directly (UID, ID). Everything else routes via the peer
    // map (i.e. the user lists `Iota` as a peer package and a generated
    // `iota-rs` crate provides the rest). Same for `std::*` below.
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
            // `string::String` and `ascii::String` are wire-identical
            // (both are `vector<u8>` BCS), but the chain checks
            // struct identity at generic-instantiation positions. Map
            // them to distinct Rust types so `MoveType::type_tag`
            // produces the right tag for each.
            ("string", "String") => return Ok(quote!(String)),
            ("ascii", "String") => return Ok(quote!(AsciiString)),
            _ => {}
        }
    }

    if module_addr == ctx.build_addr {
        let type_ident = format_ident!("{type_name}");
        if dt.module.name == *ctx.current_module {
            return Ok(quote!(#type_ident #generics));
        }
        let mod_ident = format_ident!("{module_name}");
        return Ok(quote!(super::#mod_ident::#type_ident #generics));
    }

    // Cross-package: route through the peer crate.
    if let Some(peer) = ctx.peers.lookup(&module_addr) {
        let crate_ident = format_ident!("{}", peer.crate_name.replace('-', "_"));
        let mod_ident = format_ident!("{module_name}");
        let type_ident = format_ident!("{type_name}");
        return Ok(quote!(::#crate_ident::#mod_ident::#type_ident #generics));
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

/// `T::type_tag(b)` for a type — used to fill in `MoveCall.type_arguments`.
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
        Type::U256 => quote!(TypeTag::U256),
        Type::Address => quote!(TypeTag::Address),
        Type::Signer => bail!("`signer` is not supported on IOTA"),
        Type::Vector(inner) => {
            let inner = type_tag_expr(inner, ctx)?;
            quote!(TypeTag::Vector(Box::new(#inner)))
        }
        Type::TypeParameter(idx) => {
            let ident = format_ident!("T{}", *idx);
            quote!(<#ident as MoveType>::type_tag(b))
        }
        Type::Reference(_, _) => bail!("references have no TypeTag"),
        Type::Datatype(_) => {
            let rust = rust_type(ty, ctx)?;
            quote!(<#rust as MoveType>::type_tag(b))
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
