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

mod addresses;
mod manifest_rewrite;
mod move_deps;
mod staging_layout;
mod worklist;

pub(crate) use self::move_deps::read_addresses_block;
pub use self::staging_layout::is_canonical_framework;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use self::addresses::build_address_overrides;
use self::manifest_rewrite::rewrite_staged_manifest;
use self::move_deps::{dep_source_from_kind, read_move_deps, read_move_package_name};
use self::staging_layout::{
    copy_dir_recursive, source_basename, source_key, unique_basename, unique_basename_against,
};
use self::worklist::WorkItem;
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
        // happens here (before the manifest rewrite) because the
        // rewrite only touches the staged copy — the original is
        // untouched, so it doesn't matter when we hash, as long as we
        // hash the original tree.
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

/// What we keep around per staged package between source-copy and the
/// manifest rewrite. `source_root` is the on-disk directory we copied
/// from — needed in the rewrite to resolve `local = "..."` deps
/// relative to the original authoring layout (the staged copy lives
/// elsewhere).
struct StagedRecord {
    staged: StagedPackage,
    source_root: PathBuf,
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
