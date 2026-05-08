use std::path::PathBuf;

use clap::{Parser, Subcommand};
use move_binary_format::file_format::Visibility;
use move_bindgen::{config_path_in, Bindings, Config, OutputFormat};

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
    /// Output is a directory containing `Cargo.toml`, `src/lib.rs`, and one
    /// `src/<module>.rs` per Move module that has datatypes. Defaults to
    /// `<package>-rs` next to the Move package directory.
    Generate {
        /// Path to the Move package (directory containing `Move.toml`).
        package: PathBuf,
        /// Output directory. Defaults to `<package>-rs` sibling to `package`.
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
        /// Path-dependency to use for `move-bindgen-runtime` in the
        /// generated `Cargo.toml`. Default targets the in-tree runtime.
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
            out,
            runtime_path,
        } => {
            let bindings = move_bindgen::load_package(&package)?;
            let opts = move_bindgen::GenerateOptions { runtime_path };
            let crate_ = move_bindgen::generate(&bindings, &opts)?;
            let out_dir = out.unwrap_or_else(|| default_out_dir(&package, &crate_.crate_name));
            write_crate(&out_dir, &crate_)?;
            eprintln!("wrote crate to {}", out_dir.display());
        }
        Cmd::Check {
            config,
            input_folders,
        } => {
            let config_path =
                config.unwrap_or_else(|| config_path_in(std::path::Path::new(".")));
            let cfg = Config::load(&config_path)?;
            check_config(&cfg, &input_folders)?;
        }
    }
    Ok(())
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
