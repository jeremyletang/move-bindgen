//! Build a Move package and surface a flavour-agnostic IR seed.
//!
//! Each flavour (IOTA / Sui) drives its own `move-package` fork to
//! compile the package, then funnels the result through a single
//! intermediate ([`BuiltPackage`]) so downstream IR + codegen never has
//! to branch on flavour. For Sui that funnel goes via a bytecode
//! round-trip — Sui's compiler hands back `move-binary-format` types
//! from `mystenlabs/sui`'s fork, but the rest of the pipeline only
//! knows iota's fork. Both forks agree on the wire format, so a
//! `serialize_with_version` / `deserialize_with_defaults` pair bridges
//! them cleanly.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use move_binary_format::CompiledModule;
use move_core_types::account_address::AccountAddress;

use crate::config::Flavour;

mod iota;
mod sui;

/// Per-network publishable artifact. Produced by [`build_publish`] —
/// modules sorted in dependency order with the package's own address
/// pinned to `0x0`, plus the transitive deps' on-chain addresses for
/// the target network. This is exactly what a publish PTB needs.
#[derive(Debug, Clone)]
pub(crate) struct PublishArtifact {
    /// Network name (matches `PublishNetwork::name`). Used as the
    /// codegen filename and enum-variant key.
    pub network: String,
    /// Serialized module bytecode, topologically sorted. Empty if the
    /// build produced no root modules (shouldn't happen in practice).
    pub modules: Vec<Vec<u8>>,
    /// Transitive dependency package addresses for this network.
    pub dependencies: Vec<AccountAddress>,
    /// 32-byte digest the publisher reports to the chain for this
    /// build artifact.
    pub digest: [u8; 32],
}

/// Per-module: constant-pool index → source-level constant name.
///
/// Bytecode `module.constants` carries only `(type, BCS-data)` — names
/// are dropped at compilation. The source map preserves them. Each
/// flavour reads its own source map and produces this view directly,
/// which avoids us round-tripping incompatible `SourceMap` shapes
/// between the IOTA and Sui forks.
pub type ConstantNames = Vec<Option<String>>;

/// Flavour-agnostic build output. Holds the bare minimum the IR phase
/// needs: a name, an optional published address, and the root modules
/// with their constant-name maps. Dependency modules are dropped here —
/// codegen only ever generates bindings for the root.
#[derive(Debug)]
pub(crate) struct BuiltPackage {
    pub name: String,
    pub published_at: Option<AccountAddress>,
    pub modules: Vec<(CompiledModule, ConstantNames)>,
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Move chain flavour the build targets.
    pub flavour: Flavour,
    /// Forward move-package's `BUILDING X` / `INCLUDING DEPENDENCY X`
    /// chatter to stderr. Off by default — the CLI's `Reporter` already
    /// surfaces compile progress in cargo style, and the upstream
    /// chatter overlaps and uses different formatting. Compile **errors**
    /// are reported through a different channel and remain visible
    /// regardless of this flag.
    pub print_diags_to_stderr: bool,
    /// Run the bytecode verifier on the produced bytecode. Off by
    /// default for codegen — we trust source we just compiled, and
    /// skipping it shaves a meaningful chunk off cold builds.
    pub run_bytecode_verifier: bool,
    /// Build in dev mode (`[dev-dependencies]` + `[dev-addresses]`).
    /// Default off — those blocks are for the Move developer's local
    /// testing, not for external consumers extracting types. Off-mode
    /// also avoids the conflict where a `[dev-addresses]` entry collides
    /// with our `additional_named_addresses` override for the same name.
    pub dev_mode: bool,
    /// Suppress the Move compiler's warning-channel output (lints,
    /// `[note]` chatter, `[W…]` warnings). Errors are unaffected — they
    /// always go to stderr via a separate path. Default `true` because
    /// the warnings overwhelmingly come from upstream sources we don't
    /// control (e.g. Iota framework's `///` doc-comment quirks) and
    /// drown out our own progress output.
    pub silence_warnings: bool,
    /// Optional chain ID for resolving published-at addresses from
    /// `Move.lock`. For Sui it doubles as the network selector
    /// (`mainnet` / `testnet` / `devnet`) when picking the build
    /// environment.
    pub chain_id: Option<String>,
    /// Override named addresses at build time. Used by workspace mode to
    /// give each Move package a unique synthetic address (so cross-package
    /// type refs stay distinguishable in the IR even when each package's
    /// `Move.toml` still uses `"0x0"` / `"_"` placeholders).
    pub additional_named_addresses: BTreeMap<String, AccountAddress>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            flavour: Flavour::Iota,
            print_diags_to_stderr: false,
            run_bytecode_verifier: false,
            dev_mode: false,
            silence_warnings: true,
            chain_id: None,
            additional_named_addresses: BTreeMap::new(),
        }
    }
}

/// Build a Move package located at `path` (a directory containing
/// `Move.toml`). Dispatches to a flavour-specific implementation.
pub(crate) fn build_package(path: &Path, opts: &BuildOptions) -> Result<BuiltPackage> {
    match opts.flavour {
        Flavour::Iota => iota::build(path, opts),
        Flavour::Sui => sui::build(path, opts),
    }
}

/// Run a publish-style Move build and extract the bytes a publish PTB
/// needs. The caller is responsible for setting up `opts` so the
/// package's own named address resolves to `AccountAddress::ZERO`
/// (otherwise the resulting modules carry the wrong package id).
///
/// `network` is recorded on the returned [`PublishArtifact`] so codegen
/// can route it to the right `bytecode/<name>.rs` file.
pub(crate) fn build_publish(
    path: &Path,
    opts: &BuildOptions,
    network: &str,
) -> Result<PublishArtifact> {
    match opts.flavour {
        Flavour::Iota => iota::build_publish(path, opts, network),
        Flavour::Sui => sui::build_publish(path, opts, network),
    }
}

/// Run a build closure with stderr captured; replay it only on failure.
/// move-package (both forks) hardcodes `eprintln!` for linter `[note]`
/// chatter, the "linter warnings suppressed: N" summary, and a few
/// status lines that aren't reachable from any flag. Capturing keeps
/// successful builds quiet while preserving compile-error diagnostics
/// (which write rich context to stderr just before the build returns
/// `Err`).
pub(crate) fn with_captured_stderr<R, F>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    use std::io::{Read, Write};

    // BufferRedirect can fail on platforms without a usable
    // duplicate-stderr primitive. Fall back to passthrough.
    let Ok(mut redirect) = gag::BufferRedirect::stderr() else {
        return f();
    };
    let result = f();
    let mut captured = Vec::new();
    let _ = redirect.read_to_end(&mut captured);
    drop(redirect);

    if result.is_err() && !captured.is_empty() {
        let _ = std::io::stderr().write_all(&captured);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_package_bails_on_missing_path() {
        let opts = BuildOptions::default();
        let err = build_package(Path::new("/nonexistent"), &opts).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to build Move package"),
            "expected the package-build context, got: {msg}",
        );
    }
}
