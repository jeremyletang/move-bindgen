//! In-memory IR consumed by codegen.
//!
//! Walks the root modules of a built Move package and reduces each to a
//! `normalized::Module`. Dependency modules are skipped — the user wants
//! bindings for *their* package, not the framework.

use std::path::Path;

use anyhow::Result;
use move_binary_format::normalized::{self, NoPool};
use move_core_types::{account_address::AccountAddress, identifier::Identifier};

use crate::build::{build_package, BuildOptions};
use crate::docs::{self, DocMap};

/// Per-module: constant-pool index → source-level constant name.
///
/// Bytecode `module.constants` carries only `(type, BCS-data)` — names are
/// dropped at compilation. The source map preserves them, so we read it
/// alongside the bytecode and reverse the `name → idx` mapping. `None` at
/// some index means that constant was synthesised by the compiler (e.g.
/// `#[error]` clever-error metadata, or address literals from source) and
/// has no user-facing name.
pub type ConstantNames = Vec<Option<String>>;

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
    /// Parallel to `modules`. Indexed the same way as
    /// `module.constants` — see [`ConstantNames`].
    pub constant_names: Vec<ConstantNames>,
    /// Source-level `///` doc comments, keyed by module/item/field. Empty
    /// if the package's `sources/` directory is missing or has no docs.
    pub docs: DocMap,
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
    let mut modules = Vec::with_capacity(pkg.package.root_compiled_units.len());
    let mut constant_names = Vec::with_capacity(pkg.package.root_compiled_units.len());

    for u in pkg.package.root_compiled_units.iter() {
        let normalized =
            normalized::Module::new(&mut pool, &u.unit.module, /* include_code */ false);
        let names = constant_names_from_source_map(&u.unit.source_map, normalized.constants.len());
        modules.push(normalized);
        constant_names.push(names);
    }

    let docs = docs::collect(&path.join("sources"))?;

    Ok(Bindings {
        package_name,
        published_at,
        modules,
        constant_names,
        docs,
    })
}

/// Read constant names out of the source map, indexed by constant-pool index.
fn constant_names_from_source_map(
    sm: &move_bytecode_source_map::source_map::SourceMap,
    num_constants: usize,
) -> ConstantNames {
    let mut names = vec![None; num_constants];
    for (name, &idx) in &sm.constant_map {
        let i = idx as usize;
        if i < names.len() {
            names[i] = Some(name.to_string());
        }
    }
    names
}
