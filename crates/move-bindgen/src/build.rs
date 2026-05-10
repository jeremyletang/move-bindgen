//! Wraps `iota-move-build`'s `BuildConfig::build`.
//!
//! Drops the IOTA-specific knobs (`run_bytecode_verifier`, `chain_id`, …) into
//! sane defaults so callers normally only need a path. `BuildOptions` exists
//! for the cases where they don't.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use iota_move_build::{BuildConfig as IotaBuildConfig, CompiledPackage, IotaPackageHooks};
use iota_package_management::system_package_versions::latest_system_packages;
use move_core_types::account_address::AccountAddress;
use move_package::BuildConfig as MoveBuildConfig;

use crate::config::Flavour;

#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Move chain flavour the build targets. Selects which build
    /// chain (`iota_move_build` vs Sui's equivalent — landing in a
    /// later phase) is invoked. Defaults to `Iota`.
    pub flavour: Flavour,
    /// Forward move-package's `BUILDING X` / `INCLUDING DEPENDENCY X`
    /// chatter to stderr. Off by default — the CLI's `Reporter` already
    /// surfaces compile progress in cargo style, and the upstream
    /// chatter overlaps and uses different formatting. Compile **errors**
    /// are reported through a different channel and remain visible
    /// regardless of this flag.
    pub print_diags_to_stderr: bool,
    /// Run the IOTA bytecode verifier on the produced bytecode. Off by default
    /// for codegen — we trust source we just compiled, and skipping it shaves
    /// a meaningful chunk off cold builds.
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
    /// Optional chain ID for resolving published-at addresses from `Move.lock`.
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
pub(crate) fn build_package(path: &Path, opts: &BuildOptions) -> Result<CompiledPackage> {
    match opts.flavour {
        Flavour::Iota => build_package_iota(path, opts),
        Flavour::Sui => bail!("Sui flavour is not yet implemented"),
    }
}

fn build_package_iota(path: &Path, opts: &BuildOptions) -> Result<CompiledPackage> {
    move_package::package_hooks::register_package_hooks(Box::new(IotaPackageHooks));

    let config = MoveBuildConfig {
        dev_mode: opts.dev_mode,
        additional_named_addresses: opts.additional_named_addresses.clone(),
        implicit_dependencies: iota_move_build::implicit_deps(latest_system_packages()),
        silence_warnings: opts.silence_warnings,
        ..Default::default()
    };

    let cfg = IotaBuildConfig {
        config,
        run_bytecode_verifier: opts.run_bytecode_verifier,
        print_diags_to_stderr: opts.print_diags_to_stderr,
        chain_id: opts.chain_id.clone(),
    };

    if opts.print_diags_to_stderr {
        // User opted in to upstream chatter (e.g. for debugging).
        return cfg
            .build(path)
            .with_context(|| format!("failed to build Move package at {}", path.display()));
    }
    build_with_captured_stderr(cfg, path)
        .with_context(|| format!("failed to build Move package at {}", path.display()))
}

/// Run the build with stderr piped into a buffer; replay it only on
/// failure. Move-package and iota-move-build hardcode `eprintln!` for
/// the linter `[note]` chatter, the "linter warnings suppressed: N"
/// summary, and a few other status lines that aren't reachable from any
/// flag. Capturing keeps successful builds quiet while preserving
/// compile-error diagnostics (which write rich context to stderr just
/// before the build returns `Err`).
fn build_with_captured_stderr(cfg: IotaBuildConfig, path: &Path) -> Result<CompiledPackage> {
    use std::io::{Read, Write};

    // BufferRedirect can fail on platforms without a usable
    // duplicate-stderr primitive. Fall back to passthrough on init
    // failure — noisy but correct.
    let Ok(mut redirect) = gag::BufferRedirect::stderr() else {
        return cfg.build(path).map_err(anyhow::Error::from);
    };
    let result = cfg.build(path);
    let mut captured = Vec::new();
    let _ = redirect.read_to_end(&mut captured);
    drop(redirect);

    if result.is_err() && !captured.is_empty() {
        let _ = std::io::stderr().write_all(&captured);
    }
    result.map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_package_bails_on_unimplemented_sui() {
        // Make sure we error early with a friendly message instead of
        // mysteriously trying to drive `iota_move_build` against a
        // Sui package.
        let opts = BuildOptions {
            flavour: Flavour::Sui,
            ..Default::default()
        };
        let err = build_package(std::path::Path::new("/nonexistent"), &opts).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Sui flavour is not yet implemented"),
            "expected the not-yet-implemented bail, got: {msg}",
        );
    }
}
