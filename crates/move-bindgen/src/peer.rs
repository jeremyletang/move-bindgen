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

/// Outcome of [`PeerMap::insert`]. `Aliased` carries the canonical
/// crate-name the address already maps to so the caller can surface
/// it (e.g. via the reporter) and skip generating a redundant crate.
#[derive(Debug, Clone)]
pub enum InsertOutcome {
    Inserted,
    Aliased { canonical: String },
}

impl PeerMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a peer. First-wins: if `addr` is already registered,
    /// returns `InsertOutcome::Aliased { canonical }` carrying the
    /// existing crate name and leaves the map unchanged.
    ///
    /// Why first-wins: Move identifies types by `(address, module,
    /// name)`. Two Move packages compiled against the same address
    /// (e.g. `iota = "0x2"`) emit IR-equivalent types — they're
    /// indistinguishable at the type level, so a single canonical
    /// Rust crate covers references from anywhere. We elect one and
    /// route everyone through it.
    ///
    /// If the address-sharing packages have *divergent* APIs (e.g. a
    /// vendored Iota cut against a newer git Iota), missing items
    /// surface as cargo errors downstream — that's the user's signal
    /// to align framework versions, since we can't paper over
    /// genuinely-divergent APIs.
    pub fn insert(&mut self, addr: AccountAddress, crate_name: String) -> InsertOutcome {
        if let Some(prev) = self.by_address.get(&addr) {
            return InsertOutcome::Aliased {
                canonical: prev.crate_name.clone(),
            };
        }
        self.by_address.insert(addr, PeerEntry { crate_name });
        InsertOutcome::Inserted
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
