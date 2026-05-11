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

pub use crate::build::ConstantNames;

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
    /// Parallel to `modules`. For each module, a `Vec<Option<String>>`
    /// indexed the same way as `module.constants` — `Some(name)` for
    /// source-level constants, `None` for compiler-synthesised ones
    /// (e.g. `#[error]` clever-error metadata or address literals).
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

    let mut pool = NoPool;
    let mut modules = Vec::with_capacity(pkg.modules.len());
    let mut constant_names = Vec::with_capacity(pkg.modules.len());

    for (module, names) in pkg.modules.into_iter() {
        let normalized = normalized::Module::new(&mut pool, &module, /* include_code */ false);
        modules.push(normalized);
        constant_names.push(names);
    }

    let docs = docs::collect(&path.join("sources"))?;

    Ok(Bindings {
        package_name: pkg.name,
        published_at: pkg.published_at,
        modules,
        constant_names,
        docs,
    })
}
