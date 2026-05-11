//! `move-bindgen install` — populate the staging directory + write an
//! [`InstallManifest`].
//!
//! This is the side-effecting half of the pipeline (network, disk,
//! Move.toml rewrites). [`generate`] then reads the manifest and produces
//! Rust code purely from staging. Decoupling the two means generate runs
//! offline and is repeatable; install is the only step that may hit the
//! network.
//!
//! Discovery is worklist-driven. The user only lists root packages in
//! `move-bindgen.toml`; transitive deps fall out of walking each staged
//! Move.toml's `[dependencies]` block. For each item popped from the
//! worklist:
//!
//! 1. Resolve its source to a local on-disk directory
//!    (`PackageSource::Path` → input-folder lookup;
//!    `PackageSource::Git` → [`crate::resolve_git_source`], which drives
//!    `move-package`'s git fetcher and returns the cached path).
//! 2. Dedup by source key — same source = same staged entry.
//! 3. Copy the source tree to `<staging>/<basename>/`, dropping build
//!    artefacts (`build/`, `target/`, `Move.lock`).
//! 4. Rewrite the staged Move.toml's `[addresses]` block, converting
//!    literal `"0x0"` placeholders to `"_"` so the address-override pass
//!    below can bind them to unique synthetic values without conflict.
//! 5. Read its `[dependencies]` block and enqueue each (local or git) dep
//!    for the next iteration. `[dev-dependencies]` is skipped — bindings
//!    reflect the production interface.
//!
//! Once the worklist drains, install scans every staged Move.toml's
//! `[addresses]` block for `_`-valued names and assigns each a
//! deterministic synthetic address (`0xff_…`). The resulting map is baked
//! into the manifest so generate replays it identically across runs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use move_core_types::account_address::AccountAddress;

use crate::config::{default_crate_name, Config, PackageSource};
use crate::install_manifest::{
    InstallManifest, SerializableSource, StagedPackage, MANIFEST_VERSION,
};

