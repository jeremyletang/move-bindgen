//! Codegen for module-level Move `const` declarations.
//!
//! Bytecode `module.constants` only carries `(type, BCS-bytes)`; the
//! constant's source name comes from the source map (collected in `ir.rs`
//! into `Bindings::constant_names`). Compiler-synthesised constants
//! (`#[error]` clever-error metadata, `@addr` literals from source) have no
//! source name and are skipped here.
//!
//! Move's const grammar is restricted to bool / unsigned ints / address /
//! `vector<...>` over those, so the Rust mapping fits cleanly into `const`
//! form (`bool`, `uN`, `Address`, `&[u8]`, `&[uN]`, …). Other `vector<T>`
//! shapes drop to `&[…]` of the right Rust type.

use anyhow::Result;
use move_binary_format::normalized::{Constant, Type};
use move_core_types::identifier::Identifier;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;

use crate::codegen::ty::TypeCtx;
use crate::ir::ConstantNames;

/// Emit `pub const NAME: T = value;` for every named constant in `module`.
pub fn emit_constants(
    constants: &[std::rc::Rc<Constant<Identifier>>],
    names: &ConstantNames,
    ctx: &TypeCtx,
) -> Result<TokenStream> {
    let mut out = TokenStream::new();
    for (i, c) in constants.iter().enumerate() {
        let Some(name) = names.get(i).and_then(|n| n.as_ref()) else {
            continue;
        };
        if let Some(item) = emit_one(name, c, ctx)? {
            out.extend(item);
        }
    }
    Ok(out)
}

fn emit_one(name: &str, c: &Constant<Identifier>, ctx: &TypeCtx) -> Result<Option<TokenStream>> {
    let ident = Ident::new(name, Span::call_site());
    let Some((ty, value)) = decode(&c.type_, &c.data)? else {
        // Type we don't know how to const-emit (e.g. vector<DatatypeRef>).
        // Skip silently — Move's const grammar restricts these but we may
        // hit corner cases as the language evolves.
        return Ok(None);
    };
    let doc_attr = ctx
        .docs
        .item(ctx.current_module.as_str(), name)
        .map(crate::codegen::outer_doc)
        .unwrap_or_default();
    Ok(Some(quote! {
        #doc_attr
        pub const #ident: #ty = #value;
    }))
}

/// Map a const's type+BCS-bytes to (Rust type tokens, value tokens).
/// Returns `None` if we can't represent this const at compile time.
fn decode(ty: &Type<Identifier>, data: &[u8]) -> Result<Option<(TokenStream, TokenStream)>> {
    Ok(match ty {
        Type::Bool => {
            let v: bool = bcs::from_bytes(data)?;
            Some((quote!(bool), quote!(#v)))
        }
        Type::U8 => {
            let v: u8 = bcs::from_bytes(data)?;
            Some((quote!(u8), quote!(#v)))
        }
        Type::U16 => {
            let v: u16 = bcs::from_bytes(data)?;
            Some((quote!(u16), quote!(#v)))
        }
        Type::U32 => {
            let v: u32 = bcs::from_bytes(data)?;
            Some((quote!(u32), quote!(#v)))
        }
        Type::U64 => {
            let v: u64 = bcs::from_bytes(data)?;
            Some((quote!(u64), quote!(#v)))
        }
        Type::U128 => {
            let v: u128 = bcs::from_bytes(data)?;
            Some((quote!(u128), quote!(#v)))
        }
        Type::U256 => None, // not yet supported
        Type::Address => {
            let v: [u8; 32] = bcs::from_bytes(data)?;
            let bytes = v.iter().map(|b| quote!(#b));
            Some((quote!(Address), quote!(Address::new([ #( #bytes ),* ]))))
        }
        Type::Vector(inner) => decode_vector(inner, data)?,
        // signer / Datatype / TypeParameter / Reference can't be const targets.
        _ => None,
    })
}

fn decode_vector(
    inner: &Type<Identifier>,
    data: &[u8],
) -> Result<Option<(TokenStream, TokenStream)>> {
    Ok(match inner {
        Type::U8 => {
            let v: Vec<u8> = bcs::from_bytes(data)?;
            let bytes = v.iter().map(|b| quote!(#b));
            Some((quote!(&[u8]), quote!(&[ #( #bytes ),* ])))
        }
        Type::Bool => {
            let v: Vec<bool> = bcs::from_bytes(data)?;
            let elems = v.iter().map(|b| quote!(#b));
            Some((quote!(&[bool]), quote!(&[ #( #elems ),* ])))
        }
        Type::U16 => {
            let v: Vec<u16> = bcs::from_bytes(data)?;
            let elems = v.iter().map(|x| quote!(#x));
            Some((quote!(&[u16]), quote!(&[ #( #elems ),* ])))
        }
        Type::U32 => {
            let v: Vec<u32> = bcs::from_bytes(data)?;
            let elems = v.iter().map(|x| quote!(#x));
            Some((quote!(&[u32]), quote!(&[ #( #elems ),* ])))
        }
        Type::U64 => {
            let v: Vec<u64> = bcs::from_bytes(data)?;
            let elems = v.iter().map(|x| quote!(#x));
            Some((quote!(&[u64]), quote!(&[ #( #elems ),* ])))
        }
        Type::U128 => {
            let v: Vec<u128> = bcs::from_bytes(data)?;
            let elems = v.iter().map(|x| quote!(#x));
            Some((quote!(&[u128]), quote!(&[ #( #elems ),* ])))
        }
        Type::Address => {
            let v: Vec<[u8; 32]> = bcs::from_bytes(data)?;
            let lits = v.iter().map(|a| {
                let bytes = a.iter().map(|b| quote!(#b));
                quote!(Address::new([ #( #bytes ),* ]))
            });
            Some((quote!(&[Address]), quote!(&[ #( #lits ),* ])))
        }
        // Nested vectors / other types: leave for a follow-up.
        _ => None,
    })
}
