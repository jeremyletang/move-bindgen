use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use move_binary_format::file_format::Visibility;
use std::collections::BTreeMap;

use anyhow::Context;
use move_bindgen::{
    config_path_in, foreign_addresses_used, Bindings, BuildOptions, Config, OutputFormat, PeerDep,
    PeerMap, Reporter, RuntimeSpec,
};
use move_core_types::account_address::AccountAddress;

#[derive(Parser, Debug)]
#[command(
    name = "move-bindgen",
    version,
    about = "Generate Rust bindings from a Move package"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Build the package and print a summary of its IR. No code is written.
    Dump {
        /// Path to the Move package (directory containing `Move.toml`).
        package: PathBuf,
    },
    /// Build the package and write a complete Rust bindings crate.
    ///
    /// Two modes:
    ///   1. Zero-config: pass a positional `<package>` path — generates
    ///      one crate at `<package>-rs` (or `--out`). Uses
    ///      `--runtime-path` for the runtime dep in `Cargo.toml`.
    ///   2. Config-driven: pass `--config <toml>`. Reads the staging dir
    ///      written by `move-bindgen install` and emits Rust against it.
    ///      Errors clearly if `install` hasn't run yet.
    Generate {
        /// Zero-config: path to a Move package (directory with `Move.toml`).
        #[arg(conflicts_with = "config")]
        package: Option<PathBuf>,
        /// Config-driven: path to a `move-bindgen.toml`. Mutually
        /// exclusive with the positional `<package>`. Reads staging
        /// from `<config-dir>/.move-bindgen-<config-stem>/`.
        #[arg(long, conflicts_with = "package")]
        config: Option<PathBuf>,
        /// Output directory. Defaults to `<package>-rs` sibling to
        /// `package` (zero-config mode) or `<config-dir>/<output.name>`
        /// (config mode).
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
        /// Path dependency for `move-bindgen-runtime` in zero-config mode.
        /// Ignored when `--config` is set (the config's runtime takes
        /// precedence).
        #[arg(long, default_value = "../../crates/move-bindgen-runtime")]
        runtime_path: String,
        /// Suppress progress output. Errors still report to stderr.
        #[arg(long, short = 'q')]
        quiet: bool,
    },
    /// Load and validate a `move-bindgen.toml`. No code is written; this
    /// surfaces parse / shape / uniqueness errors and prints a summary of
    /// the resolved config.
    ///
    /// `--input-dir` is repeatable. Each `[packages.*].path` is
    /// resolved against each input dir in order; first hit wins.
    /// Defaults to the config file's directory if no input dirs are
    /// passed.
    Check {
        /// Path to the config file. Defaults to `./move-bindgen.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Where to look for the Move packages referenced by `[packages.*].path`.
        /// Repeatable; first hit wins. Defaults to the config file's dir.
        #[arg(long = "input-dir", short = 'i')]
        input_dirs: Vec<PathBuf>,
    },
    /// Resolve every `[packages.*]` entry, copy/clone its source into a
    /// staging directory next to the config, rewrite Move.toml address
    /// placeholders, and write a `packages.json` manifest. `move-bindgen
    /// generate` reads that manifest — install is the only step that
    /// may hit the network.
    Install {
        /// Path to the config file. Defaults to `./move-bindgen.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Bases against which `[packages.*].path` entries resolve.
        /// Repeatable; first hit wins. Defaults to the config dir.
        #[arg(long = "input-dir", short = 'i')]
        input_dirs: Vec<PathBuf>,
        /// Suppress progress output. Errors still report to stderr.
        #[arg(long, short = 'q')]
        quiet: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Dump { package } => {
            let bindings = move_bindgen::load_package(&package)?;
            print_dump(&bindings);
        }
        Cmd::Generate {
            package,
            config,
            out,
            runtime_path,
            quiet,
        } => {
            let reporter = if quiet {
                Reporter::quiet()
            } else {
                Reporter::new()
            };
            match (package, config) {
                (Some(pkg), None) => {
                    generate_zero_config(&pkg, out.as_deref(), &runtime_path, &reporter)?
                }
                (None, Some(cfg)) => generate_with_config(&cfg, out.as_deref(), &reporter)?,
                _ => anyhow::bail!(
                    "pass either a positional <package> path or --config <toml>, but not both"
                ),
            }
        }
        Cmd::Check { config, input_dirs } => {
            let config_path = config.unwrap_or_else(|| config_path_in(std::path::Path::new(".")));
            let cfg = Config::load(&config_path)?;
            check_config(&cfg, &input_dirs)?;
        }
        Cmd::Install {
            config,
            input_dirs,
            quiet,
        } => {
            let config_path = config.unwrap_or_else(|| config_path_in(std::path::Path::new(".")));
            let reporter = if quiet {
                Reporter::quiet()
            } else {
                Reporter::new()
            };
            move_bindgen::install(&config_path, &input_dirs, &reporter)?;
        }
    }
    Ok(())
}

