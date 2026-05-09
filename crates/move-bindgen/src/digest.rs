//! Content digests over Move source trees + the user's config file.
//!
//! Used to detect "the user changed something between install and
//! generate." Install records digests in `packages.json`; generate
//! recomputes them and errors loudly on drift instead of silently
//! producing stale bindings.
//!
//! Uniform format: `"sha256:<hex>"`. Cheap to compute (well under 100ms
//! for the realmarkets/dex workspace), so we don't bother with caching
//! or per-package incremental hashing.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

const PREFIX: &str = "sha256:";

/// Hash a single file's contents. Returns `"sha256:<hex>"`.
pub fn file_digest(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading {} for digest", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("{PREFIX}{}", hex::encode(h.finalize())))
}

/// Hash the relevant contents of a Move source directory. Returns
/// `"sha256:<hex>"`.
///
/// Walks `root` recursively, hashing each file's path-relative-to-root
/// alongside its contents into a single SHA-256. Output is deterministic
/// across runs (entries are sorted before hashing) but specific to
/// *this* implementation — don't compare digests across different
/// move-bindgen versions.
///
/// Skip set:
///   - `build/`, `target/`, `.git/`: build/VCS noise.
///   - `Move.lock`: re-derivable by move-package; not a source file.
///
/// Symlinked files are followed; symlinked directories are skipped (to
/// avoid loops). The skip + no-symlink-recursion rules mirror what
/// `install`'s `copy_dir_recursive` already does, so the digest covers
/// exactly the set of files that ended up in staging.
pub fn source_dir_digest(root: &Path) -> Result<String> {
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect(&mut entries, root, root)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut h = Sha256::new();
    for (rel, path) in entries {
        h.update(rel.as_bytes());
        h.update([0u8]);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading {} for digest", path.display()))?;
        h.update(&bytes);
        h.update([0u8]);
    }
    Ok(format!("{PREFIX}{}", hex::encode(h.finalize())))
}

/// Recursively gather (relative-path-string, absolute-path) pairs for
/// every file under `dir` we want to hash.
fn collect(out: &mut Vec<(String, std::path::PathBuf)>, root: &Path, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(name_str.as_ref(), "build" | "target" | ".git" | "Move.lock") {
            continue;
        }
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            // Hash symlinked files (read_to_bytes follows the link), but
            // don't recurse into symlinked directories.
            let meta = std::fs::metadata(&path).ok();
            if let Some(m) = meta {
                if m.is_file() {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    out.push((rel, path));
                }
            }
            continue;
        }
        if ft.is_dir() {
            collect(out, root, &path)?;
        } else if ft.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.push((rel, path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(seed: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "move-bindgen-digest-{}-{}",
            seed,
            std::process::id()
        ))
    }

    #[test]
    fn file_digest_matches_known_value() {
        let dir = tmp("file");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.txt");
        std::fs::write(&path, b"hello").unwrap();
        // Known: SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            file_digest(&path).unwrap(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn source_dir_digest_is_deterministic_and_content_sensitive() {
        let dir = tmp("dir");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(dir.join("sources")).unwrap();
        std::fs::write(dir.join("Move.toml"), "[package]\nname=\"X\"\n").unwrap();
        std::fs::write(dir.join("sources/foo.move"), "module X::foo {}").unwrap();
        std::fs::write(dir.join("sources/bar.move"), "module X::bar {}").unwrap();
        let d1 = source_dir_digest(&dir).unwrap();
        let d2 = source_dir_digest(&dir).unwrap();
        assert_eq!(d1, d2, "deterministic across re-hash");

        // Edit a source — digest must change.
        std::fs::write(dir.join("sources/foo.move"), "module X::foo { fun y() {} }").unwrap();
        let d3 = source_dir_digest(&dir).unwrap();
        assert_ne!(d1, d3, "content change must change digest");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn source_dir_digest_skips_build_and_lock() {
        let dir = tmp("skip");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(dir.join("sources")).unwrap();
        std::fs::write(dir.join("sources/m.move"), "module X::m {}").unwrap();
        let baseline = source_dir_digest(&dir).unwrap();

        // Add noise we explicitly skip; digest must not budge.
        std::fs::create_dir_all(dir.join("build/x")).unwrap();
        std::fs::write(dir.join("build/x/junk"), b"noise").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/blob"), b"more noise").unwrap();
        std::fs::write(dir.join("Move.lock"), "lockstuff").unwrap();
        let after = source_dir_digest(&dir).unwrap();
        assert_eq!(baseline, after, "skip set must not affect digest");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn source_dir_digest_distinguishes_path_from_content() {
        // Two trees with the same total bytes but different file layouts
        // should hash differently — proves the relative path is mixed in.
        let a = tmp("layout-a");
        let b = tmp("layout-b");
        for d in [&a, &b] {
            if d.exists() {
                std::fs::remove_dir_all(d).unwrap();
            }
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(a.join("foo"), b"hello").unwrap();
        std::fs::write(b.join("bar"), b"hello").unwrap();
        assert_ne!(
            source_dir_digest(&a).unwrap(),
            source_dir_digest(&b).unwrap(),
            "same content under different filenames must differ"
        );
        std::fs::remove_dir_all(&a).unwrap();
        std::fs::remove_dir_all(&b).unwrap();
    }
}
