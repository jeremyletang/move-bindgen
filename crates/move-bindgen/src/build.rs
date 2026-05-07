//! Wraps `iota-move-build`'s `BuildConfig::build`.
//!
//! Drops the IOTA-specific knobs (`run_bytecode_verifier`, `chain_id`, …) into
//! sane defaults so callers normally only need a path. `BuildOptions` exists
//! for the cases where they don't.

use std::path::Path;

use anyhow::{Context, Result};
use iota_move_build::{BuildConfig as IotaBuildConfig, CompiledPackage, IotaPackageHooks};
use iota_package_management::system_package_versions::latest_system_packages;
use move_package::BuildConfig as MoveBuildConfig;

#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Forward compiler diagnostics to stderr while building.
    pub print_diags_to_stderr: bool,
    /// Run the IOTA bytecode verifier on the produced bytecode. Off by default
    /// for codegen — we trust source we just compiled, and skipping it shaves
    /// a meaningful chunk off cold builds.
    pub run_bytecode_verifier: bool,
    /// Optional chain ID for resolving published-at addresses from `Move.lock`.
    pub chain_id: Option<String>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            print_diags_to_stderr: true,
            run_bytecode_verifier: false,
            chain_id: None,
        }
    }
}

/// Build a Move package located at `path` (a directory containing `Move.toml`).
pub(crate) fn build_package(path: &Path, opts: &BuildOptions) -> Result<CompiledPackage> {
    move_package::package_hooks::register_package_hooks(Box::new(IotaPackageHooks));

    let config = MoveBuildConfig {
        implicit_dependencies: iota_move_build::implicit_deps(latest_system_packages()),
        ..Default::default()
    };

    IotaBuildConfig {
        config,
        run_bytecode_verifier: opts.run_bytecode_verifier,
        print_diags_to_stderr: opts.print_diags_to_stderr,
        chain_id: opts.chain_id.clone(),
    }
    .build(path)
    .with_context(|| format!("failed to build Move package at {}", path.display()))
}
