//! How the generated code should reference `move-bindgen-runtime` —
//! [`RuntimeSpec`] (validated) and its TOML-backing form [`RawRuntime`].

use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::Deserialize;

/// Public git URL for `move-bindgen-runtime`. Used as the default
/// runtime spec in `move-bindgen init` templates and zero-config
/// `generate` invocations. Tracks `master` — pin via `rev` once we
/// start cutting tagged releases.
pub const DEFAULT_RUNTIME_GIT_URL: &str = "https://github.com/jeremyletang/move-bindgen.git";

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

    pub(super) fn from_raw(raw: RawRuntime) -> Result<Self> {
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
#[serde(untagged)]
pub(super) enum RawRuntime {
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