fn generate_zero_config(
    package: &Path,
    out: Option<&Path>,
    runtime_path: &str,
    reporter: &Reporter,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    reporter.stage("Compiling", package.display().to_string());
    let bindings = move_bindgen::load_package(package)?;
    let opts = move_bindgen::GenerateOptions {
        runtime: RuntimeSpec::Path(PathBuf::from(runtime_path)),
        ..Default::default()
    };
    let crate_ = move_bindgen::generate(&bindings, &opts)?;
    let out_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_out_dir(package, &crate_.crate_name));
    reporter.stage("Generating", &crate_.crate_name);
    write_crate(&out_dir, &crate_)?;
    reporter.stage(
        "Finished",
        format!(
            "generating {} in {:.2}s",
            crate_.crate_name,
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}

fn generate_with_config(
    config_path: &Path,
    out: Option<&Path>,
    reporter: &Reporter,
) -> anyhow::Result<()> {
    reporter.stage("Verifying", config_path.display().to_string());
    let cfg = Config::load(config_path)?;
    let staging_root = move_bindgen::staging_dir_for(config_path);
    let manifest = move_bindgen::InstallManifest::load(&staging_root)?;
    move_bindgen::verify_freshness(config_path, &manifest)?;
    let overrides = parse_overrides(&manifest.address_overrides)?;
    match cfg.format {
        OutputFormat::SingleCrate => {
            generate_single_from_staging(&cfg, &staging_root, &manifest, &overrides, out, reporter)
        }
        OutputFormat::Workspace => generate_workspace_from_staging(
            &cfg,
            &staging_root,
            &manifest,
            &overrides,
            out,
            reporter,
        ),
    }
}

fn parse_overrides(
    map: &BTreeMap<String, String>,
) -> anyhow::Result<BTreeMap<String, AccountAddress>> {
    map.iter()
        .map(|(name, hex)| {
            let addr = AccountAddress::from_hex_literal(hex)
                .with_context(|| format!("parsing override for {name}: {hex}"))?;
            Ok((name.clone(), addr))
        })
        .collect()
}

fn generate_single_from_staging(
    cfg: &Config,
    staging_root: &Path,
    manifest: &move_bindgen::InstallManifest,
    overrides: &BTreeMap<String, AccountAddress>,
    out: Option<&Path>,
    reporter: &Reporter,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let pkg = manifest
        .packages
        .first()
        .ok_or_else(|| anyhow::anyhow!("staging manifest has no packages"))?;
    let pkg_path = staging_root.join(&pkg.staged_path);
    reporter.stage("Compiling", &pkg.move_name);
    let bindings = move_bindgen::load_package_with_options(
        &pkg_path,
        &BuildOptions {
            additional_named_addresses: overrides.clone(),
            ..Default::default()
        },
    )?;
    let out_name = cfg
        .output_name
        .clone()
        .unwrap_or_else(|| pkg.crate_name.clone());
    let out_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cfg.config_dir.join(&out_name));
    let runtime = relativize_runtime(&cfg.runtime, &cfg.config_dir, &out_dir);
    let opts = move_bindgen::GenerateOptions {
        runtime,
        crate_name_override: Some(pkg.crate_name.clone()),
        ..Default::default()
    };
    let crate_ = move_bindgen::generate(&bindings, &opts)?;
    reporter.stage("Generating", &crate_.crate_name);
    write_crate(&out_dir, &crate_)?;
    reporter.stage(
        "Finished",
        format!(
            "generating {} in {:.2}s",
            crate_.crate_name,
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}

fn generate_workspace_from_staging(
    cfg: &Config,
    staging_root: &Path,
    manifest: &move_bindgen::InstallManifest,
    overrides: &BTreeMap<String, AccountAddress>,
    out: Option<&Path>,
    reporter: &Reporter,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let workspace_name = cfg
        .output_name
        .clone()
        .expect("config validator guarantees [output].name in workspace mode");
    let workspace_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cfg.config_dir.join(&workspace_name));

    // Phase 1 — load every staged package, build the peer map keyed by
    // address. Framework-marked entries are skipped: the runtime owns
    // their types and ty.rs's well-known mappings handle the routing.
    let mut loaded = Vec::with_capacity(manifest.packages.len());
    let mut peers = PeerMap::new();
    for pkg in &manifest.packages {
        if pkg.framework {
            continue;
        }
        let pkg_path = staging_root.join(&pkg.staged_path);
        reporter.stage("Compiling", &pkg.move_name);
        let bindings = move_bindgen::load_package_with_options(
            &pkg_path,
            &BuildOptions {
                additional_named_addresses: overrides.clone(),
                ..Default::default()
            },
        )?;
        let addr = bindings_address(&bindings);
        peers.insert(addr, pkg.crate_name.clone())?;
        loaded.push((pkg.clone(), bindings));
    }

    // Phase 2 — codegen each member crate, computing per-crate peer
    // deps by walking type references in its IR.
    std::fs::create_dir_all(&workspace_dir)?;
    let mut member_dirs = Vec::with_capacity(loaded.len());
    for (pkg, bindings) in &loaded {
        let crate_dir = workspace_dir.join(&pkg.crate_name);
        let foreign = foreign_addresses_used(bindings);
        let own_addr = bindings_address(bindings);
        let peer_deps = foreign
            .into_iter()
            .filter_map(|addr| {
                if addr == own_addr {
                    return None;
                }
                let peer = peers.lookup(&addr)?;
                Some(PeerDep {
                    crate_name: peer.crate_name.clone(),
                    rel_path: PathBuf::from(format!("../{}", peer.crate_name)),
                })
            })
            .collect::<Vec<_>>();

        let runtime = relativize_runtime(&cfg.runtime, &cfg.config_dir, &crate_dir);
        let skip_modules = framework_skip_modules(&pkg.move_name);
        let opts = move_bindgen::GenerateOptions {
            runtime,
            peers: peers.clone(),
            as_workspace_member: true,
            peer_deps,
            skip_modules,
            crate_name_override: Some(pkg.crate_name.clone()),
        };
        let crate_ = move_bindgen::generate(bindings, &opts)?;
        write_crate(&crate_dir, &crate_)?;
        member_dirs.push(pkg.crate_name.clone());
    }

    // Phase 3 — workspace-level Cargo.toml + .gitignore.
    let runtime = relativize_runtime(&cfg.runtime, &cfg.config_dir, &workspace_dir);
    let workspace_cargo = render_workspace_cargo_toml(&member_dirs, &runtime);
    std::fs::write(workspace_dir.join("Cargo.toml"), workspace_cargo)?;
    std::fs::write(workspace_dir.join(".gitignore"), "/target\n")?;

    reporter.stage(
        "Generating",
        format!("{workspace_name} ({} crates)", member_dirs.len()),
    );
    reporter.stage(
        "Finished",
        format!(
            "generating {workspace_name} in {:.2}s",
            started.elapsed().as_secs_f64()
        ),
    );
    Ok(())
}

/// Modules whose types are owned by `move-bindgen-runtime` and so should
/// be skipped during codegen for the Iota framework / Move stdlib peer
/// crates. ty.rs's well-known mappings route references to these types
/// into the runtime instead of looking them up via the peer map.
fn framework_skip_modules(package_name: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    match package_name {
        "Iota" => {
            // `object` types live in the runtime; `ptb_command` /
            // `ptb_call_arg` define their own `Argument` / `Command`
            // enums that collide with the SDK's PTB types our generated
            // code already imports. `auth_context` / `ptb` reference
            // those skipped types so they cascade out.
            out.insert("object".to_string());
            out.insert("ptb_command".to_string());
            out.insert("ptb_call_arg".to_string());
            out.insert("auth_context".to_string());
            out.insert("ptb".to_string());
        }
        "MoveStdlib" => {
            out.insert("option".to_string());
            out.insert("string".to_string());
            out.insert("ascii".to_string());
        }
        _ => {}
    }
    out
}

fn bindings_address(b: &Bindings) -> AccountAddress {
    b.published_at
        .or_else(|| b.modules.first().map(|m| m.id.address))
        .unwrap_or(AccountAddress::ZERO)
}

fn render_workspace_cargo_toml(members: &[String], runtime: &RuntimeSpec) -> String {
    let mut s = String::new();
    s.push_str("# @generated by move-bindgen — regenerate with `move-bindgen generate`.\n\n");
    s.push_str("[workspace]\nresolver = \"2\"\nmembers = [\n");
    for m in members {
        s.push_str(&format!("    \"{m}\",\n"));
    }
    s.push_str("]\n\n");
    s.push_str("[workspace.package]\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n");
    s.push_str("[workspace.dependencies]\n");
    s.push_str(&render_runtime_dep_line_text(runtime));
    s.push('\n');
    s.push_str("serde = { version = \"1\", features = [\"derive\"] }\n");
    s.push_str("bcs   = \"0.1\"\n");
    s
}

fn render_runtime_dep_line_text(spec: &RuntimeSpec) -> String {
    match spec {
        RuntimeSpec::Path(p) => format!("move-bindgen-runtime = {{ path = \"{}\" }}", p.display()),
        RuntimeSpec::Version(v) => format!("move-bindgen-runtime = \"{v}\""),
        RuntimeSpec::Git {
            url,
            rev,
            branch,
            tag,
        } => {
            let mut parts = vec![format!("git = \"{url}\"")];
            if let Some(r) = rev {
                parts.push(format!("rev = \"{r}\""));
            }
            if let Some(b) = branch {
                parts.push(format!("branch = \"{b}\""));
            }
            if let Some(t) = tag {
                parts.push(format!("tag = \"{t}\""));
            }
            format!("move-bindgen-runtime = {{ {} }}", parts.join(", "))
        }
    }
}

/// For `RuntimeSpec::Path`, the path in the config is relative to the
/// config's directory; the `Cargo.toml` we write needs the path relative
/// to the output directory. Resolve via canonicalisation, then
/// re-relativise. Falls back to the original path if canonicalisation
/// fails (e.g. the runtime crate doesn't exist at codegen time).
fn relativize_runtime(spec: &RuntimeSpec, config_dir: &Path, output_dir: &Path) -> RuntimeSpec {
    match spec {
        RuntimeSpec::Path(p) => {
            let abs_runtime = config_dir.join(p);
            let abs_runtime = std::fs::canonicalize(&abs_runtime).unwrap_or(abs_runtime);
            // Output dir may not yet exist; canonicalise its parent and
            // append the basename.
            let abs_out = canonicalise_or_keep(output_dir);
            let rel = relative_from(&abs_runtime, &abs_out);
            RuntimeSpec::Path(rel)
        }
        other => other.clone(),
    }
}

fn canonicalise_or_keep(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        if let Ok(c) = std::fs::canonicalize(parent) {
            return c.join(name);
        }
    }
    p.to_path_buf()
}

