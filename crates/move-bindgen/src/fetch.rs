//! `move-bindgen fetch` — populate the staging directory + write a
//! [`FetchManifest`].
//!
//! This is the side-effecting half of the pipeline (network, disk,
//! Move.toml rewrites). [`generate`] then reads the manifest and produces
//! Rust code purely from staging. Decoupling the two means generate runs
//! offline and is repeatable; fetch is the only step that may hit the
//! network.
//!
//! For each `[packages.*]` entry the config lists, fetch:
//! 1. Resolves the source to a local on-disk directory
//!    (`PackageSource::Path` → input-folder lookup; `PackageSource::Git`
//!    → [`crate::resolve_git_source`] which drives `move-package`'s git
//!    fetcher).
//! 2. Copies the source tree to `<staging>/<id>/`, dropping build
//!    artefacts (`build/`, `target/`, `Move.lock`).
//! 3. Rewrites the staged Move.toml's `[addresses]` block, converting
//!    literal `"0x0"` placeholders to `"_"` so the generate step's
//!    address overrides can bind them to unique synthetic values without
//!    conflict.
//! 4. Records the package's identity in [`StagedPackage`] (id, Move
//!    package name, crate name, framework status, source spec).
//!
//! Once every listed package is staged, fetch scans every staged
//! Move.toml's `[addresses]` block for `_`-valued names and assigns each
//! a deterministic synthetic address (`0xff_…`). The resulting map is
//! baked into the manifest so generate replays it identically across
//! runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use move_core_types::account_address::AccountAddress;

use crate::config::{Config, PackageEntry, PackageSource};
use crate::fetch_manifest::{FetchManifest, SerializableSource, StagedPackage, MANIFEST_VERSION};

/// Drive a fetch. `config_path` points at the user's `move-bindgen.toml`;
/// `input_folders` are the bases against which `PackageSource::Path`
/// entries are resolved. Returns the staging root that was populated.
pub fn run(config_path: &Path, input_folders: &[PathBuf]) -> Result<PathBuf> {
    let cfg = Config::load(config_path)?;
    let staging_root = crate::config::staging_dir_for(config_path);

    if staging_root.exists() {
        std::fs::remove_dir_all(&staging_root)
            .with_context(|| format!("clearing {}", staging_root.display()))?;
    }
    std::fs::create_dir_all(&staging_root)
        .with_context(|| format!("creating {}", staging_root.display()))?;

    let mut staged = Vec::with_capacity(cfg.packages.len());
    for entry in &cfg.packages {
        let source_root = resolve_entry_source(&cfg, entry, input_folders, &staging_root)?;
        let dest = staging_root.join(&entry.id);
        copy_dir_recursive(&source_root, &dest)
            .with_context(|| format!("copying {} to staging", source_root.display()))?;
        rewrite_addresses_to_underscore(&dest.join("Move.toml"))?;
        let move_name = read_move_package_name(&dest.join("Move.toml"))?;
        let framework = cfg.framework_packages.contains(&move_name);
        staged.push(StagedPackage {
            id: entry.id.clone(),
            move_name,
            crate_name: entry.crate_name(),
            staged_path: PathBuf::from(&entry.id),
            source: SerializableSource::from_config(&entry.source),
            framework,
        });
    }

    let address_overrides = build_address_overrides(&staging_root, &staged)?;

    let manifest = FetchManifest {
        version: MANIFEST_VERSION,
        packages: staged,
        address_overrides,
    };
    manifest.save(&staging_root)?;

    eprintln!(
        "staged {} package(s) to {}",
        manifest.packages.len(),
        staging_root.display()
    );
    Ok(staging_root)
}

fn resolve_entry_source(
    cfg: &Config,
    entry: &PackageEntry,
    input_folders: &[PathBuf],
    staging_root: &Path,
) -> Result<PathBuf> {
    match &entry.source {
        PackageSource::Path(_) => cfg.resolve_path_source(entry, input_folders),
        PackageSource::Git { .. } => crate::git_resolver::resolve_git_source(
            &entry.source,
            &staging_root.join(".git-probes").join(&entry.id),
        ),
    }
}

