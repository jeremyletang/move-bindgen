//! Layout helpers for the staging directory: source-tree copying,
//! basename derivation + disambiguation, framework-package detection,
//! and the [`source_key`] dedup key used to fold identical
//! [`PackageSource`]s together.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;

use crate::config::PackageSource;

/// Move packages whose `[addresses]` entries are part of the on-chain
/// framework identity and must NOT be rewritten to the `_` placeholder.
/// Rewriting `iota = "0x2"` → `"_"` makes downstream packages reject
/// `iota::object::UID` as not-from-`iota::object::new`, since the
/// framework ends up compiled at a synthetic `0xff…` address.
///
/// Distinct from `Config::framework_packages` — that one controls
/// codegen routing (skip vs emit-as-peer). The decision here is solely
/// about whether the Move source is part of the canonical framework.
pub fn is_canonical_framework(move_name: &str) -> bool {
    matches!(
        move_name,
        "Iota"
            | "IotaSystem"
            | "MoveStdlib"
            | "Stardust"
            | "Sui"
            | "SuiSystem"
            | "Bridge"
            | "DeepBook"
    )
}

/// Derive the staging directory basename for a package: last path
/// component of its resolved source directory. Falls back to the
/// supplied label if the source has no usable basename (extremely
/// unlikely — implies the source resolves to `/`).
pub(super) fn source_basename(source_root: &Path, fallback: &str) -> String {
    source_root
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// Pick a basename not already in `used`. If `raw` is free, use it
/// verbatim; otherwise append `-2`, `-3`, … until a free slot is
/// found. The counter is open-ended so this never fails.
pub(super) fn unique_basename(raw: &str, used: &BTreeSet<String>) -> String {
    unique_basename_against(raw, used, None)
}

/// Like [`unique_basename`] but also avoids any string in `extra` (the
/// existing entry_id set). Used for auto-discovered entries, whose
/// `entry_id` derives from the chosen basename.
pub(super) fn unique_basename_against(
    raw: &str,
    used: &BTreeSet<String>,
    extra: Option<&BTreeSet<String>>,
) -> String {
    let taken = |s: &str| used.contains(s) || extra.map(|e| e.contains(s)).unwrap_or(false);
    if !taken(raw) {
        return raw.to_string();
    }
    let mut suffix: u32 = 2;
    loop {
        let candidate = format!("{raw}-{suffix}");
        if !taken(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Stable dedup key for a package source. Two work items sharing a key
/// stage the same on-disk directory and are folded together. For local
/// paths the key is the canonical absolute path (so symlinks and
/// trailing-slash variants collapse); for git it's the full
/// `(url, rev, branch, tag, subdir)` tuple.
pub(super) fn source_key(s: &PackageSource) -> String {
    match s {
        PackageSource::Path(p) => {
            let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            format!("path:{}", canonical.display())
        }
        PackageSource::Git {
            url,
            rev,
            branch,
            tag,
            subdir,
        } => format!(
            "git:{url}#{}#{}#{}#{}",
            rev.as_deref().unwrap_or(""),
            branch.as_deref().unwrap_or(""),
            tag.as_deref().unwrap_or(""),
            subdir.as_deref().unwrap_or(""),
        ),
    }
}

/// Recursively copy `src` to `dest`. Skips entries the build doesn't
/// need to see — `build/`, `target/`, `Move.lock`, `.git/` — to keep
/// staging fast and reproducible.
pub(super) fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "build"
            || name_str == "target"
            || name_str == ".git"
            || name_str == "Move.lock"
            // `Published.toml` (Sui's move-package-alt publication file)
            // carries on-chain `published-at` per environment. We strip
            // it so the synthetic-address override is what drives codegen
            // — otherwise sui-move-build hands back the testnet/mainnet
            // address and codegen splits on it.
            || name_str == "Published.toml"
        {
            continue;
        }
        let src_path = entry.path();
        let dest_path = dest.join(&name);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else if ft.is_file() {
            std::fs::copy(&src_path, &dest_path)?;
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&src_path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dest_path)?;
            #[cfg(not(unix))]
            anyhow::bail!("symlink in input not supported on this platform");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_basename_disambiguates() {
        let mut used: BTreeSet<String> = BTreeSet::new();
        assert_eq!(unique_basename("iota-framework", &used), "iota-framework");
        used.insert("iota-framework".into());
        assert_eq!(unique_basename("iota-framework", &used), "iota-framework-2");
        used.insert("iota-framework-2".into());
        assert_eq!(unique_basename("iota-framework", &used), "iota-framework-3");
        // Distinct base unaffected by collisions on a sibling.
        assert_eq!(unique_basename("move-stdlib", &used), "move-stdlib");
    }
}
