use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use move_binary_format::file_format::Visibility;
use move_bindgen::{config_path_in, Bindings, Config, OutputFormat, RuntimeSpec};

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
            let opts = move_bindgen::GenerateOptions { runtime };
            let crate_ = move_bindgen::generate(&bindings, &opts)?;
            write_crate(&out_dir, &crate_)?;
            eprintln!("wrote crate to {}", out_dir.display());
        }
        OutputFormat::Workspace => {
            anyhow::bail!("workspace mode is not yet wired up — codegen step still pending");
        }
    }
    Ok(())
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
