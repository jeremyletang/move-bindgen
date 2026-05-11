//! IOTA build path. Drives `iota-move-build` and adapts its output to
//! [`BuiltPackage`].

use std::path::Path;

use anyhow::{Context, Result};
use iota_move_build::{BuildConfig as IotaBuildConfig, IotaPackageHooks};
use iota_package_management::system_package_versions::latest_system_packages;
use move_bytecode_source_map::source_map::SourceMap;
use move_core_types::account_address::AccountAddress;
use move_package::BuildConfig as MoveBuildConfig;

use super::{BuildOptions, BuiltPackage, ConstantNames, with_captured_stderr};

pub(super) fn build(path: &Path, opts: &BuildOptions) -> Result<BuiltPackage> {
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

    let build = || cfg.build(path).map_err(anyhow::Error::from);
    let pkg = if opts.print_diags_to_stderr {
        build()
    } else {
        with_captured_stderr(build)
    };
    let pkg = pkg
        .with_context(|| format!("failed to build Move package at {}", path.display()))?;

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
