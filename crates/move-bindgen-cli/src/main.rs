use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use move_binary_format::file_format::Visibility;
use std::collections::BTreeMap;

use anyhow::Context;
use move_bindgen::{
    config_path_in, foreign_addresses_used, Bindings, BuildOptions, Config, OutputFormat, PeerDep,
    PeerMap, RuntimeSpec,
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
    ///   2. Config-driven: pass `--config <toml>`. Reads
    ///      `move-bindgen.toml`; output goes alongside the config (or to
    ///      `--out`); runtime dep comes from the config.
    Generate {
        /// Zero-config: path to a Move package (directory with `Move.toml`).
        #[arg(conflicts_with = "config")]
        package: Option<PathBuf>,
        /// Config-driven: path to a `move-bindgen.toml`. Mutually
        /// exclusive with the positional `<package>`.
        #[arg(long, conflicts_with = "package")]
        config: Option<PathBuf>,
        /// Where to look for the Move packages referenced by
        /// `[packages.*].path` in the config. Repeatable; first match
        /// wins. Defaults to the config file's directory.
        #[arg(long = "input-folder", requires = "config")]
        input_folders: Vec<PathBuf>,
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
    },
    /// Load and validate a `move-bindgen.toml`. No code is written; this
    /// surfaces parse / shape / uniqueness errors and prints a summary of
    /// the resolved config.
    ///
    /// `--input-folder` is repeatable. Each `[packages.*].path` is
    /// resolved against each input folder in order; first hit wins.
    /// Defaults to the config file's directory if no input folders are
    /// passed.
    Check {
        /// Path to the config file. Defaults to `./move-bindgen.toml`.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Where to look for the Move packages referenced by `[packages.*].path`.
        #[arg(long = "input-folder")]
        input_folders: Vec<PathBuf>,
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
            input_folders,
            out,
            runtime_path,
        } => match (package, config) {
            (Some(pkg), None) => generate_zero_config(&pkg, out.as_deref(), &runtime_path)?,
            (None, Some(cfg)) => generate_with_config(&cfg, &input_folders, out.as_deref())?,
            _ => anyhow::bail!(
                "pass either a positional <package> path or --config <toml>, but not both"
            ),
        },
        Cmd::Check {
            config,
            input_folders,
        } => {
            let config_path = config.unwrap_or_else(|| config_path_in(std::path::Path::new(".")));
            let cfg = Config::load(&config_path)?;
            check_config(&cfg, &input_folders)?;
        }
    }
    Ok(())
}

fn generate_zero_config(
    package: &Path,
    out: Option<&Path>,
    runtime_path: &str,
) -> anyhow::Result<()> {
    let bindings = move_bindgen::load_package(package)?;
    let opts = move_bindgen::GenerateOptions {
        runtime: RuntimeSpec::Path(PathBuf::from(runtime_path)),
        ..Default::default()
    };
    let crate_ = move_bindgen::generate(&bindings, &opts)?;
    let out_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_out_dir(package, &crate_.crate_name));
    write_crate(&out_dir, &crate_)?;
    eprintln!("wrote crate to {}", out_dir.display());
    Ok(())
}

fn generate_with_config(
    config_path: &Path,
    input_folders: &[PathBuf],
    out: Option<&Path>,
) -> anyhow::Result<()> {
    let cfg = Config::load(config_path)?;
    match cfg.format {
        OutputFormat::SingleCrate => {
            let entry = cfg
                .packages
                .first()
                .expect("validator guarantees one entry in single-crate mode");
            let pkg_path = cfg.resolve_package_path(entry, input_folders)?;
            let bindings = move_bindgen::load_package(&pkg_path)?;
            // Default name = `<entry crate name>` (e.g. `counter-rs`).
            let out_name = cfg
                .output_name
                .clone()
                .unwrap_or_else(|| entry.crate_name());
            let out_dir = out
                .map(Path::to_path_buf)
                .unwrap_or_else(|| cfg.config_dir.join(&out_name));
            let runtime = relativize_runtime(&cfg.runtime, &cfg.config_dir, &out_dir);
            let opts = move_bindgen::GenerateOptions {
                runtime,
                ..Default::default()
            };
            let crate_ = move_bindgen::generate(&bindings, &opts)?;
            write_crate(&out_dir, &crate_)?;
            eprintln!("wrote crate to {}", out_dir.display());
        }
        OutputFormat::Workspace => {
            generate_workspace(&cfg, input_folders, out)?;
        }
    }
    Ok(())
}

