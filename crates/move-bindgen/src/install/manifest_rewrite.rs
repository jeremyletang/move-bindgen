//! Staged-Move.toml rewriter — patches the staged copy of each
//! package's `Move.toml` so it builds against the flattened staging
//! layout.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::move_deps::{dep_source_from_kind, parse_dep_table};
use super::staging_layout::source_key;

/// Re-shape a staged Move.toml so it builds against the flattened
/// staging layout. One pass via `toml::Value`:
///
///   - drop `[dev-dependencies]` / `[dev-addresses]` (auto-discovery
///     skips dev edges, and leaving them in trips move-package on
///     parse since the targets aren't staged);
///   - rewrite `[addresses]` literal `"0x0"` → `"_"` so synthetic
///     address overrides bind cleanly at build time;
///   - rewrite each `[dependencies]` entry to `local = "../<basename>"`
///     using `staged_basenames` — without this, the staged Move.toml
///     keeps its original `local = "../vendor/..."` or `git = "..."`
///     paths, which point at locations that don't exist in staging.
///
/// Comments and key order in the staged Move.toml are not preserved
/// (toml::to_string regenerates the document). The staged copy is an
/// internal artefact, so the loss is fine.
pub(super) fn rewrite_staged_manifest(
    staged_move_toml: &Path,
    parent_source_root: &Path,
    staged_basenames: &BTreeMap<String, String>,
    framework: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(staged_move_toml)
        .with_context(|| format!("reading {}", staged_move_toml.display()))?;
    let mut v: toml::Value =
        toml::from_str(&text).with_context(|| format!("parsing {}", staged_move_toml.display()))?;
    let table = v.as_table_mut().ok_or_else(|| {
        anyhow!(
            "{}: top-level Move.toml is not a table",
            staged_move_toml.display()
        )
    })?;

    table.remove("dev-dependencies");
    table.remove("dev-addresses");

    // Address rewrite: only for non-framework packages.
    //
    // Framework packages (`Iota`, `Sui`, `MoveStdlib`, …) have
    // canonical fixed addresses (`0x1`, `0x2`, …) that downstream
    // packages reference by their on-chain identity. Rewriting them
    // to `_` would make the framework compile at a synthetic address
    // and break every dep that references `sui::tx_context::TxContext`
    // / `iota::tx_context::TxContext`.
    //
    // For everything else (user packages, published deps like Pyth)
    // we force every named address to `_` so the synthetic-address
    // override pass drives the final value. Packages with hard-coded
    // published addresses would otherwise compile against their real
    // mainnet address, and codegen's peer map — which keys on the
    // synthetic addresses we assign — wouldn't recognize the resulting
    // bytecode references.
    if !framework {
        // Read the package name first; some packages (predict,
        // pyth_lazer) lack an `[addresses]` block entirely and rely
        // on Move's "address name = package name" default. We have to
        // inject an explicit `<pkg> = "_"` entry so the override pass
        // sees something to synthesise.
        //
        // The Move convention is to use the lowercase form for the
        // address key even when `[package].name` is capitalised
        // (e.g. `name = "Pyth"` pairs with `[addresses].pyth = "…"`).
        let pkg_addr_name = table
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .map(|s| s.to_lowercase());

        // Ensure the [addresses] block exists.
        if !table.contains_key("addresses") {
            table.insert(
                "addresses".to_string(),
                toml::Value::Table(toml::value::Table::new()),
            );
        }
        if let Some(addrs) = table
            .get_mut("addresses")
            .and_then(toml::Value::as_table_mut)
        {
            // Force every named address to `_`.
            for (_name, val) in addrs.iter_mut() {
                *val = toml::Value::String("_".into());
            }
            // Make sure the package's own address-name is present so
            // the override pass picks it up.
            if let Some(name) = pkg_addr_name {
                addrs
                    .entry(name)
                    .or_insert_with(|| toml::Value::String("_".into()));
            }
        }

        // Strip `published-at`: it can substitute for a hard-coded
        // `[addresses]` entry. Framework packages keep theirs.
        if let Some(pkg) = table.get_mut("package").and_then(toml::Value::as_table_mut) {
            pkg.remove("published-at");
        }

        // Strip move-package-alt's `[dep-replacements.<env>]` blocks.
        // Each replacement carries a `published-at` for the
        // dep-in-question per environment, which the resolver uses
        // verbatim — bypassing our synthetic-address overrides for
        // that dep. By dropping the block, the resolver falls back to
        // the source the staging rewrite already redirected to.
        table.remove("dep-replacements");
    }

    if let Some(deps) = table
        .get_mut("dependencies")
        .and_then(toml::Value::as_table_mut)
    {
        for (alias, val) in deps.iter_mut() {
            let dep_table = val.as_table().ok_or_else(|| {
                anyhow!(
                    "{}: dependency '{}' is not a table",
                    staged_move_toml.display(),
                    alias
                )
            })?;
            let dep_kind = parse_dep_table(alias, dep_table)?;
            let dep_source = dep_source_from_kind(&dep_kind, parent_source_root);
            let key = source_key(&dep_source);
            let basename = staged_basenames.get(&key).ok_or_else(|| {
                anyhow!(
                    "{}: dependency '{}' references a package we didn't stage (key {})",
                    staged_move_toml.display(),
                    alias,
                    key,
                )
            })?;
            // Preserve `override = true` when the original dep had it.
            // Move-package uses this flag at the *root* of a build to
            // force a single version of an otherwise-conflicting
            // package (e.g. dex's `oracle-pyth-source` overrides
            // Pyth's git-pinned Iota with a vendored copy). Dropping
            // it would re-surface the resolver conflict our staging
            // disambiguation just made possible.
            let preserve_override = dep_table
                .get("override")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            let mut replacement = toml::value::Table::new();
            replacement.insert(
                "local".into(),
                toml::Value::String(format!("../{basename}")),
            );
            if preserve_override {
                replacement.insert("override".into(), toml::Value::Boolean(true));
            }
            *val = toml::Value::Table(replacement);
        }
    }

    let serialized = toml::to_string(&v)
        .with_context(|| format!("re-serialising {}", staged_move_toml.display()))?;
    std::fs::write(staged_move_toml, serialized)
        .with_context(|| format!("writing {}", staged_move_toml.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_staged_manifest_strips_dev_rewrites_addrs_and_redirects_deps() {
        // Build a fake staging tree:
        //   <root>/parent/Move.toml  (the one we rewrite)
        //   <root>/iota-framework/   (a "staged" sibling for the Iota dep)
        //   <root>/fixed18/          (a "staged" sibling for the fixed18 dep)
        // The parent's [dependencies] reference these via paths relative
        // to its ORIGINAL source dir, which we simulate as <root>/orig/.
        let root =
            std::env::temp_dir().join(format!("move-bindgen-rewrite-test-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(root.join("orig")).unwrap();
        std::fs::create_dir_all(root.join("iota-framework")).unwrap();
        std::fs::create_dir_all(root.join("fixed18")).unwrap();
        let staged_toml = root.join("parent").join("Move.toml");
        std::fs::create_dir_all(staged_toml.parent().unwrap()).unwrap();

        std::fs::write(
            &staged_toml,
            r#"
[package]
name    = "Parent"
edition = "2024"

[dependencies]
Iota.local    = "../iota-framework"
fixed18.local = "../fixed18"

[dev-dependencies]
Test.local = "../test"

[addresses]
parent = "0x0"
hardcoded = "0x123"

[dev-addresses]
test = "0x10"
"#,
        )
        .unwrap();

        // Match the keys phase 1 would have inserted: source_key uses
        // canonical absolute paths, so canonicalise the sibling dirs.
        let iota_canon = std::fs::canonicalize(root.join("iota-framework")).unwrap();
        let fixed_canon = std::fs::canonicalize(root.join("fixed18")).unwrap();
        let mut bases: BTreeMap<String, String> = BTreeMap::new();
        bases.insert(
            format!("path:{}", iota_canon.display()),
            "iota-framework".into(),
        );
        bases.insert(format!("path:{}", fixed_canon.display()), "fixed18".into());

        // The parent's "original source dir" — relative deps in its
        // Move.toml resolve from here. Setting it so `../iota-framework`
        // and `../fixed18` land inside <root>.
        let parent_source_root = root.join("orig");

        rewrite_staged_manifest(&staged_toml, &parent_source_root, &bases, false).unwrap();

        let after = std::fs::read_to_string(&staged_toml).unwrap();
        let parsed: toml::Value = toml::from_str(&after).unwrap();
        let table = parsed.as_table().unwrap();

        assert!(table.get("dev-dependencies").is_none(), "dev-deps stripped");
        assert!(
            table.get("dev-addresses").is_none(),
            "dev-addresses stripped"
        );

        let addrs = table["addresses"].as_table().unwrap();
        // For non-framework packages, every named address is forced to
        // the `_` placeholder so the synthetic-address override drives
        // the final value. Framework packages skip this rewrite — see
        // the separate test below.
        assert_eq!(addrs["parent"].as_str(), Some("_"), "0x0 → _");
        assert_eq!(
            addrs["hardcoded"].as_str(),
            Some("_"),
            "non-0x0 → _ for non-framework"
        );

        let deps = table["dependencies"].as_table().unwrap();
        assert_eq!(
            deps["Iota"].as_table().unwrap()["local"].as_str(),
            Some("../iota-framework"),
        );
        assert_eq!(
            deps["fixed18"].as_table().unwrap()["local"].as_str(),
            Some("../fixed18"),
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rewrite_staged_manifest_errors_on_unknown_dep() {
        let root = std::env::temp_dir().join(format!(
            "move-bindgen-rewrite-unknown-{}",
            std::process::id()
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(root.join("orig")).unwrap();
        let staged_toml = root.join("parent").join("Move.toml");
        std::fs::create_dir_all(staged_toml.parent().unwrap()).unwrap();
        std::fs::write(
            &staged_toml,
            r#"
[package]
name    = "P"
edition = "2024"

[dependencies]
Mystery.local = "../mystery"
"#,
        )
        .unwrap();
        let bases: BTreeMap<String, String> = BTreeMap::new();
        let err =
            rewrite_staged_manifest(&staged_toml, &root.join("orig"), &bases, false).unwrap_err();
        assert!(
            format!("{err}").contains("didn't stage"),
            "expected an unknown-dep error, got: {err}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
