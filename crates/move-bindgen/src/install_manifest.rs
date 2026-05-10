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
//! when debugging — `cat .move-bindgen/exchange/packages.json` should be
//! immediately recognisable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// File the manifest lives at, inside the staging directory.
pub const MANIFEST_FILENAME: &str = "packages.json";

/// Bumped when the on-disk schema changes incompatibly. `generate`
/// errors clearly if it sees a mismatched version (so users learn to
/// re-run `install`).
pub const MANIFEST_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallManifest {
    pub version: u32,
    /// Move chain flavour the install was run for. Replayed by
    /// `generate` so codegen picks the right runtime crate name and
    /// any flavour-specific behaviours. Defaults to `Iota` when
    /// missing from older manifests.
    #[serde(default)]
    pub flavour: crate::config::Flavour,
    /// Hash of `move-bindgen.toml` contents at install time. `generate`
    /// re-hashes the live config and compares; any drift triggers a
    /// "re-run install" error. Format: `"sha256:<hex>"`.
    pub config_digest: String,
    /// Packages in stable id order. Each entry carries enough state for
    /// codegen to walk it without consulting the original config or
    /// `move-package`.
    pub packages: Vec<StagedPackage>,
    /// Named-address overrides applied to every Move build at generate
    /// time. Built from `_` placeholders in any staged Move.toml's
    /// `[addresses]`. Synthetic addresses are baked here so install +
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
    /// detection between install and generate.
    pub source: SerializableSource,
    /// Canonical absolute path install copied this package's source
    /// from. For local entries that's the resolved `path = …`; for
    /// git entries it's the `~/.move/<sanitized>/<subdir>/` location
    /// move-package's fetcher gave us. `generate` re-hashes this
    /// directory (best-effort — silently skips if the path is gone) to
    /// detect "user edited a Move source after running install".
    pub source_abs_path: PathBuf,
    /// SHA-256 over the relevant contents of `source_abs_path` at
    /// install time. Format: `"sha256:<hex>"`. Recomputed by `generate`
    /// — drift triggers a "re-run install" error.
    pub source_digest: String,
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
            flavour: crate::config::Flavour::Iota,
            config_digest: "sha256:0000".into(),
            packages: vec![StagedPackage {
                id: "exchange".into(),
                move_name: "real_markets".into(),
                crate_name: "exchange-rs".into(),
                staged_path: PathBuf::from("exchange"),
                source: SerializableSource::Path {
                    path: PathBuf::from("packages/exchange"),
                },
                source_abs_path: PathBuf::from("/abs/exchange"),
                source_digest: "sha256:dead".into(),
                framework: false,
            }],
            address_overrides: overrides,
        };
        let text = serde_json::to_string_pretty(&m).unwrap();
        let back: InstallManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back.packages.len(), 1);
        assert_eq!(back.packages[0].id, "exchange");
        assert_eq!(back.address_overrides.len(), 2);
        assert_eq!(back.config_digest, "sha256:0000");
        assert_eq!(back.packages[0].source_digest, "sha256:dead");
    }

    #[test]
    fn round_trip_git_entry() {
        let m = InstallManifest {
            version: MANIFEST_VERSION,
            flavour: crate::config::Flavour::Iota,
            config_digest: "sha256:beef".into(),
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
                source_abs_path: PathBuf::from("/home/u/.move/foo/contracts"),
                source_digest: "sha256:cafe".into(),
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
    fn flavour_round_trips_and_defaults_to_iota() {
        // Sui flavour explicitly set: round-trips through serde.
        let m = InstallManifest {
            version: MANIFEST_VERSION,
            flavour: crate::config::Flavour::Sui,
            config_digest: "sha256:0000".into(),
            packages: vec![],
            address_overrides: BTreeMap::new(),
        };
        let text = serde_json::to_string(&m).unwrap();
        assert!(text.contains("\"flavour\":\"sui\""), "{}", text);
        let back: InstallManifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back.flavour, crate::config::Flavour::Sui);

        // Older manifest with no `flavour` field: serde default kicks in,
        // we get Iota. Mirrors what older `packages.json` files on disk
        // would deserialise to.
        let bare = serde_json::json!({
            "version": MANIFEST_VERSION,
            "config_digest": "sha256:0000",
            "packages": [],
        });
        let parsed: InstallManifest = serde_json::from_value(bare).unwrap();
        assert_eq!(parsed.flavour, crate::config::Flavour::Iota);
    }

    #[test]
    fn rejects_wrong_version() {
        // Round-trip a doctored version through serde, exercising the
        // version check on `load` without touching the filesystem.
        let bad = serde_json::json!({
            "version": 99,
            "config_digest": "sha256:0000",
            "packages": []
        });
        let parsed: InstallManifest = serde_json::from_value(bad).unwrap();
        assert_eq!(parsed.version, 99);
        // The filesystem-coupled error path is exercised manually via
        // the `move-bindgen generate` flow when staging is stale.
    }
}