fn generate_workspace(
    cfg: &Config,
    input_folders: &[PathBuf],
    out: Option<&Path>,
) -> anyhow::Result<()> {
    // Workspace name is required by config validation, so it's always Some.
    let workspace_name = cfg
        .output_name
        .clone()
        .expect("config validator guarantees [output].name in workspace mode");
    let workspace_dir = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cfg.config_dir.join(&workspace_name));

    // Phase 0 — stage every input folder into a writeable mirror, then
    // rewrite each listed package's Move.toml in the mirror to convert
    // literal `"0x0"` placeholders to `"_"` so our address overrides can
    // bind them to unique synthetic values. Original sources untouched.
    let staging_root = workspace_dir.join(".staging");
    if staging_root.exists() {
        std::fs::remove_dir_all(&staging_root)?;
    }
    std::fs::create_dir_all(&staging_root)?;
    let staged_inputs_owned: Vec<PathBuf> = {
        let mut out: Vec<PathBuf> = Vec::with_capacity(input_folders.len().max(1));
        if input_folders.is_empty() {
            let dest = staging_root.join("0");
            copy_dir_recursive(&cfg.config_dir, &dest)
                .with_context(|| format!("copying {} to staging", cfg.config_dir.display()))?;
            out.push(dest);
        } else {
            for (i, src) in input_folders.iter().enumerate() {
                let dest = staging_root.join(i.to_string());
                copy_dir_recursive(src, &dest)
                    .with_context(|| format!("copying {} to staging", src.display()))?;
                out.push(dest);
            }
        }
        out
    };

    // Phase 1a — resolve each [packages.*].path against staging (not the
    // original input folders), and rewrite its Move.toml to convert
    // literal `"0x0"` to `"_"`. Then pre-scan each rewritten Move.toml's
    // `[addresses]` block and assign a unique synthetic address to every
    // named address that's now `"_"`. The combined override map is fed
    // to every package's build so the IR sees consistent distinct
    // addresses across the workspace.
    let mut overrides: BTreeMap<String, AccountAddress> = BTreeMap::new();
    let mut next_synth: u128 = 0xff00_0000_0000_0001;
    let mut resolved_paths: Vec<std::path::PathBuf> = Vec::with_capacity(cfg.packages.len());
    for entry in &cfg.packages {
        let pkg_path = cfg.resolve_package_path(entry, &staged_inputs_owned)?;
        let move_toml = pkg_path.join("Move.toml");
        rewrite_addresses_to_underscore(&move_toml)?;
        let names = read_addresses_block(&move_toml)?;
        for (name, val) in names {
            if val != "_" {
                continue;
            }
            if overrides.contains_key(&name) {
                continue;
            }
            let addr = synthetic_address(next_synth);
            next_synth += 1;
            overrides.insert(name, addr);
        }
        resolved_paths.push(pkg_path);
    }

    // Phase 1b — load every package with the override map applied,
    // build the peer map keyed by address.
    let mut loaded: Vec<(move_bindgen::PackageEntry, Bindings)> =
        Vec::with_capacity(cfg.packages.len());
    let mut peers = PeerMap::new();
    for (entry, pkg_path) in cfg.packages.iter().zip(resolved_paths.iter()) {
        let opts = BuildOptions {
            additional_named_addresses: overrides.clone(),
            ..Default::default()
        };
        let bindings = move_bindgen::load_package_with_options(pkg_path, &opts)?;
        let addr = bindings_address(&bindings);
        peers.insert(addr, entry.crate_name())?;
        loaded.push((entry.clone(), bindings));
    }

    // Phase 2 — codegen each member crate, computing per-crate peer deps
    // by walking type references in its IR.
    std::fs::create_dir_all(&workspace_dir)?;
    let mut member_dirs = Vec::with_capacity(loaded.len());
    for (entry, bindings) in &loaded {
        let crate_name = entry.crate_name();
        let crate_dir = workspace_dir.join(&crate_name);

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

        // Each member's runtime path is relative to the member's own
        // `Cargo.toml` (one level deeper than the workspace root).
        let runtime = relativize_runtime(&cfg.runtime, &cfg.config_dir, &crate_dir);
        let skip_modules = framework_skip_modules(&bindings.package_name);
        let opts = move_bindgen::GenerateOptions {
            runtime,
            peers: peers.clone(),
            as_workspace_member: true,
            peer_deps,
            skip_modules,
            crate_name_override: Some(crate_name.clone()),
        };
        let crate_ = move_bindgen::generate(bindings, &opts)?;
        write_crate(&crate_dir, &crate_)?;
        member_dirs.push(crate_name);
    }

    // Phase 3 — workspace-level Cargo.toml.
    let runtime = relativize_runtime(&cfg.runtime, &cfg.config_dir, &workspace_dir);
    let workspace_cargo = render_workspace_cargo_toml(&member_dirs, &runtime);
    std::fs::write(workspace_dir.join("Cargo.toml"), workspace_cargo)?;
    std::fs::write(workspace_dir.join(".gitignore"), "/target\n")?;

    eprintln!(
        "wrote workspace to {} ({} crates)",
        workspace_dir.display(),
        member_dirs.len()
    );
    Ok(())
}

