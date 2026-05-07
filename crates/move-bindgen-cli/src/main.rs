use std::path::PathBuf;

use clap::{Parser, Subcommand};
use move_bindgen::Bindings;
use move_binary_format::file_format::Visibility;

#[derive(Parser, Debug)]
#[command(name = "move-bindgen", version, about = "Generate Rust bindings from a Move package")]
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Dump { package } => {
            let bindings = move_bindgen::load_package(&package)?;
            print_dump(&bindings);
        }
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
                    let fields: Vec<String> =
                        v.fields.iter().map(|f| format!("{}: {}", f.name, f.type_)).collect();
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
    if a.has_copy()  { out.push("copy");  }
    if a.has_drop()  { out.push("drop");  }
    if a.has_store() { out.push("store"); }
    if a.has_key()   { out.push("key");   }
    if out.is_empty() { "[]".into() } else { format!("[{}]", out.join(", ")) }
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
