//! Resolve `PackageSource::Git` entries to local on-disk paths via a
//! plain `git clone` + `git checkout`.
//!
//! We deliberately don't recurse into the cloned package's `Move.toml`
//! to chase transitive deps — the downstream build chain
//! (`iota-move-build` or `sui-move-build`) does its own resolution and
//! will reject our pre-fetched copies anyway if it disagrees on
//! versions. Resolving here only causes pain: for workspaces that bind
//! many packages whose own Move.tomls pin conflicting revs of a shared
//! framework, a recursive fetcher trips over "conflicting versions of
//! package X" before any binding work starts.
//!
//! Cache layout mirrors `move-package`'s own scheme:
//! `MOVE_HOME/<sanitized_url>_<rev_label>/<subdir>/`
//! so other tools (and the build chain) hit the same checkout. URL has
//! `/`, `:`, `.`, `@` replaced by `_`; rev replaces `/` with `__`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use move_command_line_common::env::MOVE_HOME;

use crate::config::PackageSource;

/// Resolve a `PackageSource::Git` to a local directory containing
/// `Move.toml`. Cloning is flavour-agnostic — only the bytes on disk
/// matter; the downstream build step is what interprets them.
///
/// `_scratch_root` is kept in the signature for callers, even though
/// the current implementation doesn't need it (the synthetic-Move.toml
/// dance is gone).
pub fn resolve_git_source(
    source: &PackageSource,
    _scratch_root: &Path,
    verbose: bool,
) -> Result<PathBuf> {
    let (url, rev_label, rev, branch, tag, subdir) = match source {
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
            (
                url.as_str(),
                label,
                rev.as_deref(),
                branch.as_deref(),
                tag.as_deref(),
                subdir.as_deref(),
            )
        }
        PackageSource::Path(_) => bail!("resolve_git_source called with a Path source"),
    };

    let cache_root = git_cache_dir(url, rev_label);
    if !cache_root.join(".git").is_dir() {
        clone_into(url, &cache_root, verbose)
            .with_context(|| format!("cloning {url} into {}", cache_root.display()))?;
    }
    // Always checkout — cheap if it's already there, ensures we're on
    // the requested rev when the cache existed but pointed elsewhere.
    let checkout_ref = rev.or(tag).or(branch).expect("validated above");
    checkout(&cache_root, checkout_ref, verbose)
        .with_context(|| format!("checking out {checkout_ref} in {}", cache_root.display()))?;

    let pkg_path = match subdir {
        Some(s) => cache_root.join(s),
        None => cache_root,
    };
    if !pkg_path.join("Move.toml").is_file() {
        bail!(
            "clone succeeded but no Move.toml found at expected location {}",
            pkg_path.display()
        );
    }
    Ok(pkg_path)
}

fn clone_into(url: &str, dest: &Path, verbose: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("creating MOVE_HOME parent dir {}", parent.display())
        })?;
    }
    let stdio = || {
        if verbose {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        }
    };
    let status = Command::new("git")
        .args([
            "clone",
            "--no-checkout",
            "--filter=blob:none",
            url,
            dest.to_str().context("MOVE_HOME path is not UTF-8")?,
        ])
        .stdout(stdio())
        .stderr(stdio())
        .status()
        .context("invoking `git clone`")?;
    if !status.success() {
        bail!("git clone failed with status {status}");
    }
    Ok(())
}

fn checkout(repo: &Path, reference: &str, verbose: bool) -> Result<()> {
    let stdio = || {
        if verbose {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        }
    };
    // `git fetch <ref>` makes sure the rev is available locally even
    // when `clone --filter=blob:none` left the object missing.
    let _ = Command::new("git")
        .args(["fetch", "--quiet", "origin", reference])
        .current_dir(repo)
        .stdout(stdio())
        .stderr(stdio())
        .status();

    let status = Command::new("git")
        .args(["checkout", "--quiet", reference])
        .current_dir(repo)
        .stdout(stdio())
        .stderr(stdio())
        .status()
        .context("invoking `git checkout`")?;
    if !status.success() {
        bail!("git checkout {reference} failed with status {status}");
    }
    Ok(())
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
