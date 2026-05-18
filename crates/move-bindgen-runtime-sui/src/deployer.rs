//! Sui-side stub of the IOTA `PackageDeployer` surface.
//!
//! Codegen emits the same `Package::deployer(Network)` entry point for
//! Sui packages as for IOTA. We mirror the IOTA runtime's
//! `PackageDeployer` / `UpgradePolicy` / `DeployResult` types here so
//! the generated code compiles. The runtime itself is not yet wired —
//! [`PackageDeployer::execute`] panics with a clear `unimplemented!`.
//!
//! When Sui deploy is wired up, replace the `unimplemented!` body with
//! a real publish PTB. The shape of this module matches the IOTA side
//! by intent — keep them in sync.

use crate::{Address, ObjectReference, TransactionEffects};

const NOT_YET_IMPLEMENTED: &str =
    "Sui package deployment via move-bindgen is not yet implemented. \
     The codegen emits `Package::deployer(Network::...)` for parity \
     with the IOTA flavour, but the runtime side hasn't been wired. \
     Until then: build with `sui client publish` and register the \
     resulting address via `PtbBuilder::with_package::<Package>(addr)`.";

/// What to do with the `UpgradeCap` returned by `publish`. Matches the
/// IOTA runtime's variant set so generated code can pass policies
/// across flavours uniformly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradePolicy {
    /// Transfer the `UpgradeCap` to `recipient` in the same PTB.
    Transfer(Address),
    /// Call `sui::package::make_immutable` on the cap.
    MakeImmutable,
}

/// Result of a successful publish — mirrors the IOTA shape. Not
/// currently produced (the Sui side panics in `execute`).
#[derive(Debug)]
pub struct DeployResult {
    pub package_id: Address,
    pub effects: TransactionEffects,
    pub cache: crate::ObjectCache,
}

/// Chainable builder produced by generated `Package::deployer(network)`
/// — Sui flavour. Carries the bytecode tables from
/// `bytecode/<network>.rs` but does **not** execute them yet.
pub struct PackageDeployer {
    #[allow(dead_code)]
    modules: &'static [&'static [u8]],
    #[allow(dead_code)]
    dependencies: &'static [Address],
    #[allow(dead_code)]
    digest: [u8; 32],
    #[allow(dead_code)]
    dep_labels: &'static [(Address, &'static str)],
    sender: Option<Address>,
    auto_gas: bool,
    gas_objects: Vec<ObjectReference>,
    gas_price: Option<u64>,
    gas_budget: Option<u64>,
    policy: Option<UpgradePolicy>,
}

impl PackageDeployer {
    pub fn new(
        modules: &'static [&'static [u8]],
        dependencies: &'static [Address],
        digest: [u8; 32],
        dep_labels: &'static [(Address, &'static str)],
    ) -> Self {
        Self {
            modules,
            dependencies,
            digest,
            dep_labels,
            sender: None,
            auto_gas: false,
            gas_objects: Vec::new(),
            gas_price: None,
            gas_budget: None,
            policy: None,
        }
    }

    pub fn dep_labels(&self) -> &'static [(Address, &'static str)] {
        self.dep_labels
    }

    /// Source-level parity with the IOTA side. No-op in the Sui stub
    /// since `execute()` panics anyway.
    pub fn resolve_dep(self, _name: impl Into<String>, _real_address: Address) -> Self {
        self
    }

    /// Source-level parity with the IOTA side. No-op in the Sui stub.
    pub fn resolve_from(
        self,
        _resolved: &std::collections::HashMap<&'static str, Address>,
    ) -> Self {
        self
    }

    pub fn sender(mut self, addr: Address) -> Self {
        self.sender = Some(addr);
        self
    }

    /// Accept-anything signature — kept for source-level parity with
    /// the IOTA side. Discards the value; `execute()` panics anyway.
    pub fn with_submitter<T>(self, _s: T) -> Self {
        self
    }
    pub fn with_signer<T>(self, _s: T) -> Self {
        self
    }
    pub fn with_gas_oracle<T>(self, _o: T) -> Self {
        self
    }
    pub fn with_dry_runner<T>(self, _d: T) -> Self {
        self
    }
    pub fn with_client<T>(self, _c: T) -> Self {
        self
    }

    pub fn with_auto_gas(mut self) -> Self {
        self.auto_gas = true;
        self
    }

    pub fn gas(mut self, refs: impl IntoIterator<Item = ObjectReference>) -> Self {
        self.gas_objects = refs.into_iter().collect();
        self
    }

    pub fn gas_price(mut self, p: u64) -> Self {
        self.gas_price = Some(p);
        self
    }

    pub fn gas_budget(mut self, b: u64) -> Self {
        self.gas_budget = Some(b);
        self
    }

    pub fn with_policy(mut self, p: UpgradePolicy) -> Self {
        self.policy = Some(p);
        self
    }

    pub async fn execute(self) -> Result<DeployResult, crate::ExecuteError> {
        unimplemented!("{NOT_YET_IMPLEMENTED}")
    }

    pub async fn execute_with<S>(self, _signer: &S) -> Result<DeployResult, crate::ExecuteError> {
        unimplemented!("{NOT_YET_IMPLEMENTED}")
    }
}
