//! In-memory IR consumed by codegen.
//!
//! Walks the root modules of a built Move package and reduces each to a
//! `normalized::Module`. Dependency modules are skipped — the user wants
//! bindings for *their* package, not the framework.

use std::path::Path;

use anyhow::Result;
use move_binary_format::normalized::{self, NoPool};
use move_core_types::{account_address::AccountAddress, identifier::Identifier};

use crate::build::{build_package, build_publish, BuildOptions};
use crate::config::PublishNetwork;
use crate::docs::{self, DocMap};
use crate::install::read_addresses_block;
use crate::reporter::Reporter;

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
    /// Per-network publish artifacts. Empty when no `[publish].networks`
    /// were configured (or for non-target packages — only top-level
    /// `[packages.*]` entries get publish bytes; transitive deps don't).
    pub publish: Vec<NetworkArtifact>,
}

/// One configured network's publish-bytes view. Mirrors
/// [`crate::build::PublishArtifact`] in the public API.
#[derive(Debug, Clone)]
pub struct NetworkArtifact {
    pub network: String,
    pub modules: Vec<Vec<u8>>,
    pub dependencies: Vec<AccountAddress>,
    pub digest: [u8; 32],
}

/// Build `path` and produce the IR.
pub fn load_package(path: &Path) -> Result<Bindings> {
    load_package_with_options(path, &BuildOptions::default())
}

pub fn load_package_with_options(path: &Path, opts: &BuildOptions) -> Result<Bindings> {
    load_package_for_publish(path, opts, &[], None)
}

/// Same as [`load_package_with_options`] but also runs a publish build
/// per entry in `networks`, returning the artifacts on `Bindings`.
///
/// The publish builds pin every `_`-valued entry in the package's
/// `[addresses]` block to `AccountAddress::ZERO`. Those entries are the
/// package's own named address(es) — the chain substitutes the
/// freshly-minted package id for them at publish time. Dep addresses
/// (named in `[dependencies]`) keep their synthetic codegen values
/// unless the network's `addresses` override map says otherwise.
///
/// `reporter`, if provided, gets a `Compiling <pkg> (publish/<network>)`
/// stage per network so users see why time is going by on slow builds.
pub fn load_package_for_publish(
    path: &Path,
    opts: &BuildOptions,
    networks: &[PublishNetwork],
    reporter: Option<&Reporter>,
) -> Result<Bindings> {
    let pkg = build_package(path, opts)?;

    // The set of named-address slots that need to resolve to `0x0` for
    // a publishable build. Read straight out of the staged Move.toml's
    // `[addresses]` block — every entry whose value is `_` is the
    // package's own address (often more than one, since install adds
    // both the original snake_case form *and* a lowercased move-name
    // form as placeholders). Pin them all; harmless for the dummy ones.
    let own_address_names: Vec<String> = if networks.is_empty() {
        Vec::new()
    } else {
        read_addresses_block(&path.join("Move.toml"))?
            .into_iter()
            .filter_map(|(k, v)| (v == "_").then_some(k))
            .collect()
    };

    let mut pool = NoPool;
    let mut modules = Vec::with_capacity(pkg.modules.len());
    let mut constant_names = Vec::with_capacity(pkg.modules.len());

    for (module, names) in pkg.modules.into_iter() {
        let normalized = normalized::Module::new(&mut pool, &module, /* include_code */ false);
        modules.push(normalized);
        constant_names.push(names);
    }

    let docs = docs::collect(&path.join("sources"))?;

    let mut publish = Vec::with_capacity(networks.len());
    for net in networks {
        if let Some(r) = reporter {
            r.stage(
                "Compiling",
                format!("{} (publish/{})", pkg.name, net.name),
            );
        }
        let pub_opts = publish_opts(opts, net, &own_address_names);
        let artifact = build_publish(path, &pub_opts, &net.name)?;
        publish.push(NetworkArtifact {
            network: artifact.network,
            modules: artifact.modules,
            dependencies: artifact.dependencies,
            digest: artifact.digest,
        });
    }

    Ok(Bindings {
        package_name: pkg.name,
        published_at: pkg.published_at,
        modules,
        constant_names,
        docs,
        publish,
    })
}

/// Build `BuildOptions` for a publish pass: chain_id comes from the
/// network entry, additional addresses are the codegen-build's overrides
/// with the package's own name pinned to `0x0`, and the network's
/// `addresses` map layered on top.
fn publish_opts(
    codegen_opts: &BuildOptions,
    net: &PublishNetwork,
    own_address_names: &[String],
) -> BuildOptions {
    let mut addrs = codegen_opts.additional_named_addresses.clone();
    for name in own_address_names {
        addrs.insert(name.clone(), AccountAddress::ZERO);
    }
    for (k, v) in &net.addresses {
        addrs.insert(k.clone(), *v);
    }
    BuildOptions {
        chain_id: Some(net.effective_chain_id().to_string()),
        additional_named_addresses: addrs,
        ..codegen_opts.clone()
    }
}
