//! `move-bindgen` — generate Rust bindings for a Move package.
//!
//! ```text
//! Move package path  ──►  CompiledPackage  ──►  Bindings  ──►  Rust source
//!                          (build.rs)          (ir.rs)        (codegen)
//! ```

mod build;
mod clean;
mod codegen;
mod config;
mod digest;
mod docs;
mod git_resolver;
mod init;
mod install;
mod install_manifest;
mod ir;
mod peer;
mod reporter;

pub use build::BuildOptions;
pub use clean::{run as clean, CleanOutcome};
pub use codegen::{generate, GenerateOptions, GeneratedCrate, PeerDep};
pub use config::{
    config_path_in, staging_dir_for, Config, Flavour, OutputFormat, PackageEntry, PackageSource,
    RuntimeSpec, DEFAULT_RUNTIME_GIT_URL,
};
pub use digest::{file_digest, source_dir_digest};
pub use docs::DocMap;
pub use git_resolver::resolve_git_source;
pub use init::{run as init, InitOptions};
pub use install::{run as install, verify_freshness};
pub use install_manifest::{
    InstallManifest, SerializableSource, StagedPackage, MANIFEST_FILENAME, MANIFEST_VERSION,
};
pub use ir::{load_package, load_package_with_options, Bindings};
pub use peer::{foreign_addresses_used, InsertOutcome, PeerEntry, PeerMap};
pub use reporter::{Reporter, ReporterMode};
