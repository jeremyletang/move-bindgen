//! Workspace-level deployer codegen.
//!
//! In workspace mode the CLI generates one member crate per Move
//! package. This module adds an *additional* member crate —
//! `<workspace>-deploy` — that depends on every per-package member and
//! exposes a single `deploy_all` entry point. Topological order is
//! baked in at codegen time from each package's `dep_labels`; the
//! generated function chains `.resolve_from(...)` between steps so
//! workspace-internal deps are auto-patched without user input.
//!
//! Emitted only for the IOTA flavour for now; Sui workspaces still
//! ship a stub that panics via `unimplemented!` because the runtime's
//! `PackageDeployer::execute` does the same.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::config::{Flavour, PublishNetwork, RuntimeSpec};
use crate::ir::Bindings;

/// One member package's info, as the workspace-deployer codegen sees it.
#[derive(Debug, Clone)]
pub struct DeployMember {
    /// Canonical Move-source address name (e.g. `"fixed18"`). Used as
    /// the deploy-time map key. Skip members where this is empty —
    /// they can't be wired into the deployer (framework, or unusual
    /// `[addresses]` shape).
    pub address_name: String,
    /// Generated Rust crate name (e.g. `"fixed18-rs"`). Path-dep'd
    /// from the deployer crate and named in `use … as` aliases.
    pub crate_name: String,
    /// `move_name`s of this member's workspace-internal deps. Drives
    /// the topological sort and serves as a doc.
    pub direct_deps: Vec<String>,
    /// Number of root modules in the first publish artifact. Zero
    /// means the package's only sources are `#[test_only]` — the chain
    /// can't publish it, so it's excluded from `deploy_all`.
    pub module_count: usize,
}

/// Output of [`build`]: ready-to-write Cargo.toml + src/lib.rs for the
/// `<workspace>-deploy` crate.
#[derive(Debug)]
pub struct DeployCrate {
    /// The crate name (`<workspace>-deploy`).
    pub crate_name: String,
    /// Workspace-relative directory the crate gets written to.
    /// Same string as `crate_name` for path-dep symmetry.
    pub dir_name: String,
    pub cargo_toml: String,
    pub lib_rs: String,
}

/// Build the deployer crate for `members`. Returns `None` if there's
/// nothing useful to emit:
///   - no networks configured (no publish bytes anywhere), OR
///   - fewer than 1 deployable member (i.e. no `address_name`s set).
///
/// `workspace_name` is the parent workspace's name (e.g. `exchange-rs`);
/// the deploy crate is named `<workspace_name>-deploy`.
pub fn build(
    workspace_name: &str,
    members: &[DeployMember],
    networks: &[PublishNetwork],
    runtime: &RuntimeSpec,
    flavour: Flavour,
) -> Result<Option<DeployCrate>> {
    if networks.is_empty() {
        return Ok(None);
    }
    // Filter out:
    //  - members without a canonical address_name (framework / weird
    //    `[addresses]` shape) — we can't key them in the deploy map.
    //  - members whose root-module set is empty (`#[test_only]`-only
    //    packages) — the chain rejects empty publish commands and
    //    they'd just bail every workspace deploy with a "nothing to
    //    publish" error.
    let deployable: Vec<&DeployMember> = members
        .iter()
        .filter(|m| !m.address_name.is_empty() && m.module_count > 0)
        .collect();
    if deployable.is_empty() {
        return Ok(None);
    }

    // `-rs` is bindgen's per-package suffix ("Rust bindings of a Move
    // package"); the deployer isn't bindings, so strip it before
    // tacking on `-deploy`. `exchange-rs` → `exchange-deploy`,
    // `myws` → `myws-deploy`.
    let trimmed = workspace_name.strip_suffix("-rs").unwrap_or(workspace_name);
    let crate_name = format!("{trimmed}-deploy");
    let order = topo_sort(&deployable)?;

    let cargo_toml = render_cargo_toml(&crate_name, &deployable, runtime, flavour);
    let lib_rs = render_lib_rs(&crate_name, &order, networks)
        .context("rendering workspace deployer lib.rs")?;

    Ok(Some(DeployCrate {
        crate_name: crate_name.clone(),
        dir_name: crate_name,
        cargo_toml,
        lib_rs,
    }))
}

