//! `move-bindgen` — generate Rust bindings for a Move package.
//!
//! ```text
//! Move package path  ──►  CompiledPackage  ──►  Bindings  ──►  Rust source
//!                          (build.rs)          (ir.rs)        (codegen)
//! ```

mod build;
mod codegen;
mod config;
mod docs;
mod ir;

pub use build::BuildOptions;
pub use codegen::{generate, GenerateOptions, GeneratedCrate};
pub use config::{config_path_in, Config, OutputFormat, PackageEntry, RuntimeSpec};
pub use docs::DocMap;
pub use ir::{load_package, load_package_with_options, Bindings};
