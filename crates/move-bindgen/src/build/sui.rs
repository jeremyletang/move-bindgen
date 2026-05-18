//! Sui build path. Drives `sui-move-build`, then round-trips each
//! compiled module through the bytecode wire format into iota's
//! `move-binary-format` so the rest of the pipeline stays
//! flavour-agnostic.
//!
//! The source-map types diverge between the IOTA and Sui forks (Sui's
//! `FunctionSourceMap` has an extra `labels` field), so source maps are
//! *not* round-tripped — instead each flavour reads `constant_map`
//! locally and hands the IR phase a pre-extracted `index → name` view.

use std::path::Path;

use anyhow::{Context, Result};
use move_binary_format::file_format_common::VERSION_MAX;
use move_binary_format::CompiledModule;
use move_core_types::account_address::AccountAddress;
use move_package_alt::schema::Environment;
use move_package_alt_compilation::build_config::BuildConfig as MoveAltBuildConfig;
use move_package_alt_compilation::compiled_package::CompiledUnitWithSource;
use sui_move_build::BuildConfig as SuiBuildConfig;
use sui_package_alt::{mainnet_environment, testnet_environment, SuiFlavor};

use super::{with_captured_stderr, BuildOptions, BuiltPackage, ConstantNames, PublishArtifact};

pub(super) fn build(path: &Path, opts: &BuildOptions) -> Result<BuiltPackage> {
    let cfg = make_config(opts);

    let pkg = if opts.print_diags_to_stderr {
        cfg.build(path)
    } else {
        with_captured_stderr(|| cfg.build(path))
    };
    let pkg = pkg.with_context(|| format!("failed to build Move package at {}", path.display()))?;

    let name = pkg
        .package
        .compiled_package_info
        .package_name
        .as_str()
        .to_string();

    let published_at = pkg
        .published_at
        .map(|id| AccountAddress::new(id.into_bytes()));

    let mut modules = Vec::with_capacity(pkg.package.root_compiled_units.len());
    for u in pkg.package.root_compiled_units.iter() {
        modules.push(roundtrip_unit(u)?);
    }

    Ok(BuiltPackage {
        name,
        published_at,
        modules,
    })
}

/// Publish-flavoured build for Sui. Not yet implemented — codegen still
/// emits the same `Package::deployer(...)` surface for Sui packages so
/// the API stays at parity, but the runtime `PackageDeployer::execute`
/// is a `unimplemented!`. Mirror this build path when wiring real Sui
/// deploy: see `iota::build_publish` for the IOTA shape.
pub(super) fn build_publish(
    _path: &Path,
    _opts: &BuildOptions,
    network: &str,
) -> Result<PublishArtifact> {
    Ok(PublishArtifact {
        network: network.to_string(),
        modules: Vec::new(),
        dependencies: Vec::new(),
        digest: [0u8; 32],
        dep_labels: Vec::new(),
    })
}

fn make_config(opts: &BuildOptions) -> SuiBuildConfig {
    let environment: Environment = opts
        .chain_id
        .as_deref()
        .and_then(environment_for_chain_id)
        .unwrap_or_else(testnet_environment);

    let alt_config = MoveAltBuildConfig {
        // move-package-alt has no direct equivalent of iota's `dev_mode`
        // (which gates `[dev-dependencies]` / `[dev-addresses]`). For
        // codegen we never want test-only code, so leave `test_mode`
        // off regardless of `opts.dev_mode`.
        test_mode: false,
        silence_warnings: opts.silence_warnings,
        additional_named_addresses: opts
            .additional_named_addresses
            .iter()
            .map(|(k, addr)| (k.clone(), iota_to_sui_address(*addr)))
            .collect(),
        ..MoveAltBuildConfig::default()
    };

    SuiBuildConfig {
        config: alt_config,
        run_bytecode_verifier: opts.run_bytecode_verifier,
        print_diags_to_stderr: opts.print_diags_to_stderr,
        environment,
        flavor: SuiFlavor::new(),
    }
}

/// Bridge between iota's and Sui's `move-core-types` forks. Both wrap a
/// `[u8; 32]`; only the Rust type identity differs.
fn iota_to_sui_address(
    addr: AccountAddress,
) -> move_core_types_sui::account_address::AccountAddress {
    move_core_types_sui::account_address::AccountAddress::new(addr.into_bytes())
}

/// Map a known Sui chain-id string to a `move-package-alt` environment.
/// Returns `None` for unrecognised ids so the caller can fall back to
/// testnet.
fn environment_for_chain_id(chain_id: &str) -> Option<Environment> {
    match chain_id {
        "mainnet" => Some(mainnet_environment()),
        "testnet" => Some(testnet_environment()),
        _ => None,
    }
}

/// Round-trip a Sui-built unit's bytecode through the wire format into
/// iota's `move-binary-format`, and extract the constant-name view
/// directly from Sui's source map (avoiding the fork divergence in
/// source-map struct layout). Bytecode V5–V7 agrees across forks, so
/// the module round-trip is lossless in practice.
///
/// Assumption: both forks agree on the bytecode wire format for the
/// version range they advertise. If the two ever drift on version
/// support, deserialization will fail loudly (not silently corrupt) —
/// the smoke tests are expected to catch it.
fn roundtrip_unit(u: &CompiledUnitWithSource) -> Result<(CompiledModule, ConstantNames)> {
    let mut module_bytes = Vec::new();
    u.unit
        .module
        .serialize_with_version(VERSION_MAX, &mut module_bytes)
        .context("re-serializing Sui-compiled module to wire bytes")?;
    let module = CompiledModule::deserialize_with_defaults(&module_bytes).map_err(|e| {
        anyhow::anyhow!("re-parsing Sui-compiled module as iota CompiledModule: {e:?}")
    })?;

    let num_constants = module.constant_pool().len();
    let mut names: ConstantNames = vec![None; num_constants];
    for (name, &idx) in &u.unit.source_map.constant_map {
        let i = idx as usize;
        if i < names.len() {
            names[i] = Some(name.to_string());
        }
    }

    Ok((module, names))
}