/// Build dependency-info per package from already-loaded bindings.
/// Edges = the union of every network artifact's `dep_labels` (since
/// labels reference Move address names, the same set across networks).
pub fn member_from_bindings(
    address_name: &str,
    crate_name: &str,
    bindings: &Bindings,
) -> DeployMember {
    let mut deps = BTreeSet::new();
    for artifact in &bindings.publish {
        for (_synth, name) in &artifact.dep_labels {
            deps.insert(name.clone());
        }
    }
    // Every network's artifact has the same module count for a given
    // package (publish builds differ only in chain_id + dep-address
    // overrides, not in source-set composition). Read it from the
    // first artifact, or 0 if there's no publish config at all.
    let module_count = bindings
        .publish
        .first()
        .map(|a| a.modules.len())
        .unwrap_or(0);
    DeployMember {
        address_name: address_name.to_string(),
        crate_name: crate_name.to_string(),
        direct_deps: deps.into_iter().collect(),
        module_count,
    }
}

/// Topologically sort members so each appears after every member it
/// depends on. Cycles (which shouldn't happen for a buildable workspace)
/// surface as a clear error rather than an infinite loop.
fn topo_sort<'a>(members: &[&'a DeployMember]) -> Result<Vec<&'a DeployMember>> {
    // Adjacency: package → set of names of packages it depends on.
    // Build a name→index map for quick lookups.
    let by_name: HashMap<&str, &DeployMember> = members
        .iter()
        .map(|m| (m.address_name.as_str(), *m))
        .collect();

    let mut visited: BTreeMap<&str, Visit> = BTreeMap::new();
    let mut order = Vec::new();
    for m in members {
        visit(m, &by_name, &mut visited, &mut order)?;
    }
    Ok(order)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visit {
    InProgress,
    Done,
}

fn visit<'a>(
    node: &'a DeployMember,
    by_name: &HashMap<&str, &'a DeployMember>,
    visited: &mut BTreeMap<&'a str, Visit>,
    order: &mut Vec<&'a DeployMember>,
) -> Result<()> {
    match visited.get(node.address_name.as_str()) {
        Some(Visit::Done) => return Ok(()),
        Some(Visit::InProgress) => {
            anyhow::bail!(
                "cycle in workspace dep graph involving package `{}`",
                node.address_name
            );
        }
        None => {}
    }
    visited.insert(node.address_name.as_str(), Visit::InProgress);
    for dep in &node.direct_deps {
        // Skip framework / canonical-address deps — they're never in
        // `by_name` (no synthetic, no member crate).
        if let Some(dep_node) = by_name.get(dep.as_str()) {
            visit(dep_node, by_name, visited, order)?;
        }
    }
    visited.insert(node.address_name.as_str(), Visit::Done);
    order.push(node);
    Ok(())
}

fn render_cargo_toml(
    crate_name: &str,
    members: &[&DeployMember],
    _runtime: &RuntimeSpec,
    flavour: Flavour,
) -> String {
    // Member path-deps. `../crate_name` since this crate lives at
    // workspace_dir/<crate_name>/, sibling to every member.
    let mut deps = String::new();
    for m in members {
        deps.push_str(&format!(
            "{} = {{ path = \"../{}\" }}\n",
            m.crate_name, m.crate_name,
        ));
    }
    // Workspace-shared deps the deployer pulls in.
    deps.push_str("move-bindgen-runtime.workspace = true\n");
    let extras = match flavour {
        Flavour::Iota => {
            "iota-sdk-types         = { git = \"https://github.com/iotaledger/iota-rust-sdk.git\", rev = \"dee951f490769f46c78ce84058f5d3cb8b606fd7\", default-features = false }\n\
             iota-sdk-crypto        = { git = \"https://github.com/iotaledger/iota-rust-sdk.git\", rev = \"dee951f490769f46c78ce84058f5d3cb8b606fd7\", default-features = false, features = [\"ed25519\", \"bech32\"] }\n\
             iota-sdk-graphql-client = { git = \"https://github.com/iotaledger/iota-rust-sdk.git\", rev = \"dee951f490769f46c78ce84058f5d3cb8b606fd7\", default-features = false }\n"
        }
        Flavour::Sui => "",
    };
    deps.push_str(extras);

    format!(
        "# @generated by move-bindgen — regenerate with `move-bindgen generate`.\n\
         \n\
         [package]\n\
         name = \"{crate_name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         publish = false\n\
         description = \"Workspace-level deployer for the bindings generated by move-bindgen.\"\n\
         \n\
         [dependencies]\n\
         {deps}\
         \n\
         [lints]\n\
         workspace = true\n",
    )
}