/// Drive an install. `config_path` points at the user's `move-bindgen.toml`;
/// `input_folders` are the bases against which `PackageSource::Path`
/// entries are resolved. `reporter` receives cargo-style status lines
/// (use `Reporter::quiet()` to suppress). Returns the staging root that
/// was populated.
pub fn run(
    config_path: &Path,
    input_folders: &[PathBuf],
    reporter: &crate::reporter::Reporter,
) -> Result<PathBuf> {
    let started = std::time::Instant::now();
    reporter.stage("Resolving", config_path.display().to_string());

    let cfg = Config::load(config_path)?;
    let staging_root = crate::config::staging_dir_for(config_path);

    if staging_root.exists() {
        std::fs::remove_dir_all(&staging_root)
            .with_context(|| format!("clearing {}", staging_root.display()))?;
    }
    std::fs::create_dir_all(&staging_root)
        .with_context(|| format!("creating {}", staging_root.display()))?;

    // Seed the worklist from listed [packages.*] entries. Resolving local
    // paths up-front to absolute directories means every later WorkItem
    // carries an already-resolved source — dedup-by-key works without
    // canonicalising at every check.
    let mut worklist: VecDeque<WorkItem> = VecDeque::new();
    for entry in &cfg.packages {
        let source = match &entry.source {
            PackageSource::Path(_) => {
                let abs = cfg.resolve_path_source(entry, input_folders)?;
                PackageSource::Path(abs)
            }
            git @ PackageSource::Git { .. } => git.clone(),
        };
        worklist.push_back(WorkItem {
            id: Some(entry.id.clone()),
            source,
            crate_name_override: entry.crate_name_override.clone(),
            origin: format!("listed as [packages.{}]", entry.id),
        });
    }

    let mut records: Vec<StagedRecord> = Vec::new();
    let mut staged_keys: BTreeSet<String> = BTreeSet::new();
    // Tracks staging basenames already in use. Distinct sources that
    // resolve to the same basename (e.g. a vendored `iota-framework`
    // alongside Pyth's git `iota-framework`) get an auto-numeric suffix
    // — basenames are internal, users don't refer to them.
    let mut used_basenames: BTreeSet<String> = BTreeSet::new();
    let mut used_ids: BTreeSet<String> = BTreeSet::new();
    let mut used_crate_names: BTreeMap<String, String> = BTreeMap::new();
    // source_key → staging basename. The manifest-rewriting pass below
    // uses this to rewrite each dep edge in a staged Move.toml from
    // its original `local = "..."` / `git = "..."` form into a sibling
    // reference like `local = "../<basename>"`.
    let mut staged_basenames: BTreeMap<String, String> = BTreeMap::new();

    // Walk the worklist. Stage each package under its source
    // directory's basename — NOT the [packages.X] id. Move.toml-level
    // relative paths between packages (e.g. `Iota.local =
    // "../iota-framework"`) only become valid after the
    // manifest-rewriting pass below points them at the staged sibling
    // layout, but we use basenames because they line up with the
    // typical authored form.
    while let Some(item) = worklist.pop_front() {
        let key = source_key(&item.source);
        if !staged_keys.insert(key.clone()) {
            continue;
        }

        let source_root = resolve_item_source(&item, &staging_root, reporter)?;
        let entry_label = item.label();
        // Auto-disambiguate the staging basename. Two distinct sources
        // can naturally share a basename (e.g. a local `iota-framework`
        // alongside a git-pinned one); a numeric suffix (-2, -3, …)
        // keeps them in separate dirs without forcing the user to name
        // them manually.
        let raw_basename = source_basename(&source_root, &entry_label);
        // For id-less (auto-discovered) entries, the basename also
        // becomes the entry_id. Avoid both basename and entry_id
        // collisions in one pass so a transitive dep whose source dir
        // happens to share a name with an explicit `[packages.<id>]`
        // (e.g. pyth_lazer's `lazer/contracts/sui` subdir vs our
        // `[packages.sui]`) gets a unique suffix instead of erroring.
        let basename = if item.id.is_none() {
            unique_basename_against(&raw_basename, &used_basenames, Some(&used_ids))
        } else {
            unique_basename(&raw_basename, &used_basenames)
        };
        used_basenames.insert(basename.clone());

        let entry_id = item.id.clone().unwrap_or_else(|| basename.clone());
        if !used_ids.insert(entry_id.clone()) {
            bail!(
                "package id '{}' is used by two staged entries (the second came in via {})",
                entry_id,
                item.origin,
            );
        }

        let dest = staging_root.join(&basename);
        copy_dir_recursive(&source_root, &dest)
            .with_context(|| format!("copying {} to staging", source_root.display()))?;
        let move_name = read_move_package_name(&source_root.join("Move.toml"))?;
        let framework = cfg.framework_packages.contains(&move_name);
        reporter.stage("Staging", &move_name);

        // Crate name resolution:
        //   - Explicit override → use it.
        //   - Listed entry without override → derive from source spec
        //     (existing behaviour). Conflicts → bail with guidance.
        //   - Auto-discovered entry → derive from the (possibly
        //     disambiguated) staging basename so it tracks the
        //     auto-suffix and never clashes silently.
        let crate_name = match (&item.crate_name_override, item.id.is_some()) {
            (Some(name), _) => name.clone(),
            (None, true) => default_crate_name(&item.source),
            (None, false) => format!("{basename}-rs"),
        };
        if let Some(prev) = used_crate_names.insert(crate_name.clone(), entry_id.clone()) {
            bail!(
                "packages '{}' and '{}' both produce crate '{}' — set `crate_name` on one to disambiguate",
                prev,
                entry_id,
                crate_name,
            );
        }

        // Walk this package's [dependencies] block and enqueue anything
        // we haven't already staged. Local paths resolve relative to
        // `source_root` (the original package dir, not the staged copy
        // — same `[dependencies]` text either way, but the original is
        // canonical and the relative path resolution stays uniform
        // regardless of whether the dep ends up staged or pre-existed
        // in `~/.move/`).
        let deps = read_move_deps(&source_root.join("Move.toml"))?;
        for dep in &deps {
            let dep_source = dep_source_from_kind(&dep.kind, &source_root);
            if staged_keys.contains(&source_key(&dep_source)) {
                continue;
            }
            worklist.push_back(WorkItem {
                id: None,
                source: dep_source,
                crate_name_override: None,
                origin: format!("transitive dep '{}' of '{}'", dep.name, entry_id),
            });
        }

        // Snapshot the original source for staleness detection. Hashing
        // happens here (before phase 2) because phase 2 only touches
        // the staged copy — the original is untouched, so it doesn't
        // matter when we hash, as long as we hash the original tree.
        let source_abs_path =
            std::fs::canonicalize(&source_root).unwrap_or_else(|_| source_root.clone());
        let source_digest = crate::digest::source_dir_digest(&source_root)
            .with_context(|| format!("hashing source dir {}", source_root.display()))?;

        staged_basenames.insert(key, basename.clone());
        records.push(StagedRecord {
            staged: StagedPackage {
                id: entry_id,
                move_name,
                crate_name,
                staged_path: PathBuf::from(&basename),
                source: SerializableSource::from_config(&item.source),
                source_abs_path,
                source_digest,
                framework,
            },
            source_root,
        });
    }

    // Rewrite each staged Move.toml so it builds against the
    // flattened staging layout. One pass via `toml::Value` handles all
    // three edits at once:
    //   - strip [dev-dependencies] / [dev-addresses] (we never staged
    //     dev edges; leaving them in trips move-package on parse);
    //   - rewrite [addresses] literal "0x0" → "_" (so synthetic-address
    //     overrides bind cleanly at build time);
    //   - rewrite each [dependencies] entry to `local = "../<basename>"`
    //     using `staged_basenames`, so the build resolves siblings in
    //     staging instead of chasing the original (now-invalid) paths.
    // The round-trip drops comments and reorders keys; the staged
    // Move.tomls are throwaway internal artefacts so the loss is fine.
    for rec in &records {
        rewrite_staged_manifest(
            &staging_root.join(&rec.staged.staged_path).join("Move.toml"),
            &rec.source_root,
            &staged_basenames,
            is_canonical_framework(&rec.staged.move_name),
        )?;
    }

    let staged: Vec<StagedPackage> = records.into_iter().map(|r| r.staged).collect();
    let address_overrides = build_address_overrides(&staging_root, &staged)?;
    let config_digest = crate::digest::file_digest(config_path)
        .with_context(|| format!("hashing config {}", config_path.display()))?;

    let manifest = InstallManifest {
        version: MANIFEST_VERSION,
        flavour: cfg.flavour,
        config_digest,
        packages: staged,
        address_overrides,
    };
    manifest.save(&staging_root)?;

    reporter.stage(
        "Finished",
        format!(
            "installing {} package(s) in {:.2}s",
            manifest.packages.len(),
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(staging_root)
}

/// Best-effort staleness check for `generate`. Re-hashes the user's
/// config + each staged package's *original* source dir and compares
/// against the digests `install` recorded.
///
/// Two failure shapes:
///   - `move-bindgen.toml` content differs from install — error,
///     because re-running install will produce different staging
///     (different package set / paths / git revs).
///   - One or more package source trees changed since install — error
///     listing the affected ids.
///
/// One non-failure: `source_abs_path` no longer exists (e.g. user
/// deleted their dex checkout). Skip silently — staging is
/// self-contained and generate can still build from it. Anyone
/// deliberately working without the originals on hand is fine; we
/// only flag drift, not absence.
pub fn verify_freshness(
    config_path: &Path,
    manifest: &crate::install_manifest::InstallManifest,
) -> Result<()> {
    let live_config = crate::digest::file_digest(config_path)
        .with_context(|| format!("hashing {}", config_path.display()))?;
    if live_config != manifest.config_digest {
        bail!(
            "{} has changed since install — re-run `move-bindgen install`",
            config_path.display()
        );
    }

    let mut drifted: Vec<&str> = Vec::new();
    for pkg in &manifest.packages {
        if !pkg.source_abs_path.is_dir() {
            continue;
        }
        let live = crate::digest::source_dir_digest(&pkg.source_abs_path)
            .with_context(|| format!("hashing source for '{}'", pkg.id))?;
        if live != pkg.source_digest {
            drifted.push(&pkg.id);
        }
    }

    if drifted.is_empty() {
        return Ok(());
    }
    bail!(
        "sources changed since install for: {} — re-run `move-bindgen install`",
        drifted.join(", ")
    );
}

/// One unit of staging work. Listed entries from `move-bindgen.toml`
/// carry their explicit id + crate-name override; transitively-discovered
/// entries set `id = None` and rely on the staging basename.
struct WorkItem {
    id: Option<String>,
    source: PackageSource,
    crate_name_override: Option<String>,
    /// Human-readable provenance string, surfaced in error messages so
    /// the user can trace e.g. a basename collision back to the dep
    /// edge that introduced it.
    origin: String,
}

impl WorkItem {
    fn label(&self) -> String {
        self.id.clone().unwrap_or_else(|| self.origin.clone())
    }
}

/// What we keep around per staged package between phase 1 and phase 2.
/// `source_root` is the on-disk directory we copied from — needed in
/// phase 2 to resolve `local = "..."` deps relative to the original
/// authoring layout (the staged copy lives elsewhere).
struct StagedRecord {
    staged: StagedPackage,
    source_root: PathBuf,
}

/// Reconstruct a `PackageSource` from a parsed dep entry, resolving
/// any local relative path against the parent package's source root.
fn dep_source_from_kind(kind: &MoveDepKind, parent_source_root: &Path) -> PackageSource {
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

/// Stable dedup key for a package source. Two work items sharing a key
/// stage the same on-disk directory and are folded together. For local
/// paths the key is the canonical absolute path (so symlinks and
/// trailing-slash variants collapse); for git it's the full
/// `(url, rev, branch, tag, subdir)` tuple.
fn source_key(s: &PackageSource) -> String {
    match s {
        PackageSource::Path(p) => {
            let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            format!("path:{}", canonical.display())
        }
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

fn resolve_item_source(
    item: &WorkItem,
    staging_root: &Path,
    reporter: &crate::reporter::Reporter,
) -> Result<PathBuf> {
    match &item.source {
        PackageSource::Path(p) => {
            // Listed entries were resolved against input folders before
            // being queued; transitive deps were resolved relative to
            // their parent's source dir. Either way `p` should already
            // be absolute and point at a real Move package.
            if !p.join("Move.toml").is_file() {
                bail!("{} has no Move.toml ({})", p.display(), item.origin,);
            }
            Ok(p.clone())
        }
        PackageSource::Git {
            url,
            rev,
            branch,
            tag,
            ..
        } => {
            let label = rev
                .as_deref()
                .or(branch.as_deref())
                .or(tag.as_deref())
                .unwrap_or("?");
            reporter.stage("Fetching", format!("{url} @ {label}"));
            crate::git_resolver::resolve_git_source(
                &item.source,
                &staging_root.join(".git-probes").join(scratch_id(item)),
                reporter.is_verbose(),
            )
        }
    }
}

/// Filesystem-safe scratch directory name for a git probe. Listed
/// entries use their id; auto-discovered ones substitute non-alnum
/// chars in their origin string with `_` so the probe can live on
/// disk without clobbering siblings.
fn scratch_id(item: &WorkItem) -> String {
    if let Some(id) = &item.id {
        return id.clone();
    }
    item.origin
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Derive the staging directory basename for a package: last path
/// component of its resolved source directory. Falls back to the
/// supplied label if the source has no usable basename (extremely
/// unlikely — implies the source resolves to `/`).
/// Move packages whose `[addresses]` entries are part of the on-chain
/// framework identity and must NOT be rewritten to the `_` placeholder.
/// Rewriting `iota = "0x2"` → `"_"` makes downstream packages reject
/// `iota::object::UID` as not-from-`iota::object::new`, since the
/// framework ends up compiled at a synthetic `0xff…` address.
///
/// Distinct from `Config::framework_packages` — that one controls
/// codegen routing (skip vs emit-as-peer). The decision here is solely
/// about whether the Move source is part of the canonical framework.
fn is_canonical_framework(move_name: &str) -> bool {
    matches!(
        move_name,
        "Iota"
            | "IotaSystem"
            | "MoveStdlib"
            | "Stardust"
            | "Sui"
            | "SuiSystem"
            | "Bridge"
            | "DeepBook"
    )
}

fn source_basename(source_root: &Path, fallback: &str) -> String {
    source_root
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// Pick a basename not already in `used`. If `raw` is free, use it
/// verbatim; otherwise append `-2`, `-3`, … until a free slot is
/// found. The counter is open-ended so this never fails.
fn unique_basename(raw: &str, used: &BTreeSet<String>) -> String {
    unique_basename_against(raw, used, None)
}

/// Like [`unique_basename`] but also avoids any string in `extra` (the
/// existing entry_id set). Used for auto-discovered entries, whose
/// `entry_id` derives from the chosen basename.
fn unique_basename_against(
    raw: &str,
    used: &BTreeSet<String>,
    extra: Option<&BTreeSet<String>>,
) -> String {
    let taken = |s: &str| used.contains(s) || extra.map(|e| e.contains(s)).unwrap_or(false);
    if !taken(raw) {
        return raw.to_string();
    }
    let mut suffix: u32 = 2;
    loop {
        let candidate = format!("{raw}-{suffix}");
        if !taken(&candidate) {
            return candidate;
        }
        suffix += 1;
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
            // `Published.toml` (Sui's move-package-alt publication file)
            // carries on-chain `published-at` per environment. We strip
            // it so the synthetic-address override is what drives codegen
            // — otherwise sui-move-build hands back the testnet/mainnet
            // address and codegen splits on it.
            || name_str == "Published.toml"
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
fn rewrite_staged_manifest(
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

/// Project a `[dependencies.X]` sub-table down to a `MoveDepKind`.
/// Reused by both phase-1 discovery (via [`parse_move_deps`]) and
/// phase-2 dep rewriting.
fn parse_dep_table(alias: &str, dep_table: &toml::value::Table) -> Result<MoveDepKind> {
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

/// One direct dependency from a Move.toml's `[dependencies]` table.
struct MoveDep {
    /// Alias used in `[dependencies]` — only carried for diagnostics.
    name: String,
    kind: MoveDepKind,
}

enum MoveDepKind {
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

/// Parse `[dependencies]` from a Move.toml. Both inline-table form
/// (`Foo = { local = "..." }`) and dotted form (`Foo.local = "..."`)
/// land in the same `toml::Value` shape, so one matcher covers both.
/// Skipped silently:
///   - `[dev-dependencies]` (test-only — not part of the prod interface).
///   - Entries with neither `local` nor `git` (e.g. address-only deps).
///   - Unknown extra keys like `override = true` (read elsewhere if we
///     ever need version-conflict resolution).
fn read_move_deps(move_toml: &Path) -> Result<Vec<MoveDep>> {
    let text = std::fs::read_to_string(move_toml)
        .with_context(|| format!("reading {}", move_toml.display()))?;
    parse_move_deps(&text)
        .with_context(|| format!("parsing dependencies from {}", move_toml.display()))
}

fn parse_move_deps(text: &str) -> Result<Vec<MoveDep>> {
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
    fn unique_basename_disambiguates() {
        let mut used: BTreeSet<String> = BTreeSet::new();
        assert_eq!(unique_basename("iota-framework", &used), "iota-framework");
        used.insert("iota-framework".into());
        assert_eq!(unique_basename("iota-framework", &used), "iota-framework-2");
        used.insert("iota-framework-2".into());
        assert_eq!(unique_basename("iota-framework", &used), "iota-framework-3");
        // Distinct base unaffected by collisions on a sibling.
        assert_eq!(unique_basename("move-stdlib", &used), "move-stdlib");
    }

    #[test]
    fn synthetic_addresses_are_distinct() {
        let a = format_hex_address(synthetic_address(0xff00_0000_0000_0001));
        let b = format_hex_address(synthetic_address(0xff00_0000_0000_0002));
        assert_ne!(a, b);
        // Canonical hex is 64 chars + `0x`; the synthetic prefix lives in
        // the low 16 bytes of the address, so the marker shows up near
        // the end rather than at the start.
        assert!(a.ends_with("ff00000000000001"));
    }

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
