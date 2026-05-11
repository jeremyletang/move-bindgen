//! Bindings → a complete generated Rust crate.

mod constant;
mod datatype;
mod function;
mod ty;

use anyhow::{Context, Result};
use move_core_types::account_address::AccountAddress;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::Bindings;

use ty::TypeCtx;

/// Emit `#[doc = "..."]` attributes — one per line of `s` so prettyplease
/// renders them as `///` lines instead of folding into a `/** ... */` block.
pub(crate) fn outer_doc(s: &str) -> TokenStream {
    let mut out = TokenStream::new();
    for line in s.split('\n') {
        out.extend(quote!(#[doc = #line]));
    }
    out
}

/// Inner-attribute (`#![doc = "..."]`) flavour for module-level docs.
pub(crate) fn inner_doc(s: &str) -> TokenStream {
    let mut out = TokenStream::new();
    for line in s.split('\n') {
        out.extend(quote!(#![doc = #line]));
    }
    out
}

/// Files of a generated Rust crate, ready to be written to disk.
#[derive(Debug)]
pub struct GeneratedCrate {
    /// Crate name (e.g. `counter-rs`).
    pub crate_name: String,
    /// Original Move package name (e.g. `counter`).
    pub package_name: String,
    /// `Cargo.toml` source.
    pub cargo_toml: String,
    /// `src/lib.rs` source.
    pub lib_rs: String,
    /// `(file_name, source)` pairs for `src/<file_name>` — one per Move
    /// module that has datatypes.
    pub module_files: Vec<(String, String)>,
}

/// Knobs for `generate`.
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Move chain flavour the bindings target. Drives the runtime
    /// crate name in the generated `Cargo.toml`'s `package = "..."`
    /// alias.
    pub flavour: crate::config::Flavour,
    /// How the generated `Cargo.toml` should reference
    /// `move-bindgen-runtime`. For path specs, the caller is responsible
    /// for handing in a path that's already relative to the output
    /// directory (e.g. by re-relativizing a config-relative path).
    pub runtime: crate::config::RuntimeSpec,
    /// Peer-package address map. Empty in single-crate mode; populated
    /// in workspace mode so cross-package datatype refs resolve.
    pub peers: crate::PeerMap,
    /// Whether the generated crate is a member of a Cargo workspace.
    /// When true, the generated `Cargo.toml` uses `runtime.workspace =
    /// true` and inherits `serde` / `bcs` from the workspace.
    pub as_workspace_member: bool,
    /// Path-deps to add to this crate's `[dependencies]` block, in
    /// addition to `move-bindgen-runtime`. Each entry is `(crate_name,
    /// rel_path)` where `rel_path` is relative to the generated
    /// `Cargo.toml`. Workspace-mode only.
    pub peer_deps: Vec<PeerDep>,
    /// Module names to skip during codegen — no `<mod>.rs` is written
    /// and no entry is added to `lib.rs`. Used for framework modules
    /// whose types are owned by the runtime (`iota::object`,
    /// `std::option`, `std::string`, `std::ascii`); ty.rs's well-known
    /// mappings route references to those types into the runtime
    /// instead.
    pub skip_modules: std::collections::BTreeSet<String>,
    /// Override for the generated crate name. When `None`, defaults to
    /// `<bindings.package_name>-rs`. The workspace driver passes the
    /// config's `crate_name` here so member crate names line up with the
    /// peer-dep path entries.
    pub crate_name_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PeerDep {
    pub crate_name: String,
    pub rel_path: std::path::PathBuf,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            flavour: crate::config::Flavour::default(),
            runtime: crate::config::RuntimeSpec::Path(std::path::PathBuf::from(
                "../../crates/move-bindgen-runtime-iota",
            )),
            peers: crate::PeerMap::new(),
            as_workspace_member: false,
            peer_deps: Vec::new(),
            skip_modules: std::collections::BTreeSet::new(),
            crate_name_override: None,
        }
    }
}

pub fn generate(bindings: &Bindings, opts: &GenerateOptions) -> Result<GeneratedCrate> {
    // The address the modules were *compiled* against (the synthetic
    // override from `additional_named_addresses`). We use it only for
    // codegen-time equality checks ("is this type ref intra-package?"),
    // never for emitting an on-chain `PACKAGE_ID` — that's now resolved
    // at runtime via `b.package_id::<super::Package>()`.
    let build_addr = bindings
        .modules
        .first()
        .map(|m| m.id.address)
        .unwrap_or(AccountAddress::ZERO);

    let crate_name = opts
        .crate_name_override
        .clone()
        .unwrap_or_else(|| format!("{}-rs", bindings.package_name));

    // Per-module sources. Skip modules with no items to emit — they'd
    // produce empty .rs files.
    let mut module_files = Vec::new();
    let mut module_names = Vec::new();
    // Modules whose codegen emitted a `ModuleAt` wrapper (i.e. they
    // had at least one non-generic datatype). Used to populate
    // `PackageAt`'s per-module methods.
    let mut modules_with_at = Vec::new();
    for (i, m) in bindings.modules.iter().enumerate() {
        if opts.skip_modules.contains(m.id.name.as_str()) {
            continue;
        }
        let ctx = TypeCtx {
            build_addr,
            current_module: &m.id.name,
            docs: &bindings.docs,
            peers: &opts.peers,
        };
        let names = bindings
            .constant_names
            .get(i)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let mut body = constant::emit_constants(&m.constants, &names.to_vec(), &ctx)
            .with_context(|| format!("constant codegen for module {}", m.id.name))?;
        let (dt_tokens, non_generic) = datatype::emit_datatypes(m, &ctx)
            .with_context(|| format!("datatype codegen for module {}", m.id.name))?;
        body.extend(dt_tokens);
        body.extend(
            function::emit_functions(m, &ctx)
                .with_context(|| format!("function codegen for module {}", m.id.name))?,
        );
        // Append the per-module `ModuleAt` wrapper, if any.
        body.extend(datatype::emit_module_at(&non_generic));
        if body.is_empty() {
            continue;
        }
        let module_doc = bindings.docs.module(m.id.name.as_str());
        let module_source = render_module(body, module_doc)?;
        let mod_name = m.id.name.as_str().to_string();
        module_files.push((format!("{mod_name}.rs"), module_source));
        if !non_generic.is_empty() {
            modules_with_at.push(mod_name.clone());
        }
        module_names.push(mod_name);
    }

    let lib_rs = render_lib_rs(&module_names, &modules_with_at)?;
    let cargo_toml = render_cargo_toml(
        &crate_name,
        &bindings.package_name,
        &opts.runtime,
        opts.flavour,
        opts.as_workspace_member,
        &opts.peer_deps,
    );

    Ok(GeneratedCrate {
        crate_name,
        package_name: bindings.package_name.clone(),
        cargo_toml,
        lib_rs,
        module_files,
    })
}

fn render_module(body: TokenStream, module_doc: Option<&str>) -> Result<String> {
    let doc_attr = module_doc.map(inner_doc).unwrap_or_default();
    let header = quote! {
        // @generated by move-bindgen — do not edit by hand.
        #![allow(unused_imports, non_snake_case, non_upper_case_globals)]
        #doc_attr

        use move_bindgen_runtime::*;
        use serde::{Deserialize, Serialize};
        use std::marker::PhantomData;

        #body
    };
    let file: syn::File =
        syn::parse2(header).context("parsing generated module TokenStream as syn::File")?;
    Ok(prettyplease::unparse(&file))
}

fn render_lib_rs(modules: &[String], modules_with_at: &[String]) -> Result<String> {
    let mod_decls = modules.iter().map(|n| {
        let ident = format_ident!("{}", n);
        quote! { pub mod #ident; }
    });
    let pkg_at_methods = modules_with_at.iter().map(|n| {
        let ident = format_ident!("{}", n);
        quote! {
            pub fn #ident(&self) -> #ident::ModuleAt {
                #ident::ModuleAt { package: self.addr }
            }
        }
    });

    let lib = quote! {
        // @generated by move-bindgen — do not edit by hand.
        #![allow(unused_imports, non_snake_case, clippy::too_many_arguments)]

        use move_bindgen_runtime::Address;

        #( #mod_decls )*

        /// Marker type identifying this package. Register the package's
        /// on-chain address before issuing any PTB call:
        ///
        /// ```ignore
        /// let mut b = PtbBuilder::new(sender);
        /// b.with_package::<Package>(my_published_address);
        /// // ...generated calls now resolve `my_published_address`.
        /// ```
        ///
        /// Same marker is accepted by the free-standing
        /// `PackageRegistry::at::<Package>(addr)` for non-PTB callers
        /// (event decoders, BCS deserialization, etc.).
        pub struct Package;

        impl Package {
            /// Bind the package to a runtime address and get a chainable
            /// handle:
            ///
            /// ```ignore
            /// let pkg = Package::at(my_addr);
            /// let tag = pkg.counter().counter_tag();
            /// ```
            pub fn at(addr: Address) -> PackageAt {
                PackageAt { addr }
            }
        }

        /// Read-only handle bound to a runtime package address. Use
        /// the per-module accessors to navigate to a [`TypeTag`] without
        /// spinning up a `PtbBuilder`.
        pub struct PackageAt {
            addr: Address,
        }

        impl PackageAt {
            #( #pkg_at_methods )*
        }
    };
    let file: syn::File =
        syn::parse2(lib).context("parsing generated lib.rs TokenStream as syn::File")?;
    Ok(prettyplease::unparse(&file))
}

fn render_cargo_toml(
    crate_name: &str,
    package_name: &str,
    runtime: &crate::config::RuntimeSpec,
    flavour: crate::config::Flavour,
    as_workspace_member: bool,
    peer_deps: &[PeerDep],
) -> String {
    let header = format!(
        "# @generated by move-bindgen — regenerate with `move-bindgen generate`.\n\
         \n\
         [package]\n\
         name = \"{crate_name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         description = \"Generated bindings for the `{package_name}` Move package.\"\n\
         \n\
         [dependencies]\n",
    );

    let mut deps = String::new();
    if as_workspace_member {
        deps.push_str("move-bindgen-runtime.workspace = true\n");
        deps.push_str("serde.workspace = true\n");
        deps.push_str("bcs.workspace = true\n");
    } else {
        deps.push_str(&render_runtime_dep_line(runtime, flavour));
        deps.push('\n');
        deps.push_str("serde = { version = \"1\", features = [\"derive\"] }\n");
        deps.push_str("bcs   = \"0.1\"\n");
    }
    for p in peer_deps {
        deps.push_str(&format!(
            "{} = {{ path = \"{}\" }}\n",
            p.crate_name,
            p.rel_path.display()
        ));
    }

    let footer = if as_workspace_member {
        String::new()
    } else {
        "\n# Detach this crate from any parent workspace it might be generated inside.\n\
         [workspace]\n"
            .to_string()
    };

    format!("{header}{deps}{footer}")
}

/// Crate name the local `move-bindgen-runtime` alias resolves to,
/// based on flavour. Generated `Cargo.toml`s use this in
/// `package = "..."` so the `use move_bindgen_runtime::*;` import in
/// generated source stays flavour-agnostic — only the alias target
/// switches between flavours.
fn runtime_package_name(flavour: crate::config::Flavour) -> &'static str {
    match flavour {
        crate::config::Flavour::Iota => "move-bindgen-runtime-iota",
        crate::config::Flavour::Sui => "move-bindgen-runtime-sui",
    }
}

fn render_runtime_dep_line(
    spec: &crate::config::RuntimeSpec,
    flavour: crate::config::Flavour,
) -> String {
    use crate::config::RuntimeSpec;
    let pkg = runtime_package_name(flavour);
    match spec {
        RuntimeSpec::Path(p) => format!(
            "move-bindgen-runtime = {{ package = \"{pkg}\", path = \"{}\" }}",
            p.display()
        ),
        RuntimeSpec::Version(v) => {
            format!("move-bindgen-runtime = {{ package = \"{pkg}\", version = \"{v}\" }}")
        }
        RuntimeSpec::Git {
            url,
            rev,
            branch,
            tag,
        } => {
            let mut parts = vec![format!("package = \"{pkg}\""), format!("git = \"{url}\"")];
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