fn render_lib_rs(
    _crate_name: &str,
    order: &[&DeployMember],
    networks: &[PublishNetwork],
) -> Result<String> {
    let net_variants: Vec<_> = networks
        .iter()
        .map(|n| format_ident!("{}", to_pascal_case(&n.name)))
        .collect();
    let net_docs: Vec<_> = networks
        .iter()
        .map(|n| format!("Deploy against the `{}` network.", n.name))
        .collect();

    // Per-member-crate Network → workspace Network conversion arms.
    // Every member has the same publish-networks set (driven by the
    // global `[publish]` block), so each member's `Network` enum has
    // the same variants — just under a different type name.
    let convert_arms_per_member: Vec<Vec<TokenStream>> = order
        .iter()
        .map(|m| {
            let member_alias = format_ident!("{}", crate_path_alias(&m.crate_name));
            networks
                .iter()
                .map(|n| {
                    let v = format_ident!("{}", to_pascal_case(&n.name));
                    quote! { Network::#v => #member_alias::Network::#v }
                })
                .collect::<Vec<_>>()
        })
        .collect();

    let steps: Vec<TokenStream> = order
        .iter()
        .zip(convert_arms_per_member.iter())
        .map(|(m, arms)| {
            let address_name_lit = m.address_name.as_str();
            let display_name = m.crate_name.as_str();
            let member_alias = format_ident!("{}", crate_path_alias(&m.crate_name));
            quote! {
                {
                    if let Some(log) = self.log.as_deref() {
                        log(&format!("[deploy] starting `{}`", #display_name));
                    }
                    let mut deployer = #member_alias::Package::deployer(match self.network {
                        #( #arms ),*
                    })
                        .resolve_from(&artifacts.packages)
                        .sender(sender)
                        .with_client(client.clone())
                        .with_signer(signer.clone())
                        .with_auto_gas();
                    if let Some(b) = self.gas_budget {
                        // Explicit budget skips the per-step dry-run
                        // probe entirely (see `PackageDeployer`).
                        deployer = deployer.gas_budget(b);
                    }
                    if let Some(log) = self.log.clone() {
                        deployer = deployer.with_log(move |s| log(s));
                    }
                    match deployer.execute().await {
                        Ok(r) => {
                            let created = collect_created_objects(&r.effects);
                            artifacts.packages.insert(#address_name_lit, r.package_id);
                            artifacts.created_objects.insert(#address_name_lit, created.clone());
                            artifacts.steps.push(DeployStep {
                                address_name: #address_name_lit,
                                crate_name: #display_name,
                                package_id: r.package_id,
                                digest: r.effects.as_v1().transaction_digest,
                                created_objects: created,
                            });
                            // Wait for the indexer to ingest this tx's
                            // effects before the next deploy lists gas
                            // coins — otherwise we hit a stale-version
                            // error on the same coin we just consumed.
                            if let Err(e) = client
                                .wait_for_effects(&r.effects, Default::default())
                                .await
                            {
                                return Err(DeployAllError {
                                    failed_address_name: #address_name_lit,
                                    failed_crate_name: #display_name,
                                    error: ExecuteError::Finish(format!(
                                        "wait_for_effects after `{}`: {e}", #display_name,
                                    )),
                                    partial: artifacts,
                                });
                            }
                        }
                        Err(error) => {
                            return Err(DeployAllError {
                                failed_address_name: #address_name_lit,
                                failed_crate_name: #display_name,
                                error,
                                partial: artifacts,
                            });
                        }
                    }
                }
            }
        })
        .collect();

    let tokens = quote! {
        //! @generated by move-bindgen — do not edit by hand.
        //!
        //! Workspace-level deployer. One call deploys every member
        //! package in topological order, auto-resolving each package's
        //! workspace-internal deps from the accumulated address map.
        #![allow(unused_imports, clippy::needless_borrow)]

        use std::collections::HashMap;

        use move_bindgen_runtime::{Address, ClientExt, ExecuteError};

        /// Networks the workspace's publish-bytes were compiled for.
        /// Mirrors each member crate's `Network` enum — they all share
        /// the same set since `[publish]` is workspace-global.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Network {
            #( #[doc = #net_docs] #net_variants ),*
        }

        /// Per-package outcome of one successful publish.
        #[derive(Debug, Clone)]
        pub struct DeployStep {
            /// Canonical Move-source address name (e.g. `"fixed18"`).
            pub address_name: &'static str,
            /// Generated Rust crate name (e.g. `"fixed18-rs"`).
            pub crate_name: &'static str,
            /// Object id of the newly-published package.
            pub package_id: Address,
            /// Digest of the publish transaction — lets callers build
            /// a complete audit log of every tx the deploy submitted
            /// without re-querying the indexer.
            pub digest: iota_sdk_types::Digest,
            /// Non-package objects created during the publish — at
            /// minimum the `UpgradeCap`, plus anything the package's
            /// `init` function created and shared/transferred.
            pub created_objects: Vec<CreatedObject>,
        }

        /// A non-package object created during a publish PTB. The
        /// owner info comes straight from the transaction effects;
        /// fetch the object via the client if you need its type tag
        /// or contents.
        #[derive(Debug, Clone, Copy)]
        pub struct CreatedObject {
            pub id: Address,
        }

        /// Per-workspace deploy artefacts.
        #[derive(Debug, Default)]
        pub struct Artifacts {
            /// `address_name → on-chain package id` for every
            /// successfully-published package.
            pub packages: HashMap<&'static str, Address>,
            /// `address_name → created objects` for every successfully-
            /// published package (UpgradeCap and anything the
            /// package's `init` created).
            pub created_objects: HashMap<&'static str, Vec<CreatedObject>>,
            /// Detailed step trace, in deploy order.
            pub steps: Vec<DeployStep>,
        }

        /// Error from [`deploy_all`]: holds the failing package's
        /// info, the underlying publish error, and the partial
        /// artifacts collected up to the failure (so callers can
        /// inspect what got deployed before bailing).
        #[derive(Debug)]
        pub struct DeployAllError {
            pub failed_address_name: &'static str,
            pub failed_crate_name: &'static str,
            pub error: ExecuteError,
            pub partial: Artifacts,
        }

        impl std::fmt::Display for DeployAllError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "deploy_all failed at `{}` ({} packages deployed before): {}",
                    self.failed_crate_name,
                    self.partial.packages.len(),
                    self.error,
                )
            }
        }

        impl std::error::Error for DeployAllError {}

        /// Configurable workspace deploy. Build with [`DeployAll::new`],
        /// optionally attach a log sink, then call [`Self::run`].
        ///
        /// ```ignore
        /// // Silent:
        /// let artifacts = DeployAll::new(Network::Testnet)
        ///     .run(sender, &client, &signer).await?;
        ///
        /// // With per-step trace on stderr:
        /// let artifacts = DeployAll::new(Network::Testnet)
        ///     .with_log(|s| eprintln!("{s}"))
        ///     .run(sender, &client, &signer).await?;
        /// ```
        pub struct DeployAll {
            network: Network,
            log: Option<std::sync::Arc<dyn Fn(&str) + Send + Sync>>,
            gas_budget: Option<u64>,
        }

        impl DeployAll {
            pub fn new(network: Network) -> Self {
                Self { network, log: None, gas_budget: None }
            }

            /// Attach a log sink. The callback receives one line per
            /// step start, plus the deployer's per-step gas-estimation
            /// and dep-patching trace.
            pub fn with_log<F>(mut self, f: F) -> Self
            where
                F: Fn(&str) + Send + Sync + 'static,
            {
                self.log = Some(std::sync::Arc::new(f));
                self
            }

            /// Set an explicit per-step gas budget (in nanos), applied
            /// to **every** package publish. Skips each step's dry-run
            /// gas probe — useful when the deployer wallet is too thin
            /// for the probe, or when the cost profile is already
            /// known. Size it for the most expensive package in the
            /// workspace: unused budget isn't charged, but every step
            /// requires the gas coin to cover the full value.
            pub fn with_gas_budget(mut self, b: u64) -> Self {
                self.gas_budget = Some(b);
                self
            }

            /// Deploy every workspace package on the configured
            /// network, in topological dependency order. Each step's
            /// `PackageDeployer` is configured with `sender` /
            /// `client` / `signer` / auto-gas and resolves its
            /// workspace-internal deps from the running
            /// `Artifacts.packages` map.
            ///
            /// Returns the full [`Artifacts`] on success, or
            /// [`DeployAllError`] on the first failure (with the
            /// partial state collected before the error).
            pub async fn run<S>(
                self,
                sender: Address,
                client: &iota_sdk_graphql_client::Client,
                signer: &S,
            ) -> Result<Artifacts, DeployAllError>
            where
                S: iota_sdk_crypto::IotaSigner + Send + Sync + Clone + 'static,
            {
                let mut artifacts = Artifacts::default();
                #( #steps )*
                Ok(artifacts)
            }
        }

        /// Convenience shorthand for `DeployAll::new(network).run(...)`.
        /// Silent — attach a log sink via the builder for the per-step
        /// trace.
        pub async fn deploy_all<S>(
            network: Network,
            sender: Address,
            client: &iota_sdk_graphql_client::Client,
            signer: &S,
        ) -> Result<Artifacts, DeployAllError>
        where
            S: iota_sdk_crypto::IotaSigner + Send + Sync + Clone + 'static,
        {
            DeployAll::new(network).run(sender, client, signer).await
        }

        /// Walk the publish transaction's effects and pull out every
        /// non-package object it created. Used by [`deploy_all`] to
        /// fill `Artifacts.created_objects` — the UpgradeCap will
        /// always be in here.
        fn collect_created_objects(
            effects: &iota_sdk_types::TransactionEffects,
        ) -> Vec<CreatedObject> {
            use iota_sdk_types::ObjectOut;
            let mut out = Vec::new();
            for ch in &effects.as_v1().changed_objects {
                if matches!(ch.output_state, ObjectOut::ObjectWrite { .. }) {
                    out.push(CreatedObject {
                        id: Address::from(ch.object_id),
                    });
                }
            }
            out
        }
    };
    let file: syn::File =
        syn::parse2(tokens).context("parsing workspace-deployer TokenStream as syn::File")?;
    Ok(prettyplease::unparse(&file))
}

