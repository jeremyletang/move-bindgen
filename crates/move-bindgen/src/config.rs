//! `move-bindgen.toml` schema, loader, and validator.
//!
//! The config drives codegen: which Move packages to bind, what shape the
//! output takes (single crate vs. workspace of crates), and how the runtime
//! is referenced. Output **location** is decided by the CLI (`--output` flag
//! or by sitting alongside the config file) — the config itself is purely
//! about *content*.
//!
//! Loading is split into [`RawConfig`] (1:1 TOML deserialisation) and
//! [`Config`] (validated + path-resolved + ready to feed into codegen).
//! Most users only see `Config::load` and the `Config` accessors.

mod flavour;
mod package;
mod publish;
mod runtime;
mod staging;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

pub use self::flavour::Flavour;
pub use self::package::{default_crate_name, PackageEntry, PackageSource};
pub use self::publish::{PublishConfig, PublishNetwork};
pub use self::runtime::{RuntimeSpec, DEFAULT_RUNTIME_GIT_URL};
pub use self::staging::{config_path_in, staging_dir_for};

use self::package::{validate_unique_crate_names, validate_unique_sources, RawPackage};
use self::publish::RawPublish;
use self::runtime::RawRuntime;

const CONFIG_FILE_NAME: &str = "move-bindgen.toml";

/// Move package names skipped at codegen time and routed through
/// `move-bindgen-runtime` re-exports instead.
///
/// Empty by default — the runtime only re-exports a handful of types
/// (`UID`, `ID`, …) and most real Move packages reference framework
/// types it *doesn't* cover (`Coin`, `Balance`, `Table`, …). Letting
/// codegen emit the framework crates as peers is what works out of
/// the box; users who want the smaller output can opt back in via
/// `framework_packages = ["Iota", "MoveStdlib"]` in `move-bindgen.toml`.
pub const DEFAULT_FRAMEWORK_PACKAGES: &[&str] = &[];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    /// One Move package → one Rust crate.
    SingleCrate,
    /// Many Move packages → a Cargo workspace of crates, one per package.
    Workspace,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory the config was loaded from. Used as the default output
    /// location and as the base for resolving runtime paths.
    pub config_dir: PathBuf,
    /// Move chain flavour — which build chain, SDK family, and runtime
    /// crate the generated code targets. Defaults to `Iota`.
    pub flavour: Flavour,
    pub format: OutputFormat,
    /// Output directory name. For workspace mode, required in the TOML.
    /// For single-crate mode, defaults to `<input-dir basename>-rs` if not
    /// set explicitly.
    pub output_name: Option<String>,
    pub runtime: RuntimeSpec,
    /// Move package names that are already covered by the runtime — skip
    /// codegen for these and route their types through runtime re-exports.
    pub framework_packages: BTreeSet<String>,
    /// All packages we generate bindings for. For `SingleCrate`, exactly
    /// one entry. For `Workspace`, one or more.
    pub packages: Vec<PackageEntry>,
    /// `[publish]` block. Drives generation of deployable bytecode +
    /// `Package::deployer(...)` per target network. Empty `networks` =
    /// silently skip; no deploy API is emitted.
    pub publish: PublishConfig,
}

