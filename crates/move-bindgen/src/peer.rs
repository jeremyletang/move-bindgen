//! Peer-package address map used by codegen in workspace mode.
//!
//! When multiple Move packages are generated as a Cargo workspace, types
//! defined in one package can be referenced from another. Each generated
//! crate is keyed by the Move package's address; cross-package datatype
//! references are emitted as `::peer_crate::module::Type` (instead of the
//! same-package `super::module::Type`).
//!
//! Built once per `move-bindgen generate` run by walking the resolved
//! `[packages.*]` entries; consulted by `ty.rs` / `function.rs` at codegen
//! time. Single-crate mode uses an empty map and behaves as before.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{bail, Result};
use move_binary_format::normalized::{Datatype, Module, Type};
use move_core_types::{account_address::AccountAddress, identifier::Identifier};

use crate::Bindings;

/// Address → peer crate info.
#[derive(Debug, Clone, Default)]
pub struct PeerMap {
    by_address: HashMap<AccountAddress, PeerEntry>,
}

#[derive(Debug, Clone)]
pub struct PeerEntry {
    /// Rust crate name (e.g. `"oracle-price-feed-rs"`). Render-time
    /// kebab→snake conversion is the consumer's responsibility.
    pub crate_name: String,
}

impl PeerMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a peer. Errors if `addr` is already registered.
    pub fn insert(&mut self, addr: AccountAddress, crate_name: String) -> Result<()> {
        if let Some(prev) = self.by_address.get(&addr) {
            bail!(
                "two packages share address 0x{}: '{}' and '{}'.\n\
                 \n\
                 Each Move package needs a distinct address for codegen. Either:\n  \
                   - publish the packages and set `[package].published-at` in each `Move.toml`, or\n  \
                   - replace literal `\"0x0\"` placeholders in `[addresses]` with `\"_\"` so move-bindgen \
                     can assign each a unique synthetic address.",
                addr.short_str_lossless(),
                prev.crate_name,
                crate_name,
            );
        }
        self.by_address.insert(addr, PeerEntry { crate_name });
        Ok(())
    }

    pub fn lookup(&self, addr: &AccountAddress) -> Option<&PeerEntry> {
        self.by_address.get(addr)
    }

    pub fn is_empty(&self) -> bool {
        self.by_address.is_empty()
    }
}

/// Walk every type reference in `bindings` and return the set of foreign
/// package addresses (i.e. not the addresses of any module in `bindings`)
/// that appear. Used by codegen to compute the set of peer-crate path
/// dependencies for a generated member crate.
pub fn foreign_addresses_used(bindings: &Bindings) -> HashSet<AccountAddress> {
    let own_addresses: BTreeSet<AccountAddress> =
        bindings.modules.iter().map(|m| m.id.address).collect();
    let mut out = HashSet::new();
    for m in &bindings.modules {
        walk_module(m, &own_addresses, &mut out);
    }
    out
}

fn walk_module(
    m: &Module<Identifier>,
    own: &BTreeSet<AccountAddress>,
    out: &mut HashSet<AccountAddress>,
) {
    for s in m.structs.values() {
        for f in &s.fields {
            walk_type(&f.type_, own, out);
        }
    }
    for e in m.enums.values() {
        for v in &e.variants {
            for f in &v.fields {
                walk_type(&f.type_, own, out);
            }
        }
    }
    for f in m.functions.values() {
        for p in f.parameters.iter() {
            walk_type(p, own, out);
        }
        for r in f.return_.iter() {
            walk_type(r, own, out);
        }
    }
}

fn walk_type(
    ty: &Type<Identifier>,
    own: &BTreeSet<AccountAddress>,
    out: &mut HashSet<AccountAddress>,
) {
    match ty {
        Type::Datatype(dt) => walk_datatype(dt, own, out),
        Type::Vector(inner) | Type::Reference(_, inner) => walk_type(inner, own, out),
        _ => {}
    }
}

fn walk_datatype(
    dt: &Datatype<Identifier>,
    own: &BTreeSet<AccountAddress>,
    out: &mut HashSet<AccountAddress>,
) {
    let addr = dt.module.address;
    if !own.contains(&addr) {
        out.insert(addr);
    }
    for arg in &dt.type_arguments {
        walk_type(arg, own, out);
    }
}