/// Walk every staged package's `[addresses]` block, find names whose
/// value is `"_"`, and assign each a deterministic synthetic
/// `0xff_…_NNNN` address. Same name across multiple packages gets the
/// same override (so cross-package refs stay consistent).
fn build_address_overrides(
    staging_root: &Path,
    staged: &[StagedPackage],
) -> Result<BTreeMap<String, String>> {
    let mut overrides: BTreeMap<String, String> = BTreeMap::new();
    let mut next: u128 = 0xff00_0000_0000_0001;
    for pkg in staged {
        let toml_path = staging_root.join(&pkg.staged_path).join("Move.toml");
        let names = read_addresses_block(&toml_path)?;
        for (name, val) in names {
            if val != "_" || overrides.contains_key(&name) {
                continue;
            }
            overrides.insert(name, format_hex_address(synthetic_address(next)));
            next += 1;
        }
    }
    Ok(overrides)
}

fn synthetic_address(n: u128) -> AccountAddress {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&n.to_be_bytes());
    AccountAddress::new(bytes)
}

fn format_hex_address(addr: AccountAddress) -> String {
    addr.to_canonical_string(true)
}

/// Recursively copy `src` to `dest`. Skips entries the build doesn't
/// need to see — `build/`, `target/`, `Move.lock`, `.git/` — to keep
/// staging fast and reproducible.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "build"
            || name_str == "target"
            || name_str == ".git"
            || name_str == "Move.lock"
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

fn rewrite_addresses_to_underscore(move_toml: &Path) -> Result<()> {
    let text = std::fs::read_to_string(move_toml)
        .with_context(|| format!("reading {}", move_toml.display()))?;
    let mut out = String::with_capacity(text.len());
    let mut current_section: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(name) = rest.strip_suffix(']') {
                current_section = Some(name.to_string());
            }
        }
        if current_section.as_deref() == Some("addresses") {
            if let Some(rewritten) = rewrite_zero_address_line(line) {
                out.push_str(&rewritten);
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(move_toml, out).with_context(|| format!("writing {}", move_toml.display()))?;
    Ok(())
}

fn rewrite_zero_address_line(line: &str) -> Option<String> {
    let leading_ws_end = line.find(|c: char| !c.is_whitespace())?;
    let (leading_ws, rest) = line.split_at(leading_ws_end);
    let (key, after_eq) = rest.split_once('=')?;
    let after = after_eq.trim();
    let value_part = after
        .split_once('#')
        .map(|(v, _)| v.trim())
        .unwrap_or(after);
    if value_part != "\"0x0\"" {
        return None;
    }
    Some(format!("{leading_ws}{}= \"_\"", key.trim_end()))
}

fn read_addresses_block(move_toml: &Path) -> Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(move_toml)
        .with_context(|| format!("reading {}", move_toml.display()))?;
    let v: toml::Value =
        toml::from_str(&text).with_context(|| format!("parsing {}", move_toml.display()))?;
    let mut out = BTreeMap::new();
    if let Some(t) = v.get("addresses").and_then(toml::Value::as_table) {
        for (k, val) in t {
            if let Some(s) = val.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok(out)
}

fn read_move_package_name(move_toml: &Path) -> Result<String> {
    let text = std::fs::read_to_string(move_toml)
        .with_context(|| format!("reading {}", move_toml.display()))?;
    let v: toml::Value =
        toml::from_str(&text).with_context(|| format!("parsing {}", move_toml.display()))?;
    v.get("package")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("{} has no [package].name field", move_toml.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_addresses_are_distinct() {
        let a = format_hex_address(synthetic_address(1));
        let b = format_hex_address(synthetic_address(2));
        assert_ne!(a, b);
        assert!(a.starts_with("0xff"));
    }

    #[test]
    fn rewrite_zero_addr_line_basic() {
        assert_eq!(
            rewrite_zero_address_line("counter = \"0x0\""),
            Some("counter = \"_\"".into()),
        );
        assert_eq!(
            rewrite_zero_address_line("    counter = \"0x0\""),
            Some("    counter = \"_\"".into()),
        );
        assert_eq!(rewrite_zero_address_line("counter = \"_\""), None);
        assert_eq!(rewrite_zero_address_line("counter = \"0x100\""), None);
    }
}
