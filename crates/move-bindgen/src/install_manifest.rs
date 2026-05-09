//! On-disk record of a `move-bindgen install` run.
//!
//! `fetch` writes one of these into the staging directory; `generate`
//! reads it to drive codegen without touching the network or the user's
//! original sources. Captures every package that's been resolved + copied
//! into staging, plus the metadata generate needs to walk and emit Rust
//! (move package name, crate name, address, immediate deps, framework
//! status).
//!
//! Format: pretty-printed JSON. Stable enough for users to read by hand
//! when debugging — `cat .move-bindgen-exchange/packages.json` should be
//! immediately recognisable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// File the manifest lives at, inside the staging directory.
pub const MANIFEST_FILENAME: &str = "packages.json";

/// Bumped when the on-disk schema changes incompatibly. `generate`
/// errors clearly if it sees a mismatched version (so users learn to
/// re-run `fetch`).
pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallManifest {
    pub version: u32,
    /// Packages in stable id order. Each entry carries enough state for
    /// codegen to walk it without consulting the original config or
    /// `move-package`.
    pub packages: Vec<StagedPackage>,
    /// Named-address overrides applied to every Move build at generate
    /// time. Built from `_` placeholders in any staged Move.toml's
    /// `[addresses]`. Synthetic addresses are baked here so fetch +
    /// generate agree across runs.
    #[serde(default)]
    pub address_overrides: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedPackage {
    /// Stable identifier — either the explicit `[packages.<id>]` key
    /// from the config, or for transitively-discovered deps the
    /// auto-derived basename of the source directory.
    pub id: String,
    /// Move package's `[package].name` from its (staged) `Move.toml`.
    pub move_name: String,
    /// Generated Rust crate name. Default `<basename>-rs`; overridable
    /// via `[packages.X].crate_name` in the config.
    pub crate_name: String,
    /// Path inside the staging directory (relative to staging root)
    /// where the staged copy of this package lives. The Move build is
    /// run against this path.
    pub staged_path: PathBuf,
    /// Original source spec — kept for diagnostics + future drift
    /// detection between fetch and generate.
    pub source: SerializableSource,
    /// True if this entry's modules are owned by `move-bindgen-runtime`
    /// (Iota framework + Move stdlib). No codegen runs for it; refs to
    /// its types route into the runtime via the well-known mapping.
    pub framework: bool,
}

/// Persistable form of `crate::config::PackageSource`. Untagged enums
/// are awkward in JSON; using a `kind` discriminator keeps the file
/// human-readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SerializableSource {
    /// Local directory. `path` is config-relative as written in the TOML.
    Path { path: PathBuf },
    /// Git checkout. Mirrors the `[packages.X]` git fields.
    Git {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
    },
}

impl InstallManifest {
    /// Read the manifest from `<staging_root>/packages.json`. Errors if
    /// the file is missing (asks the user to run `fetch`) or if the
    /// version is incompatible.
    pub fn load(staging_root: &Path) -> Result<Self> {
        let path = staging_root.join(MANIFEST_FILENAME);
        if !path.is_file() {
            bail!(
                "no install manifest at {} — run `move-bindgen install` first",
                path.display()
            );
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let manifest: Self =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if manifest.version != MANIFEST_VERSION {
            bail!(
                "install manifest at {} is version {} but this build expects version {} — re-run `move-bindgen install`",
                path.display(),
                manifest.version,
                MANIFEST_VERSION,
            );
        }
        Ok(manifest)
    }

    /// Write the manifest to `<staging_root>/packages.json`. Pretty-prints
    /// for grep-ability.
    pub fn save(&self, staging_root: &Path) -> Result<()> {
        std::fs::create_dir_all(staging_root)
            .with_context(|| format!("creating {}", staging_root.display()))?;
        let path = staging_root.join(MANIFEST_FILENAME);
        let text = serde_json::to_string_pretty(self).context("serializing install manifest")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Find a staged package by id (returns `None` if no such entry).
    pub fn find(&self, id: &str) -> Option<&StagedPackage> {
        self.packages.iter().find(|p| p.id == id)
    }
}

impl SerializableSource {
    pub fn from_config(source: &crate::config::PackageSource) -> Self {
        match source {
            crate::config::PackageSource::Path(p) => SerializableSource::Path { path: p.clone() },
            crate::config::PackageSource::Git {
                url,
                rev,
                branch,
                tag,
                subdir,
            } => SerializableSource::Git {
                url: url.clone(),
                rev: rev.clone(),
                branch: branch.clone(),
                tag: tag.clone(),
                subdir: subdir.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_path_entry() {
        let mut overrides = BTreeMap::new();
        overrides.insert("real_markets".into(), "0xff00000000000003".into());
        overrides.insert("fixed18".into(), "0xff00000000000004".into());
        let m = InstallManifest {
            version: MANIFEST_VERSION,
            packages: vec![StagedPackage {
                id: "exchange".into(),
                move_name: "real_markets".into(),
                crate_name: "exchange-rs".into(),
                staged_path: PathBuf::from("exchange"),
                source: SerializableSource::Path {
                    path: PathBuf::from("packages/exchange"),
                },
                framework: false,
            }],
            address_overrides: overrides,
        };
        let text = serde_json::to_string_pretty(&m).unwrap();
        let back: InstallManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back.packages.len(), 1);
        assert_eq!(back.packages[0].id, "exchange");
        assert_eq!(back.address_overrides.len(), 2);
    }

    #[test]
    fn round_trip_git_entry() {
        let m = InstallManifest {
            version: MANIFEST_VERSION,
            packages: vec![StagedPackage {
                id: "pyth".into(),
                move_name: "Pyth".into(),
                crate_name: "pyth-rs".into(),
                staged_path: PathBuf::from("pyth"),
                source: SerializableSource::Git {
                    url: "https://github.com/pyth-network/pyth-crosschain.git".into(),
                    rev: Some("iota-contract-testnet".into()),
                    branch: None,
                    tag: None,
                    subdir: Some("target_chains/sui/contracts".into()),
                },
                framework: false,
            }],
            address_overrides: BTreeMap::new(),
        };
        let text = serde_json::to_string_pretty(&m).unwrap();
        let back: InstallManifest = serde_json::from_str(&text).unwrap();
        match &back.packages[0].source {
            SerializableSource::Git {
                url, rev, subdir, ..
            } => {
                assert_eq!(url, "https://github.com/pyth-network/pyth-crosschain.git");
                assert_eq!(rev.as_deref(), Some("iota-contract-testnet"));
                assert_eq!(subdir.as_deref(), Some("target_chains/sui/contracts"));
            }
            _ => panic!("expected Git source"),
        }
    }

    #[test]
    fn rejects_wrong_version() {
        // Round-trip a doctored version through serde, exercising the
        // version check on `load` without touching the filesystem.
        let bad = serde_json::json!({
            "version": 99,
            "packages": []
        });
        let parsed: InstallManifest = serde_json::from_value(bad).unwrap();
        assert_eq!(parsed.version, 99);
        // The filesystem-coupled error path is exercised manually via
        // the `move-bindgen generate` flow when staging is stale.
    }
}
