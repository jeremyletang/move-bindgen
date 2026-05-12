//! Move.toml parsing: read `[package].name`, `[addresses]`, and
//! `[dependencies]` blocks, project dep entries down to a
//! [`MoveDepKind`], and reconstruct a [`PackageSource`] from a dep
//! kind so install can fold dep edges into the worklist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::PackageSource;

/// One direct dependency from a Move.toml's `[dependencies]` table.
pub(super) struct MoveDep {
    /// Alias used in `[dependencies]` — only carried for diagnostics.
    pub(super) name: String,
    pub(super) kind: MoveDepKind,
}

pub(super) enum MoveDepKind {
    /// `name.local = "../foo"` or `name = { local = "../foo" }`.
    /// `rel_path` is verbatim from the manifest (resolved relative to
    /// the manifest's directory by the caller).
    Local { rel_path: PathBuf },
    /// `name = { git = "...", rev = "...", subdir = "..." }`.
    Git {
        url: String,
        rev: Option<String>,
        branch: Option<String>,
        tag: Option<String>,
        subdir: Option<String>,
    },
}

/// Reconstruct a `PackageSource` from a parsed dep entry, resolving
/// any local relative path against the parent package's source root.
pub(super) fn dep_source_from_kind(kind: &MoveDepKind, parent_source_root: &Path) -> PackageSource {
    match kind {
        MoveDepKind::Local { rel_path } => PackageSource::Path(parent_source_root.join(rel_path)),
        MoveDepKind::Git {
            url,
            rev,
            branch,
            tag,
            subdir,
        } => PackageSource::Git {
            url: url.clone(),
            rev: rev.clone(),
            branch: branch.clone(),
            tag: tag.clone(),
            subdir: subdir.clone(),
        },
    }
}

/// Project a `[dependencies.X]` sub-table down to a `MoveDepKind`.
/// Reused by both phase-1 discovery (via [`parse_move_deps`]) and
/// phase-2 dep rewriting.
pub(super) fn parse_dep_table(alias: &str, dep_table: &toml::value::Table) -> Result<MoveDepKind> {
    if let Some(local) = dep_table.get("local").and_then(toml::Value::as_str) {
        return Ok(MoveDepKind::Local {
            rel_path: PathBuf::from(local),
        });
    }
    if let Some(git) = dep_table.get("git").and_then(toml::Value::as_str) {
        return Ok(MoveDepKind::Git {
            url: git.to_string(),
            rev: dep_table
                .get("rev")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            branch: dep_table
                .get("branch")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            tag: dep_table
                .get("tag")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
            subdir: dep_table
                .get("subdir")
                .and_then(toml::Value::as_str)
                .map(str::to_string),
        });
    }
    bail!("dependency '{}' has neither `local` nor `git`", alias)
}

pub(super) fn read_addresses_block(move_toml: &Path) -> Result<BTreeMap<String, String>> {
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

pub(super) fn read_move_package_name(move_toml: &Path) -> Result<String> {
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

/// Parse `[dependencies]` from a Move.toml. Both inline-table form
/// (`Foo = { local = "..." }`) and dotted form (`Foo.local = "..."`)
/// land in the same `toml::Value` shape, so one matcher covers both.
/// Skipped silently:
///   - `[dev-dependencies]` (test-only — not part of the prod interface).
///   - Entries with neither `local` nor `git` (e.g. address-only deps).
///   - Unknown extra keys like `override = true` (read elsewhere if we
///     ever need version-conflict resolution).
pub(super) fn read_move_deps(move_toml: &Path) -> Result<Vec<MoveDep>> {
    let text = std::fs::read_to_string(move_toml)
        .with_context(|| format!("reading {}", move_toml.display()))?;
    parse_move_deps(&text)
        .with_context(|| format!("parsing dependencies from {}", move_toml.display()))
}

pub(super) fn parse_move_deps(text: &str) -> Result<Vec<MoveDep>> {
    let v: toml::Value = toml::from_str(text)?;
    let table = match v.get("dependencies").and_then(toml::Value::as_table) {
        Some(t) => t,
        None => return Ok(vec![]),
    };
    let mut out = Vec::with_capacity(table.len());
    for (name, val) in table {
        let dep_table = val
            .as_table()
            .ok_or_else(|| anyhow!("dependency '{}' is not a table", name))?;
        // Skip entries that are neither local nor git — e.g. address-only
        // overrides or future dep shapes — without erroring; they're not
        // sources we need to stage.
        if dep_table.get("local").is_none() && dep_table.get("git").is_none() {
            continue;
        }
        out.push(MoveDep {
            name: name.clone(),
            kind: parse_dep_table(name, dep_table)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_move_deps_handles_both_dep_forms() {
        let deps = parse_move_deps(
            r#"
[package]
name = "DepFormsProbe"
edition = "2024"

[dependencies]
Iota.local = "../iota-framework"
fixed18    = { local = "../fixed18" }

[dependencies.Pyth]
git    = "https://github.com/pyth-network/pyth-crosschain.git"
rev    = "iota-contract-testnet"
subdir = "target_chains/sui/contracts"

[dev-dependencies]
Test.local = "../test"

[addresses]
probe = "0x0"
"#,
        )
        .unwrap();
        let by_name: BTreeMap<_, _> = deps.iter().map(|d| (d.name.as_str(), &d.kind)).collect();
        assert_eq!(by_name.len(), 3, "dev-dependencies must be skipped");
        assert!(by_name.contains_key("Iota"));
        assert!(by_name.contains_key("fixed18"));
        assert!(by_name.contains_key("Pyth"));
        assert!(!by_name.contains_key("Test"));

        match &by_name["Iota"] {
            MoveDepKind::Local { rel_path } => assert_eq!(rel_path, Path::new("../iota-framework")),
            _ => panic!("expected local"),
        }
        match &by_name["fixed18"] {
            MoveDepKind::Local { rel_path } => assert_eq!(rel_path, Path::new("../fixed18")),
            _ => panic!("expected local"),
        }
        match &by_name["Pyth"] {
            MoveDepKind::Git {
                url, rev, subdir, ..
            } => {
                assert_eq!(url, "https://github.com/pyth-network/pyth-crosschain.git");
                assert_eq!(rev.as_deref(), Some("iota-contract-testnet"));
                assert_eq!(subdir.as_deref(), Some("target_chains/sui/contracts"));
            }
            _ => panic!("expected git"),
        }
    }

    #[test]
    fn parse_move_deps_missing_section_is_empty() {
        let deps = parse_move_deps(
            r#"
[package]
name = "NoDeps"
edition = "2024"

[addresses]
no_deps = "_"
"#,
        )
        .unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn parse_move_deps_ignores_override_flag() {
        let deps = parse_move_deps(
            r#"
[package]
name = "P"
edition = "2024"

[dependencies]
Iota = { local = "../iota-framework", override = true }
"#,
        )
        .unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "Iota");
        match &deps[0].kind {
            MoveDepKind::Local { rel_path } => assert_eq!(rel_path, Path::new("../iota-framework")),
            _ => panic!("expected local"),
        }
    }
}
