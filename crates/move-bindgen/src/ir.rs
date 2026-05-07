//! In-memory IR consumed by codegen.
//!
//! Walks the root modules of a built Move package and reduces each to a
//! `normalized::Module`. Dependency modules are skipped — the user wants
//! bindings for *their* package, not the framework.

use std::path::Path;

use anyhow::Result;
use move_binary_format::normalized::{self, NoPool};
use move_core_types::{account_address::AccountAddress, identifier::Identifier};

use crate::build::{BuildOptions, build_package};

/// Output of the IR phase.
///
/// `Identifier` is the chosen name representation (no Rc/Arc interning — pkgs
/// are small and codegen reads each name once).
#[derive(Debug)]
pub struct Bindings {
    pub package_name: String,
    /// `published-at` from `Move.toml`, if set. Required at codegen time to
    /// emit correct `TypeTag`s; allowed to be absent so we can run the IR
    /// dump on unpublished packages.
    pub published_at: Option<AccountAddress>,
    pub modules: Vec<normalized::Module<Identifier>>,
}

/// Build `path` and produce the IR.
pub fn load_package(path: &Path) -> Result<Bindings> {
    load_package_with_options(path, &BuildOptions::default())
}

pub fn load_package_with_options(path: &Path, opts: &BuildOptions) -> Result<Bindings> {
    let pkg = build_package(path, opts)?;

    let package_name = pkg
        .package
        .compiled_package_info
        .package_name
        .as_str()
        .to_string();

    let published_at = pkg
        .published_at
        .as_ref()
        .ok()
        .map(|id| AccountAddress::new(id.into_bytes()));

    let mut pool = NoPool;
    let modules = pkg
        .package
        .root_compiled_units
        .iter()
        .map(|u| normalized::Module::new(&mut pool, &u.unit.module, /* include_code */ false))
        .collect();

    Ok(Bindings {
        package_name,
        published_at,
        modules,
    })
}
