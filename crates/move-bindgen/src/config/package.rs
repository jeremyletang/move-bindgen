//! Per-package config: [`PackageEntry`] (validated) + its
//! [`PackageSource`] variant, plus the duplicate-detection helpers used
//! during config load.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::Deserialize;

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
    pub(super) fn from_raw(id: &str, raw: &RawPackage) -> Result<Self> {
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

#[derive(Debug, Deserialize)]
pub(super) struct RawPackage {
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) git: Option<String>,
    #[serde(default)]
    pub(super) rev: Option<String>,
    #[serde(default)]
    pub(super) branch: Option<String>,
    #[serde(default)]
    pub(super) tag: Option<String>,
    #[serde(default)]
    pub(super) subdir: Option<String>,
    #[serde(default)]
    pub(super) crate_name: Option<String>,
}

pub(super) fn validate_unique_sources(packages: &[PackageEntry]) -> Result<()> {
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

pub(super) fn validate_unique_crate_names(packages: &[PackageEntry]) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
