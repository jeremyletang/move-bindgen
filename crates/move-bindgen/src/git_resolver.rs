//! Resolve `PackageSource::Git` entries to local on-disk paths via
//! `move-package`'s built-in git fetcher.
//!
//! No clone logic of our own — we lean on
//! `MoveBuildConfig::resolution_graph_for_package`, which already handles
//! fetching, caching (under `~/.move/`), locking, and rev/branch/tag
//! resolution. Subsequent calls for the same `(url, rev)` hit the cache
//! and return immediately.
//!
//! Strategy: we write a synthetic Move.toml in a scratch directory whose
//! sole `[dependencies]` is the git source we want resolved, run the
//! resolution graph, and read the resolved package's on-disk path back
//! out.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use iota_move_build::IotaPackageHooks;
use iota_package_management::system_package_versions::latest_system_packages;
use move_package::package_hooks::register_package_hooks;
use move_package::BuildConfig as MoveBuildConfig;
use move_symbol_pool::Symbol;

use crate::config::PackageSource;

/// Probe name used in the synthetic Move.toml. Used only to look up the
/// resolved entry in the `package_table` afterwards.
const PROBE_NAME: &str = "MoveBindgenGitProbe";

/// Resolve a `PackageSource::Git` to a local directory containing
/// `Move.toml`. `scratch_root` is where the synthetic manifest is
/// written; pass an empty/disposable directory.
pub fn resolve_git_source(source: &PackageSource, scratch_root: &Path) -> Result<PathBuf> {
    let (url, rev, branch, tag, subdir) = match source {
        PackageSource::Git {
            url,
            rev,
            branch,
            tag,
            subdir,
        } => (
            url.as_str(),
            rev.as_deref(),
            branch.as_deref(),
            tag.as_deref(),
            subdir.as_deref(),
        ),
        PackageSource::Path(_) => bail!("resolve_git_source called with a Path source"),
    };

    register_package_hooks(Box::new(IotaPackageHooks));

    let scratch = scratch_root.join("git-resolver-probe");
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch)
            .with_context(|| format!("clearing scratch dir {}", scratch.display()))?;
    }
    std::fs::create_dir_all(scratch.join("sources"))
        .with_context(|| format!("creating scratch dir {}", scratch.display()))?;

    let mut manifest = String::new();
    manifest.push_str(&format!(
        "[package]\n\
         name = \"{PROBE_NAME}\"\n\
         edition = \"2024\"\n\
         \n\
         [addresses]\n\
         move_bindgen_git_probe = \"0x0\"\n\
         \n\
         [dependencies.Probe]\n\
         git = \"{url}\"\n",
    ));
    if let Some(v) = rev {
        manifest.push_str(&format!("rev = \"{v}\"\n"));
    }
    if let Some(v) = branch {
        manifest.push_str(&format!("branch = \"{v}\"\n"));
    }
    if let Some(v) = tag {
        manifest.push_str(&format!("tag = \"{v}\"\n"));
    }
    if let Some(v) = subdir {
        manifest.push_str(&format!("subdir = \"{v}\"\n"));
    }

    let manifest_path = scratch.join("Move.toml");
    std::fs::write(&manifest_path, manifest)
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    let cfg = MoveBuildConfig {
        implicit_dependencies: iota_move_build::implicit_deps(latest_system_packages()),
        ..Default::default()
    };

    let resolved = cfg
        .resolution_graph_for_package(&scratch, None, &mut std::io::sink())
        .with_context(|| format!("resolving git source {url}"))?;

    let probe = Symbol::from("Probe");
    let pkg = resolved.package_table.get(&probe).ok_or_else(|| {
        anyhow!(
            "git source {url} resolved but 'Probe' not in resolution graph (got {} entries)",
            resolved.package_table.len()
        )
    })?;
    Ok(pkg.package_path.clone())
}