/// Compute a relative path from `from` to `target`, both expected to be
/// absolute. Falls back to `target` as-is if they share no common prefix.
fn relative_from(target: &Path, from: &Path) -> PathBuf {
    let target_parts: Vec<_> = target.components().collect();
    let from_parts: Vec<_> = from.components().collect();
    let common = target_parts
        .iter()
        .zip(from_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return target.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..from_parts.len() {
        out.push("..");
    }
    for c in &target_parts[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn check_config(cfg: &Config, input_folders: &[PathBuf]) -> anyhow::Result<()> {
    println!("config: ok");
    println!(
        "  format:           {}",
        match cfg.format {
            OutputFormat::SingleCrate => "single-crate",
            OutputFormat::Workspace => "workspace",
        }
    );
    if let Some(name) = &cfg.output_name {
        println!("  output name:      {}", name);
    }
    println!("  runtime:          {:?}", cfg.runtime);
    println!(
        "  framework pkgs:   {}",
        cfg.framework_packages
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  packages ({}):", cfg.packages.len());
    for p in &cfg.packages {
        let summary: String = match &p.source {
            move_bindgen::PackageSource::Path(_) => cfg
                .resolve_path_source(p, input_folders)?
                .display()
                .to_string(),
            move_bindgen::PackageSource::Git { url, rev, .. } => {
                format!("git:{url} (rev {})", rev.as_deref().unwrap_or("<default>"))
            }
        };
        println!(
            "    [{:>20}]  crate={:<28} {}",
            p.id,
            p.crate_name(),
            summary
        );
    }
    Ok(())
}

fn default_out_dir(package: &std::path::Path, crate_name: &str) -> PathBuf {
    let parent = package
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    parent.join(crate_name)
}

fn write_crate(out_dir: &std::path::Path, c: &move_bindgen::GeneratedCrate) -> anyhow::Result<()> {
    let src_dir = out_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;
    std::fs::write(out_dir.join("Cargo.toml"), &c.cargo_toml)?;
    std::fs::write(src_dir.join("lib.rs"), &c.lib_rs)?;
    for (name, body) in &c.module_files {
        std::fs::write(src_dir.join(name), body)?;
    }
    Ok(())
}

fn print_dump(b: &Bindings) {
    println!("package: {}", b.package_name);
    match b.published_at {
        Some(addr) => println!("published_at: 0x{}", addr.short_str_lossless()),
        None => println!("published_at: <unpublished>"),
    }
    println!();

    for m in &b.modules {
        println!(
            "module 0x{}::{}",
            m.id.address.short_str_lossless(),
            m.id.name
        );

        if !m.structs.is_empty() {
            println!("  structs:");
            for (name, s) in &m.structs {
                println!(
                    "    {} {}{}",
                    abilities_str(s.abilities),
                    name,
                    fmt_type_params(&s.type_parameters),
                );
                for f in &s.fields {
                    println!("      {}: {}", f.name, f.type_);
                }
            }
        }

        if !m.enums.is_empty() {
            println!("  enums:");
            for (name, e) in &m.enums {
                println!(
                    "    {} {}{}",
                    abilities_str(e.abilities),
                    name,
                    fmt_type_params(&e.type_parameters),
                );
                for v in &e.variants {
                    let fields: Vec<String> = v
                        .fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, f.type_))
                        .collect();
                    println!("      {}({})", v.name, fields.join(", "));
                }
            }
        }

        if !m.constants.is_empty() {
            println!("  constants:");
            for (i, c) in m.constants.iter().enumerate() {
                println!("    [{i}] {} ({} bytes)", c.type_, c.data.len());
            }
        }

        if !m.functions.is_empty() {
            println!("  functions:");
            for (name, f) in &m.functions {
                let vis = match (f.visibility, f.is_entry) {
                    (Visibility::Public, true) => "public entry",
                    (Visibility::Public, false) => "public",
                    (Visibility::Friend, _) => "public(friend)",
                    (Visibility::Private, true) => "entry",
                    (Visibility::Private, false) => "private",
                };
                let params: Vec<String> = f.parameters.iter().map(|t| t.to_string()).collect();
                let returns: Vec<String> = f.return_.iter().map(|t| t.to_string()).collect();
                let ret = if returns.is_empty() {
                    String::new()
                } else {
                    format!(": ({})", returns.join(", "))
                };
                println!(
                    "    {} fun {}{}({}){}",
                    vis,
                    name,
                    fmt_ability_params(&f.type_parameters),
                    params.join(", "),
                    ret,
                );
            }
        }

        println!();
    }
}

fn abilities_str(a: move_binary_format::file_format::AbilitySet) -> String {
    let mut out = Vec::new();
    if a.has_copy() {
        out.push("copy");
    }
    if a.has_drop() {
        out.push("drop");
    }
    if a.has_store() {
        out.push("store");
    }
    if a.has_key() {
        out.push("key");
    }
    if out.is_empty() {
        "[]".into()
    } else {
        format!("[{}]", out.join(", "))
    }
}

fn fmt_type_params(ps: &[move_binary_format::file_format::DatatypeTyParameter]) -> String {
    if ps.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = ps
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let phantom = if p.is_phantom { "phantom " } else { "" };
            format!("{}T{}: {}", phantom, i, abilities_str(p.constraints))
        })
        .collect();
    format!("<{}>", parts.join(", "))
}

fn fmt_ability_params(ps: &[move_binary_format::file_format::AbilitySet]) -> String {
    if ps.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = ps
        .iter()
        .enumerate()
        .map(|(i, a)| format!("T{}: {}", i, abilities_str(*a)))
        .collect();
    format!("<{}>", parts.join(", "))
}