/// Recursively copy `src` to `dest`. Skips entries the build doesn't
/// need to see — `build/`, `target/`, `Move.lock` — to keep staging fast
/// and reproducible.
fn copy_dir_recursive(src: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "build"
            || name_str == "target"
            || name_str == ".git"
            || name_str == "Move.lock"
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

/// Rewrite a Move.toml's `[addresses]` block in place, converting any
/// `"0x0"` literal value to `"_"`. Other entries (real addresses, already
/// `_`, etc.) are left alone. Used in the staging copy only — the user's
/// originals are never touched.
fn rewrite_addresses_to_underscore(move_toml: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(move_toml)
        .with_context(|| format!("reading {}", move_toml.display()))?;
    let mut out = String::with_capacity(text.len());
    let mut current_section: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(name) = rest.strip_suffix(']') {
                current_section = Some(name.to_string());
            }
        }
        if current_section.as_deref() == Some("addresses") {
            if let Some(rewritten) = rewrite_zero_address_line(line) {
                out.push_str(&rewritten);
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(move_toml, out).with_context(|| format!("writing {}", move_toml.display()))?;
    Ok(())
}

fn rewrite_zero_address_line(line: &str) -> Option<String> {
    let leading_ws_end = line.find(|c: char| !c.is_whitespace())?;
    let (leading_ws, rest) = line.split_at(leading_ws_end);
    let (key, after_eq) = rest.split_once('=')?;
    let after = after_eq.trim();
    // Strip a possible trailing comment.
    let value_part = after
        .split_once('#')
        .map(|(v, _)| v.trim())
        .unwrap_or(after);
    if value_part != "\"0x0\"" {
        return None;
    }
    Some(format!("{leading_ws}{}= \"_\"", key.trim_end()))
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

fn read_addresses_block(move_toml: &Path) -> anyhow::Result<BTreeMap<String, String>> {
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

fn synthetic_address(n: u128) -> AccountAddress {
    let mut bytes = [0u8; 32];
    let n_bytes = n.to_be_bytes();
    bytes[16..32].copy_from_slice(&n_bytes);
    AccountAddress::new(bytes)
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
        let resolved = cfg.resolve_package_path(p, input_folders)?;
        println!(
            "    [{:>20}]  crate={:<28} path={}",
            p.id,
            p.crate_name(),
            resolved.display()
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
