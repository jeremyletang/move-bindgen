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

/// Public git URL for `move-bindgen-runtime`. Used as the default
/// runtime spec in `move-bindgen init` templates and zero-config
/// `generate` invocations. Tracks `master` — pin via `rev` once we
/// start cutting tagged releases.
pub const DEFAULT_RUNTIME_GIT_URL: &str = "https://github.com/jeremyletang/move-bindgen.git";

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

/// Move chain flavour — chooses which build chain, runtime crate,
/// and SDK family the generated code targets.
///
/// Flavour is per-project. Two flavours don't mix in one workspace
/// (different SDK type identities). Default is `Iota`; the Sui path
/// is being introduced (see `PLAN_SUI_COMPAT.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Flavour {
    #[default]
    Iota,
    Sui,
}

impl Flavour {
    pub fn as_str(&self) -> &'static str {
        match self {
            Flavour::Iota => "iota",
            Flavour::Sui => "sui",
        }
    }
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

impl RuntimeSpec {
    /// Default for templates and zero-config use: a git dep on the
    /// public move-bindgen repo, tracking master. Pin via `rev` once we
    /// start cutting tagged releases.
    pub fn default_git() -> Self {
        RuntimeSpec::Git {
            url: DEFAULT_RUNTIME_GIT_URL.to_string(),
            rev: None,
            branch: None,
            tag: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackageEntry {
    /// Stable identifier from the `[packages.<id>]` key. Used in error
    /// messages; not necessarily the Move package name.
    pub id: String,
    /// How the package source is fetched / located.
    pub source: PackageSource,
    /// Override for the generated crate name. Default derived from the
    /// source — basename of the path, or basename of the git subdir.
    pub crate_name_override: Option<String>,
}

/// Where the Move source for a `[packages.X]` entry lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    /// Local directory. Resolved against `--input-folder`s.
    Path(PathBuf),
    /// Remote git repository. `move-package`'s fetcher handles cloning
    /// into `~/.move/` and gives us the on-disk path.
    Git {
        url: String,
        rev: Option<String>,
        branch: Option<String>,
        tag: Option<String>,
        subdir: Option<String>,
    },
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

        Ok(Config {
            config_dir,
            flavour: raw.flavour.unwrap_or_default(),
            format,
            output_name: raw.output.name,
            runtime,
            framework_packages,
            packages,
        })
    }
}

/// Default crate name for a package: kebab-cased basename of the
/// source's "leaf" path component + `"-rs"`. For `Path` sources that's
/// the directory's basename; for `Git` sources it's the basename of
/// `subdir` (or the URL's repo segment if no subdir). Falls back to
/// `package-rs` if nothing usable can be extracted.
pub fn default_crate_name(source: &PackageSource) -> String {
    let stem = match source {
        PackageSource::Path(p) => p.file_name().and_then(|s| s.to_str()).map(str::to_string),
        PackageSource::Git { url, subdir, .. } => {
            if let Some(s) = subdir.as_deref() {
                Path::new(s)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            } else {
                // Last segment of the URL, stripping `.git`.
                url.rsplit('/')
                    .find(|s| !s.is_empty())
                    .map(|s| s.trim_end_matches(".git").to_string())
            }
        }
    };
    let stem = stem.unwrap_or_else(|| "package".to_string());
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
    /// [`default_crate_name`] derived from the source.
    pub fn crate_name(&self) -> String {
        self.crate_name_override
            .clone()
            .unwrap_or_else(|| default_crate_name(&self.source))
    }
}

impl PackageSource {
    fn from_raw(id: &str, raw: &RawPackage) -> Result<Self> {
        let kinds = [
            raw.path.is_some().then_some("path"),
            raw.git.is_some().then_some("git"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if kinds.is_empty() {
            bail!("package '{id}' must specify either `path` or `git` as its source");
        }
        if kinds.len() > 1 {
            bail!(
                "package '{id}' sets multiple sources ({}); pick exactly one",
                kinds.join(", ")
            );
        }
        if let Some(p) = &raw.path {
            return Ok(PackageSource::Path(PathBuf::from(p)));
        }
        let url = raw.git.clone().expect("git is set");
        let kinds = [
            raw.rev.is_some().then_some("rev"),
            raw.branch.is_some().then_some("branch"),
            raw.tag.is_some().then_some("tag"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if kinds.len() > 1 {
            bail!(
                "package '{id}' git source sets multiple of rev/branch/tag ({}); pick at most one",
                kinds.join(", ")
            );
        }
        Ok(PackageSource::Git {
            url,
            rev: raw.rev.clone(),
            branch: raw.branch.clone(),
            tag: raw.tag.clone(),
            subdir: raw.subdir.clone(),
        })
    }
}

// -----------------------------------------------------------------------------
// Raw (TOML-backing) types
// -----------------------------------------------------------------------------

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
    subdir: Option<String>,
    #[serde(default)]
    crate_name: Option<String>,
}

// -----------------------------------------------------------------------------
// Validation helpers
// -----------------------------------------------------------------------------

fn validate_unique_sources(packages: &[PackageEntry]) -> Result<()> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for p in packages {
        let key = source_key(&p.source);
        if let Some(prev) = seen.insert(key.clone(), &p.id) {
            bail!(
                "packages '{}' and '{}' both point at the same source ({})",
                prev,
                p.id,
                key
            );
        }
    }
    Ok(())
}

fn source_key(s: &PackageSource) -> String {
    match s {
        PackageSource::Path(p) => format!("path:{}", p.display()),
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

/// Staging directory convention: `<config-dir>/.move-bindgen/<name>/`.
///
/// All install artefacts for a given directory live under one
/// `.move-bindgen/` root, with a per-config subdirectory keyed by the
/// config's file stem. Examples:
///
/// - `configs/exchange.toml` → `configs/.move-bindgen/exchange/`
/// - `configs/pyth.toml`     → `configs/.move-bindgen/pyth/`
/// - `./move-bindgen.toml`   → `./.move-bindgen/default/`
///
/// The canonical `move-bindgen.toml` filename maps to `default/`
/// rather than the literal `move-bindgen/` to avoid the awkward
/// `.move-bindgen/move-bindgen/` doubled name.
///
/// One root means one gitignore line (`/.move-bindgen/`) regardless
/// of how many configs share a directory.
pub fn staging_dir_for(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = config_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    let subdir = if stem == "move-bindgen" {
        "default"
    } else {
        stem
    };
    dir.join(".move-bindgen").join(subdir)
}

#[cfg(test)]
mod staging_tests {
    use super::*;

    #[test]
    fn staging_dir_basics() {
        assert_eq!(
            staging_dir_for(Path::new("configs/exchange.toml")),
            PathBuf::from("configs/.move-bindgen/exchange"),
        );
        assert_eq!(
            staging_dir_for(Path::new("configs/counter.toml")),
            PathBuf::from("configs/.move-bindgen/counter"),
        );
        assert_eq!(
            staging_dir_for(Path::new("./move-bindgen.toml")),
            PathBuf::from("./.move-bindgen/default"),
        );
    }

    #[test]
    fn multiple_configs_in_one_dir_share_a_root() {
        // Two siblings under the same `.move-bindgen/`. This is the
        // whole point of the layout — one dotfile to gitignore.
        let a = staging_dir_for(Path::new("configs/exchange.toml"));
        let b = staging_dir_for(Path::new("configs/pyth.toml"));
        assert_eq!(a.parent(), b.parent());
        assert_eq!(a.parent().unwrap(), Path::new("configs/.move-bindgen"));
    }
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

    #[test]
    fn default_crate_name_kebabs_underscores() {
        assert_eq!(
            default_crate_name(&PackageSource::Path(PathBuf::from("oracle_price_feed"))),
            "oracle-price-feed-rs"
        );
        assert_eq!(
            default_crate_name(&PackageSource::Path(PathBuf::from("Counter"))),
            "counter-rs"
        );
        assert_eq!(
            default_crate_name(&PackageSource::Path(PathBuf::from("packages/foo"))),
            "foo-rs"
        );
    }
}