impl Config {
    /// Load and validate the config at `path` (typically `move-bindgen.toml`).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&text)
            .with_context(|| format!("parsing config at {}", path.display()))?;
        let config_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::from_raw(raw, config_dir)
    }

    /// Path-only variant. Errors if the entry is git-sourced.
    pub fn resolve_path_source(
        &self,
        entry: &PackageEntry,
        input_folders: &[PathBuf],
    ) -> Result<PathBuf> {
        let raw_path = match &entry.source {
            PackageSource::Path(p) => p,
            PackageSource::Git { .. } => {
                bail!(
                    "package '{}' has a git source — resolve via the git resolver, not resolve_path_source",
                    entry.id
                );
            }
        };
        let no_input_dir = input_folders.is_empty();
        let candidates: Vec<PathBuf> = if no_input_dir {
            vec![self.config_dir.clone()]
        } else {
            input_folders.to_vec()
        };
        let mut tried = Vec::new();
        for base in &candidates {
            let candidate = base.join(raw_path);
            if candidate.join("Move.toml").is_file() {
                return Ok(candidate);
            }
            tried.push(candidate);
        }
        let attempts = tried
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        let mut msg = format!(
            "package '{}' not found — looked for Move.toml at:\n{}",
            entry.id, attempts
        );
        if no_input_dir {
            msg.push_str(
                "\n\nhint: no --input-dir was provided; the path was resolved against the\n\
                 config's directory. If your [packages.*].path entries are relative to a\n\
                 different base (e.g. an external Move repo checkout), pass\n\
                 --input-dir <path> pointing to that base.",
            );
        }
        Err(anyhow!(msg))
    }

    fn from_raw(raw: RawConfig, config_dir: PathBuf) -> Result<Self> {
        let format = match raw.output.format.as_str() {
            "single-crate" => OutputFormat::SingleCrate,
            "workspace" => OutputFormat::Workspace,
            other => bail!("[output].format must be 'single-crate' or 'workspace', got '{other}'"),
        };

        let runtime = RuntimeSpec::from_raw(raw.output.runtime).context("[output].runtime")?;

        let framework_packages = raw
            .framework_packages
            .unwrap_or_else(|| {
                DEFAULT_FRAMEWORK_PACKAGES
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            })
            .into_iter()
            .collect::<BTreeSet<_>>();

        let mut packages = Vec::new();
        match format {
            OutputFormat::SingleCrate => {
                if raw.package.is_none() {
                    bail!(
                        "[output].format = 'single-crate' requires a `[package]` section (singular)"
                    );
                }
                if !raw.packages.is_empty() {
                    bail!(
                        "[output].format = 'single-crate' is incompatible with `[packages.*]` entries (use `[package]` instead, or switch to format = 'workspace')"
                    );
                }
                let p = raw.package.unwrap();
                let source = PackageSource::from_raw("package", &p)?;
                packages.push(PackageEntry {
                    id: "package".to_string(),
                    source,
                    crate_name_override: p.crate_name,
                });
            }
            OutputFormat::Workspace => {
                if raw.package.is_some() {
                    bail!(
                        "[output].format = 'workspace' is incompatible with a `[package]` section (use `[packages.*]` entries instead)"
                    );
                }
                if raw.packages.is_empty() {
                    bail!(
                        "[output].format = 'workspace' requires at least one `[packages.<id>]` entry"
                    );
                }
                for (id, raw_pkg) in raw.packages {
                    let source = PackageSource::from_raw(&id, &raw_pkg)?;
                    packages.push(PackageEntry {
                        id,
                        source,
                        crate_name_override: raw_pkg.crate_name,
                    });
                }
                packages.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }

        validate_unique_sources(&packages)?;
        validate_unique_crate_names(&packages)?;

        // Workspace mode has no obvious default for the output dir name —
        // require it explicitly. Single-crate mode falls back to the
        // generated crate's name (matches `default_crate_name`).
        if format == OutputFormat::Workspace && raw.output.name.is_none() {
            bail!("[output].name is required when format = 'workspace'");
        }

        let publish = PublishConfig::from_raw(raw.publish.unwrap_or_default())?;

        Ok(Config {
            config_dir,
            flavour: raw.flavour.unwrap_or_default(),
            format,
            output_name: raw.output.name,
            runtime,
            framework_packages,
            packages,
            publish,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    output: RawOutput,
    #[serde(default)]
    flavour: Option<Flavour>,
    #[serde(default)]
    framework_packages: Option<Vec<String>>,
    #[serde(default)]
    package: Option<RawPackage>,
    #[serde(default)]
    packages: BTreeMap<String, RawPackage>,
    #[serde(default)]
    publish: Option<RawPublish>,
}

#[derive(Debug, Deserialize)]
struct RawOutput {
    format: String,
    #[serde(default)]
    name: Option<String>,
    runtime: RawRuntime,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(s).context("parsing test config")?;
        Config::from_raw(raw, PathBuf::from("/test"))
    }

    #[test]
    fn workspace_minimal_loads() {
        let cfg = parse(
            r#"
            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            path = "packages/foo"

            [packages.bar]
            path = "packages/bar"
            crate_name = "barbaz"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.format, OutputFormat::Workspace);
        assert_eq!(cfg.packages.len(), 2);
        assert_eq!(cfg.packages[0].id, "bar");
        assert_eq!(cfg.packages[0].crate_name(), "barbaz");
        assert_eq!(cfg.packages[1].id, "foo");
        assert_eq!(cfg.packages[1].crate_name(), "foo-rs");
        assert_eq!(cfg.flavour, Flavour::Iota, "flavour defaults to Iota");
        assert_eq!(
            cfg.framework_packages,
            DEFAULT_FRAMEWORK_PACKAGES
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert!(
            cfg.publish.networks.is_empty(),
            "missing [publish] defaults to empty networks (silent skip)"
        );
    }

    #[test]
    fn publish_block_flows_through_config_load() {
        let cfg = parse(
            r#"
            [output]
            format  = "workspace"
            name    = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            path = "packages/foo"

            [publish]
            networks = [
                "testnet",
                "mainnet",
                { name = "localnet", addresses = { iota = "0x2" } },
            ]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.publish.networks.len(), 3);
        assert_eq!(cfg.publish.networks[0].name, "testnet");
        assert_eq!(cfg.publish.networks[2].name, "localnet");
        assert_eq!(cfg.publish.networks[2].addresses.len(), 1);
    }

    #[test]
    fn flavour_field_round_trips_iota() {
        let cfg = parse(
            r#"
            flavour = "iota"

            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            path = "packages/foo"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.flavour, Flavour::Iota);
    }

    #[test]
    fn flavour_field_round_trips_sui() {
        let cfg = parse(
            r#"
            flavour = "sui"

            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            path = "packages/foo"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.flavour, Flavour::Sui);
    }

    #[test]
    fn flavour_field_rejects_unknown() {
        let err = parse(
            r#"
            flavour = "ethereum"

            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            path = "packages/foo"
            "#,
        )
        .unwrap_err();
        // serde's default error for an unknown unit-variant; just
        // confirm we surface it as a load failure rather than silently
        // accepting unknown values.
        assert!(
            format!("{err:#}").contains("unknown variant")
                || format!("{err:#}").contains("flavour"),
            "expected an unknown-flavour error, got: {err:#}",
        );
    }

    #[test]
    fn single_crate_minimal_loads() {
        let cfg = parse(
            r#"
            [output]
            format = "single-crate"
            name = "counter-rs"
            runtime = { path = "../runtime" }

            [package]
            path = "."
            "#,
        )
        .unwrap();
        assert_eq!(cfg.format, OutputFormat::SingleCrate);
        assert_eq!(cfg.output_name.as_deref(), Some("counter-rs"));
        assert_eq!(cfg.packages.len(), 1);
    }

    #[test]
    fn workspace_rejects_singular_package() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            runtime = { path = "../runtime" }

            [package]
            path = "."
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("incompatible with a `[package]`"),
            "{err}"
        );
    }

    #[test]
    fn single_crate_rejects_plural_packages() {
        // Both `[package]` and `[packages.*]` set — the second-stage check
        // should fire and reject the combination.
        let err = parse(
            r#"
            [output]
            format = "single-crate"
            runtime = { path = "../runtime" }

            [package]
            path = "."

            [packages.foo]
            path = "x"
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("incompatible with `[packages.*]`"), "{msg}");
    }

    #[test]
    fn duplicate_paths_rejected() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.a]
            path = "x"

            [packages.b]
            path = "x"
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("both point at the same source"), "{msg}");
    }

    #[test]
    fn package_must_have_path_or_git() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            crate_name = "foo-rs"
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("must specify either `path` or `git`"), "{msg}");
    }

    #[test]
    fn package_rejects_path_and_git_together() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.foo]
            path = "foo"
            git  = "https://example.com/foo.git"
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("multiple sources"), "{msg}");
    }

    #[test]
    fn package_git_loads() {
        let cfg = parse(
            r#"
            [output]
            format = "workspace"
            name   = "ws"
            runtime = { path = "../runtime" }

            [packages.pyth]
            git    = "https://github.com/pyth-network/pyth-crosschain.git"
            rev    = "iota-contract-testnet"
            subdir = "target_chains/sui/contracts"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.packages.len(), 1);
        match &cfg.packages[0].source {
            PackageSource::Git {
                url, rev, subdir, ..
            } => {
                assert_eq!(url, "https://github.com/pyth-network/pyth-crosschain.git");
                assert_eq!(rev.as_deref(), Some("iota-contract-testnet"));
                assert_eq!(subdir.as_deref(), Some("target_chains/sui/contracts"));
            }
            other => panic!("expected Git source, got {other:?}"),
        }
        // Default crate name = kebab(basename of subdir) + -rs.
        assert_eq!(cfg.packages[0].crate_name(), "contracts-rs");
    }

    #[test]
    fn duplicate_crate_names_rejected() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            runtime = { path = "../runtime" }

            [packages.a]
            path = "foo"
            crate_name = "shared-rs"

            [packages.b]
            path = "bar"
            crate_name = "shared-rs"
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("would both produce crate 'shared-rs'"),
            "{err}"
        );
    }

    #[test]
    fn runtime_must_have_one_source() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            runtime = {}

            [packages.foo]
            path = "."
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("must set one of"), "{msg}");
    }

    #[test]
    fn runtime_rejects_multiple_sources() {
        let err = parse(
            r#"
            [output]
            format = "workspace"
            runtime = { path = "x", git = "https://example.com/x" }

            [packages.foo]
            path = "."
            "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("multiple sources"), "{msg}");
    }

    #[test]
    fn runtime_version_string_form_works() {
        let cfg = parse(
            r#"
            [output]
            format = "workspace"
            name   = "ws"
            runtime = "0.1"

            [packages.foo]
            path = "."
            "#,
        )
        .unwrap();
        assert_eq!(cfg.runtime, RuntimeSpec::Version("0.1".into()));
    }
}
