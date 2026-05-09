//! Resolve `PackageSource::Git` entries to local on-disk paths via
//! `move-package`'s git fetcher.
//!
//! No clone logic of our own — we lean on
//! `MoveBuildConfig::download_deps_for_package`, which already handles
//! fetching, caching (under `~/.move/`), locking, and rev/branch/tag
//! resolution. Subsequent calls for the same `(url, rev)` hit the cache
//! and return immediately.
//!
//! Why not `resolution_graph_for_package`? That entry runs the *full*
//! resolution and enforces "the alias name in `[dependencies.X]` must
//! match `[package].name` in the target's Move.toml". We don't know the
//! target's name yet (that's what we're fetching to find out), so we'd
//! always fail the check. `download_deps_for_package` skips the check —
//! it just walks the manifest and downloads whatever it finds.
//!
//! After fetching, we compute the local path using the same scheme
//! `move-package` uses internally:
//! `MOVE_HOME/<sanitized_url>_<rev>/<subdir>/`
//! where the URL has `/`, `:`, `.`, `@` replaced by `_` and the rev has
//! `/` replaced by `__`. The path is documented and stable; we just
//! mirror it.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use iota_move_build::IotaPackageHooks;
use iota_package_management::system_package_versions::latest_system_packages;
use move_command_line_common::env::MOVE_HOME;
use move_package::package_hooks::register_package_hooks;
use move_package::BuildConfig as MoveBuildConfig;

use crate::config::PackageSource;

/// Resolve a `PackageSource::Git` to a local directory containing
/// `Move.toml`. `scratch_root` is a writeable dir where the synthetic
/// probe manifest lives (used to drive the fetcher). Pass any
/// disposable directory.
pub fn resolve_git_source(source: &PackageSource, scratch_root: &Path) -> Result<PathBuf> {
    let (url, rev_label, subdir) = match source {
        PackageSource::Git {
            url,
            rev,
            branch,
            tag,
            subdir,
        } => {
            let label = rev
                .as_deref()
                .or(branch.as_deref())
                .or(tag.as_deref())
                .ok_or_else(|| {
                    anyhow!("git source for {url} must set one of rev / branch / tag")
                })?;
            (url.as_str(), label, subdir.as_deref())
        }
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

    let manifest = synthetic_manifest(source);
    let manifest_path = scratch.join("Move.toml");
    std::fs::write(&manifest_path, manifest)
        .with_context(|| format!("writing {}", manifest_path.display()))?;

    let cfg = MoveBuildConfig {
        implicit_dependencies: iota_move_build::implicit_deps(latest_system_packages()),
        ..Default::default()
    };

    cfg.download_deps_for_package(&scratch, &mut std::io::sink())
        .with_context(|| format!("fetching git source {url}"))?;

    // Compute the local path move-package would have placed the dep at.
    let cache_root = git_cache_dir(url, rev_label);
    let pkg_path = match subdir {
        Some(s) => cache_root.join(s),
        None => cache_root,
    };

    if !pkg_path.join("Move.toml").is_file() {
        bail!(
            "fetch succeeded but no Move.toml found at expected location {}",
            pkg_path.display()
        );
    }
    Ok(pkg_path)
}

/// Mirror of `move_package::resolution::repository_path` for git deps:
/// `<MOVE_HOME>/<sanitized_url>_<rev_label>/`. Sanitization replaces
/// `/`, `:`, `.`, `@` with `_` (URL-safe path component); rev replaces
/// `/` with `__` (so e.g. `refs/heads/main` doesn't add a directory
/// boundary).
fn git_cache_dir(url: &str, rev_label: &str) -> PathBuf {
    let mut name = String::with_capacity(url.len() + rev_label.len() + 1);
    for c in url.chars() {
        match c {
            '/' | ':' | '.' | '@' => name.push('_'),
            other => name.push(other),
        }
    }
    name.push('_');
    for c in rev_label.chars() {
        if c == '/' {
            name.push_str("__");
        } else {
            name.push(c);
        }
    }
    PathBuf::from(MOVE_HOME.as_str()).join(name)
}

fn synthetic_manifest(source: &PackageSource) -> String {
    // The dep is aliased as `Probe` here; we deliberately don't run
    // resolution (which would name-check), only the fetch step (which
    // doesn't). The alias never appears anywhere except inside this
    // throwaway file.
    let mut s = String::new();
    s.push_str(
        "[package]\n\
         name = \"MoveBindgenGitProbe\"\n\
         edition = \"2024\"\n\
         \n\
         [addresses]\n\
         move_bindgen_git_probe = \"0x0\"\n\
         \n\
         [dependencies.Probe]\n",
    );
    if let PackageSource::Git {
        url,
        rev,
        branch,
        tag,
        subdir,
    } = source
    {
        s.push_str(&format!("git = \"{url}\"\n"));
        if let Some(v) = rev {
            s.push_str(&format!("rev = \"{v}\"\n"));
        }
        if let Some(v) = branch {
            s.push_str(&format!("branch = \"{v}\"\n"));
        }
        if let Some(v) = tag {
            s.push_str(&format!("tag = \"{v}\"\n"));
        }
        if let Some(v) = subdir {
            s.push_str(&format!("subdir = \"{v}\"\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_matches_move_package_layout() {
        let dir = git_cache_dir(
            "https://github.com/pyth-network/pyth-crosschain.git",
            "main",
        );
        let last = dir.file_name().and_then(|s| s.to_str()).unwrap();
        assert_eq!(
            last,
            "https___github_com_pyth-network_pyth-crosschain_git_main"
        );
    }
}
