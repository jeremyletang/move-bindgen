//! `move-bindgen clean` — remove the staging dir.
//!
//! Useful when you want to force a fresh install (branch-pinned git
//! deps that may have advanced upstream, ad-hoc cache poisoning) or to
//! reclaim disk. Idempotent: succeeds silently if there's nothing to
//! clean.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::staging_dir_for;

/// Outcome of [`run`] — telling the caller whether anything was
/// actually removed lets the CLI emit a "Removed X" vs
/// "Nothing to clean" status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanOutcome {
    /// `path` was a directory and is now gone.
    Removed(PathBuf),
    /// Nothing was at `path`; staging was already absent.
    NotFound(PathBuf),
}

/// Remove the staging directory associated with `config_path`. The
/// staging dir layout is owned by [`staging_dir_for`]; this is the
/// only operation that deletes it.
pub fn run(config_path: &Path) -> Result<CleanOutcome> {
    let staging_root = staging_dir_for(config_path);
    if !staging_root.exists() {
        return Ok(CleanOutcome::NotFound(staging_root));
    }
    std::fs::remove_dir_all(&staging_root)
        .with_context(|| format!("removing {}", staging_root.display()))?;
    Ok(CleanOutcome::Removed(staging_root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(seed: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "move-bindgen-clean-{}-{}",
            seed,
            std::process::id()
        ))
    }

    #[test]
    fn clean_removes_existing_staging_dir() {
        let root = tmp("removes");
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        let cfg = root.join("move-bindgen.toml");
        std::fs::write(&cfg, "stub").unwrap();
        // staging_dir_for(<cfg>) -> <root>/.move-bindgen/default/ for
        // the canonical `move-bindgen.toml`. Materialise it.
        let staging = staging_dir_for(&cfg);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("packages.json"), "{}").unwrap();

        let outcome = run(&cfg).unwrap();
        assert!(matches!(outcome, CleanOutcome::Removed(p) if p == staging));
        assert!(!staging.exists(), "staging dir is gone");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn clean_succeeds_when_nothing_is_staged() {
        let root = tmp("absent");
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        let cfg = root.join("move-bindgen.toml");
        std::fs::write(&cfg, "stub").unwrap();

        let outcome = run(&cfg).unwrap();
        let staging = staging_dir_for(&cfg);
        assert!(matches!(outcome, CleanOutcome::NotFound(p) if p == staging));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
