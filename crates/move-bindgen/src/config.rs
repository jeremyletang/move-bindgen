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

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

const CONFIG_FILE_NAME: &str = "move-bindgen.toml";

/// Default Move package names whose types are already covered by
/// `move-bindgen-runtime` (`UID`, `ID`, `Option`, `String`, …). Skipped at
/// codegen time and routed through the runtime re-exports instead.
pub const DEFAULT_FRAMEWORK_PACKAGES: &[&str] = &["Iota", "MoveStdlib"];

// -----------------------------------------------------------------------------
// Public, validated config
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputFormat {
    /// One Move package → one Rust crate.
    SingleCrate,
    /// Many Move packages → a Cargo workspace of crates, one per package.
    Workspace,
}

/// How the generated code should reference `move-bindgen-runtime`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeSpec {
    /// `path = "..."` (resolved relative to the config file).
    Path(PathBuf),
    /// `git = "..." [, rev = "..."]`.
    Git {
        url: String,
        rev: Option<String>,
        branch: Option<String>,
        tag: Option<String>,
    },
    /// `version = "..."` (crates.io).
    Version(String),
}

#[derive(Debug, Clone)]
pub struct PackageEntry {
    /// Stable identifier from the `[packages.<id>]` key. Used in error
    /// messages; not necessarily the Move package name.
    pub id: String,
    /// Path as written in the TOML — relative, unresolved.
    pub raw_path: PathBuf,
    /// Override for the generated crate name. Default: kebab(basename of
    /// `raw_path`) + `"-rs"`.
    pub crate_name_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory the config was loaded from. Used as the default output
    /// location and as the base for resolving runtime paths.
    pub config_dir: PathBuf,
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

    /// Resolve a `[packages.*].raw_path` against the supplied input
    /// folders. First match wins. Errors if none of them contain a
    /// `Move.toml` at the given relative path.
    pub fn resolve_package_path(
        &self,
        entry: &PackageEntry,
        input_folders: &[PathBuf],
    ) -> Result<PathBuf> {
        let candidates: Vec<PathBuf> = if input_folders.is_empty() {
            vec![self.config_dir.clone()]
        } else {
            input_folders.to_vec()
        };
        let mut tried = Vec::new();
        for base in &candidates {
            let candidate = base.join(&entry.raw_path);
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
        Err(anyhow!(
            "package '{}' not found — looked for Move.toml at:\n{}",
            entry.id,
            attempts
        ))
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
                packages.push(PackageEntry {
                    id: "package".to_string(),
                    raw_path: PathBuf::from(p.path),
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
                    packages.push(PackageEntry {
                        id,
                        raw_path: PathBuf::from(raw_pkg.path),
                        crate_name_override: raw_pkg.crate_name,
                    });
                }
                packages.sort_by(|a, b| a.id.cmp(&b.id));
            }
        }

        validate_unique_paths(&packages)?;
        validate_unique_crate_names(&packages)?;

        // Workspace mode has no obvious default for the output dir name —
        // require it explicitly. Single-crate mode falls back to the
        // generated crate's name (matches `default_crate_name`).
        if format == OutputFormat::Workspace && raw.output.name.is_none() {
            bail!("[output].name is required when format = 'workspace'");
        }

        Ok(Config {
            config_dir,
            format,
            output_name: raw.output.name,
            runtime,
            framework_packages,
            packages,
        })
    }
}

/// Default crate name for a package: kebab-cased basename of `raw_path` +
/// `"-rs"`. Falls back to `package-rs` if the path has no usable basename.
pub fn default_crate_name(raw_path: &Path) -> String {
    let stem = raw_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("package");
    let kebab: String = stem
        .chars()
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    format!("{kebab}-rs")
}

impl PackageEntry {
    /// Effective crate name: explicit override if set, otherwise
    /// [`default_crate_name`].
    pub fn crate_name(&self) -> String {
        self.crate_name_override
            .clone()
            .unwrap_or_else(|| default_crate_name(&self.raw_path))
    }
}

// -----------------------------------------------------------------------------
// Raw (TOML-backing) types
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawConfig {
    output: RawOutput,
    #[serde(default)]
    framework_packages: Option<Vec<String>>,
    #[serde(default)]
    package: Option<RawPackage>,
    #[serde(default)]
    packages: BTreeMap<String, RawPackage>,
}

#[derive(Debug, Deserialize)]
struct RawOutput {
    format: String,
    #[serde(default)]
    name: Option<String>,
    runtime: RawRuntime,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawRuntime {
    Inline {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        git: Option<String>,
        #[serde(default)]
        rev: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        version: Option<String>,
    },
    Version(String),
}

impl RuntimeSpec {
    fn from_raw(raw: RawRuntime) -> Result<Self> {
        match raw {
            RawRuntime::Version(v) => Ok(RuntimeSpec::Version(v)),
            RawRuntime::Inline {
                path,
                git,
                rev,
                branch,
                tag,
                version,
            } => {
                let kinds = [
                    path.is_some().then_some("path"),
                    git.is_some().then_some("git"),
                    version.is_some().then_some("version"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                if kinds.is_empty() {
                    bail!("runtime spec must set one of `path`, `git`, or `version`");
                }
                if kinds.len() > 1 {
                    bail!(
                        "runtime spec sets multiple sources ({}); pick exactly one",
                        kinds.join(", ")
                    );
                }
                if let Some(p) = path {
                    Ok(RuntimeSpec::Path(PathBuf::from(p)))
                } else if let Some(url) = git {
                    Ok(RuntimeSpec::Git {
                        url,
                        rev,
                        branch,
                        tag,
                    })
                } else {
                    Ok(RuntimeSpec::Version(version.unwrap()))
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawPackage {
    path: String,
    #[serde(default)]
    crate_name: Option<String>,
}

// -----------------------------------------------------------------------------
// Validation helpers
// -----------------------------------------------------------------------------

fn validate_unique_paths(packages: &[PackageEntry]) -> Result<()> {
    let mut seen: BTreeMap<&Path, &str> = BTreeMap::new();
    for p in packages {
        if let Some(prev) = seen.insert(p.raw_path.as_path(), &p.id) {
            bail!(
                "packages '{}' and '{}' both point at path '{}'",
                prev,
                p.id,
                p.raw_path.display()
            );
        }
    }
    Ok(())
}

fn validate_unique_crate_names(packages: &[PackageEntry]) -> Result<()> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for p in packages {
        let name = p.crate_name();
        if let Some(prev) = seen.insert(name.clone(), &p.id) {
            bail!(
                "packages '{}' and '{}' would both produce crate '{}'; set `crate_name` on one to disambiguate",
                prev,
                p.id,
                name
            );
        }
    }
    Ok(())
}

/// Convenience for callers that have a directory and want to find the
/// canonical config file inside it.
pub fn config_path_in(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE_NAME)
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
        assert_eq!(
            cfg.framework_packages,
            DEFAULT_FRAMEWORK_PACKAGES
                .iter()
                .map(|s| s.to_string())
                .collect()
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
            runtime = { path = "../runtime" }

            [packages.a]
            path = "x"

            [packages.b]
            path = "x"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("both point at path"), "{err}");
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

    #[test]
    fn default_crate_name_kebabs_underscores() {
        assert_eq!(
            default_crate_name(Path::new("oracle_price_feed")),
            "oracle-price-feed-rs"
        );
        assert_eq!(default_crate_name(Path::new("Counter")), "counter-rs");
        assert_eq!(default_crate_name(Path::new("packages/foo")), "foo-rs");
    }
}