/// `foo-rs-bar` → `Foo_rsBar` is awkward; this just lowercases and
/// kebab→underscore so `foo-rs` becomes the path-segment `foo_rs`
/// (Cargo's auto-generated extern crate identifier).
fn crate_path_alias(crate_name: &str) -> String {
    crate_name
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .collect()
}

fn to_pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = true;
    for c in s.chars() {
        if c == '-' || c == '_' {
            up = true;
            continue;
        }
        if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PublishNetwork;

    fn fake_members() -> Vec<DeployMember> {
        vec![
            DeployMember {
                address_name: "fixed18".into(),
                crate_name: "fixed18-rs".into(),
                direct_deps: vec![],
                module_count: 1,
            },
            DeployMember {
                address_name: "exchange".into(),
                crate_name: "exchange-rs".into(),
                direct_deps: vec!["fixed18".into()],
                module_count: 3,
            },
        ]
    }

    fn render() -> DeployCrate {
        let networks = vec![PublishNetwork {
            name: "testnet".into(),
            chain_id: None,
            addresses: Default::default(),
        }];
        build(
            "exchange-rs",
            &fake_members(),
            &networks,
            &RuntimeSpec::default_git(),
            Flavour::Iota,
        )
        .expect("build should succeed")
        .expect("two deployable members should emit a crate")
    }

    // `render_lib_rs` round-trips its TokenStream through
    // `syn::parse2`, so reaching these assertions proves the template
    // is syntactically valid Rust. The `contains` checks pin the
    // surface added for the BUGS/ reports: per-step digests and the
    // explicit gas-budget escape hatch.
    #[test]
    fn deploy_step_carries_publish_digest() {
        let lib = render().lib_rs;
        assert!(lib.contains("pub digest: iota_sdk_types::Digest"));
        assert!(lib.contains("digest: r.effects.as_v1().transaction_digest"));
    }

    #[test]
    fn deploy_all_exposes_gas_budget_knob() {
        let lib = render().lib_rs;
        assert!(lib.contains("pub fn with_gas_budget(mut self, b: u64) -> Self"));
        assert!(lib.contains("deployer = deployer.gas_budget(b);"));
    }

    #[test]
    fn deploy_order_is_topological() {
        let lib = render().lib_rs;
        let fixed18 = lib
            .find(r#"crate_name: "fixed18-rs""#)
            .expect("fixed18 step");
        let exchange = lib
            .find(r#"crate_name: "exchange-rs""#)
            .expect("exchange step");
        assert!(
            fixed18 < exchange,
            "fixed18 (dependency) must deploy before exchange"
        );
    }
}
