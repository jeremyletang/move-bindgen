//! IOTA build path. Drives `iota-move-build` and adapts its output to
//! [`BuiltPackage`].

use std::path::Path;

use anyhow::{Context, Result};
use iota_move_build::{BuildConfig as IotaBuildConfig, CompiledPackage, IotaPackageHooks};
use iota_package_management::system_package_versions::latest_system_packages;
use move_bytecode_source_map::source_map::SourceMap;
use move_core_types::account_address::AccountAddress;
use move_package::BuildConfig as MoveBuildConfig;

use super::{with_captured_stderr, BuildOptions, BuiltPackage, ConstantNames, PublishArtifact};

pub(super) fn build(path: &Path, opts: &BuildOptions) -> Result<BuiltPackage> {
    let pkg = compile(path, opts)?;

    let name = pkg
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

    let modules = pkg
        .package
        .root_compiled_units
        .into_iter()
        .map(|u| {
            let names = constant_names_from_iota_source_map(
                &u.unit.source_map,
                u.unit.module.constant_pool().len(),
            );
            (u.unit.module, names)
        })
        .collect();

    Ok(BuiltPackage {
        name,
        published_at,
        modules,
    })
}

/// Publish-flavoured build. Compiles with `opts` (caller is responsible
/// for setting up `additional_named_addresses` so the package's own
/// name resolves to `AccountAddress::ZERO`), then extracts the byte
/// stream + dep set the publish PTB needs.
pub(super) fn build_publish(
    path: &Path,
    opts: &BuildOptions,
    network: &str,
) -> Result<PublishArtifact> {
    let pkg = compile(path, opts)?;

    // `with_unpublished_deps = false` is the standard publish path:
    // assume every transitive dep is already on-chain and reference it
    // by `dependency_storage_package_ids`. Modules in the result carry
    // the package's address from compile time — which the caller pinned
    // to 0x0 — so the chain can substitute the freshly-minted package
    // id at publish time.
    let modules = pkg.get_package_bytes(false);

    // `get_dependency_storage_package_ids()` only returns *published*
    // deps. Workspace siblings (compiled in the same install at
    // synthetic addresses) are absent — they have no on-chain id yet.
    // The chain's linkage-table check needs every transitively-
    // referenced address to appear in the publish PTB's `dependencies`
    // list — *including* deps the root modules don't reference
    // directly but their deps' deps do. So we walk the full module
    // closure (root + transitive deps via `get_modules_and_deps`),
    // union every non-zero entry from each module's
    // `address_identifiers` pool, and use that as the dep set.
    let published_deps: Vec<AccountAddress> = pkg
        .get_dependency_storage_package_ids()
        .into_iter()
        .map(|id| AccountAddress::new(id.into_bytes()))
        .collect();
    let referenced_addrs: std::collections::BTreeSet<AccountAddress> = pkg
        .get_modules_and_deps()
        .flat_map(|m| m.address_identifiers.iter().copied())
        .collect();
    let already = published_deps
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut dependencies = published_deps.clone();
    for addr in referenced_addrs {
        if !already.contains(&addr) && addr != AccountAddress::ZERO {
            dependencies.push(addr);
        }
    }

    let digest = pkg.get_package_digest(false);

    // Reverse-map: synthetic_address → move_name. Built from the
    // `additional_named_addresses` we passed in (which carries every
    // `_`-valued name across the workspace at its synthetic value).
    // Skip the zero-address pinning rows (the package's own name) —
    // they don't appear in `dependencies` anyway and would alias.
    let synth_to_name: std::collections::BTreeMap<AccountAddress, String> = opts
        .additional_named_addresses
        .iter()
        .filter(|(_, addr)| **addr != AccountAddress::ZERO)
        .map(|(name, addr)| (*addr, name.clone()))
        .collect();
    let dep_labels: Vec<(AccountAddress, String)> = dependencies
        .iter()
        .filter_map(|addr| synth_to_name.get(addr).map(|n| (*addr, n.clone())))
        .collect();

    Ok(PublishArtifact {
        network: network.to_string(),
        modules,
        dependencies,
        digest,
        dep_labels,
    })
}


/// Shared compile path used by both [`build`] and [`build_publish`].
/// Registers the IOTA package hooks and forwards options to
/// `iota-move-build`.
fn compile(path: &Path, opts: &BuildOptions) -> Result<CompiledPackage> {
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

    let run = || cfg.build(path).map_err(anyhow::Error::from);
    let pkg = if opts.print_diags_to_stderr {
        run()
    } else {
        with_captured_stderr(run)
    };
    pkg.with_context(|| format!("failed to build Move package at {}", path.display()))
}

fn constant_names_from_iota_source_map(sm: &SourceMap, num_constants: usize) -> ConstantNames {
    let mut names = vec![None; num_constants];
    for (name, &idx) in &sm.constant_map {
        let i = idx as usize;
        if i < names.len() {
            names[i] = Some(name.to_string());
        }
    }
    names
}
