//! `PackageDeployer` — publishes a Move package whose bytecode was
//! baked into the generated bindings at codegen time.
//!
//! Generated `lib.rs` exposes the entry point as `Package::deployer(net)`;
//! that constructs one of these with the `bytecode/<network>.rs` data.
//! The chainable builder mirrors [`crate::PtbBuilder`]'s
//! `with_client`/`with_signer`/`with_auto_gas`/`gas*` shape.
//!
//! After [`PackageDeployer::execute`] the new package ID is recovered
//! from the transaction effects' `PackageWrite` entry — no extra
//! fetch needed for it. The `UpgradeCap` object is handled per the
//! configured [`UpgradePolicy`] (default: transfer to sender).

use iota_sdk_transaction_builder::{assigned, TransactionBuilder};
use iota_sdk_types::{
    Address, Digest, MovePackageData, ObjectId, ObjectOut, ObjectReference, TransactionEffects,
};

use crate::{
    DryRunner, DynSigner, ExecuteError, GasOracle, ObjectCache, Submitter, IOTA_FRAMEWORK_ADDRESS,
};

/// What to do with the `UpgradeCap` returned by `publish`. Mirrors
/// `iota::package`'s on-chain entry points. Defaults to
/// [`UpgradePolicy::Transfer`] to the deploy sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradePolicy {
    /// Transfer the `UpgradeCap` to `recipient` in the same PTB.
    /// Most packages want this with `recipient = sender` so they can
    /// authorize future upgrades themselves.
    Transfer(Address),
    /// Burn upgrade rights by calling `iota::package::make_immutable`.
    /// The package becomes frozen — no future upgrades possible.
    MakeImmutable,
}

/// Outcome of a successful [`PackageDeployer::execute`] call.
#[derive(Debug)]
pub struct DeployResult {
    /// Object id of the freshly-published package.
    pub package_id: Address,
    /// Full transaction effects, for callers who need owner/version
    /// info on the `UpgradeCap` or anything else.
    pub effects: TransactionEffects,
    /// Object cache as built up during the PTB. Empty on this codepath
    /// (publish doesn't touch existing objects), but returned for
    /// symmetry with [`crate::PtbBuilder::execute`] — callers can
    /// `.with_cache(cache)` into a follow-up builder.
    pub cache: ObjectCache,
}

/// Chainable builder produced by `Package::deployer(network)`. Holds
/// references to `'static` bytecode tables emitted into
/// `bytecode/<network>.rs` plus the per-call config slots (sender,
/// client, signer, gas policy).
pub struct PackageDeployer {
    modules: &'static [&'static [u8]],
    dependencies: &'static [Address],
    digest: [u8; 32],
    sender: Option<Address>,
    submitter: Option<Box<dyn Submitter>>,
    signer: Option<Box<dyn DynSigner>>,
    gas_oracle: Option<Box<dyn GasOracle>>,
    dry_runner: Option<Box<dyn DryRunner>>,
    auto_gas: bool,
    gas_objects: Vec<ObjectReference>,
    gas_price: Option<u64>,
    gas_budget: Option<u64>,
    policy: Option<UpgradePolicy>,
}

impl PackageDeployer {
    /// Construct from the `bytecode/<network>.rs` tables. Generated
    /// `Package::deployer(network)` does the dispatch and hands `'static`
    /// slices in.
    pub fn new(
        modules: &'static [&'static [u8]],
        dependencies: &'static [Address],
        digest: [u8; 32],
    ) -> Self {
        Self {
            modules,
            dependencies,
            digest,
            sender: None,
            submitter: None,
            signer: None,
            gas_oracle: None,
            dry_runner: None,
            auto_gas: false,
            gas_objects: Vec::new(),
            gas_price: None,
            gas_budget: None,
            policy: None,
        }
    }

    /// Set the transaction sender. Required — every PTB has a sender
    /// and that's also the default `UpgradeCap` recipient.
    pub fn sender(mut self, addr: Address) -> Self {
        self.sender = Some(addr);
        self
    }

    /// Attach a [`Submitter`] used by [`Self::execute`] to send the signed tx.
    pub fn with_submitter(mut self, s: Box<dyn Submitter>) -> Self {
        self.submitter = Some(s);
        self
    }

    /// Store a signer so [`Self::execute`] can be called with no arguments.
    pub fn with_signer<S>(mut self, signer: S) -> Self
    where
        S: iota_sdk_crypto::IotaSigner + Send + Sync + 'static,
    {
        self.signer = Some(Box::new(signer));
        self
    }

    /// Attach a [`GasOracle`] used by [`Self::with_auto_gas`].
    pub fn with_gas_oracle(mut self, o: Box<dyn GasOracle>) -> Self {
        self.gas_oracle = Some(o);
        self
    }

    /// Attach a [`DryRunner`] (only consulted when [`Self::with_auto_gas`]
    /// asks for a dry-run-based budget estimate).
    pub fn with_dry_runner(mut self, d: Box<dyn DryRunner>) -> Self {
        self.dry_runner = Some(d);
        self
    }

    /// Convenience for any backend that's a [`Submitter`], [`GasOracle`],
    /// and [`DryRunner`] — e.g. the GraphQL client. Mirrors
    /// [`crate::PtbBuilder::with_client`].
    pub fn with_client<C>(mut self, c: C) -> Self
    where
        C: Submitter + GasOracle + DryRunner + Clone + 'static,
    {
        self.submitter = Some(Box::new(c.clone()));
        self.gas_oracle = Some(Box::new(c.clone()));
        self.dry_runner = Some(Box::new(c));
        self
    }

    /// Auto-fill any gas slot the user didn't set explicitly, using the
    /// attached [`GasOracle`].
    pub fn with_auto_gas(mut self) -> Self {
        self.auto_gas = true;
        self
    }

    /// Explicit gas-coin refs. Disables auto-gas selection.
    pub fn gas(mut self, refs: impl IntoIterator<Item = ObjectReference>) -> Self {
        self.gas_objects = refs.into_iter().collect();
        self
    }

    /// Explicit gas price (in nanos). Disables auto-gas pricing.
    pub fn gas_price(mut self, p: u64) -> Self {
        self.gas_price = Some(p);
        self
    }

    /// Explicit gas budget (in nanos). Disables auto-budget suggestion.
    pub fn gas_budget(mut self, b: u64) -> Self {
        self.gas_budget = Some(b);
        self
    }

    /// Override the default [`UpgradePolicy::Transfer`]-to-sender.
    pub fn with_policy(mut self, p: UpgradePolicy) -> Self {
        self.policy = Some(p);
        self
    }

    /// Build the precomputed [`MovePackageData`] BCS view from the
    /// embedded slices. Cheap — modules are copied into Vec<Vec<u8>>
    /// once, deps copy 32-byte addresses.
    fn package_data(&self) -> MovePackageData {
        let modules = self.modules.iter().map(|m| m.to_vec()).collect();
        let dependencies = self
            .dependencies
            .iter()
            .map(|a| ObjectId::from(*a))
            .collect();
        // Codegen captures the digest from the build; reuse it instead
        // of re-hashing here (avoids a feature requirement on
        // iota-sdk-types' `hash`).
        let digest = Digest::from_bytes(self.digest)
            .expect("32-byte digest is the only shape `from_bytes` accepts here");
        MovePackageData {
            modules,
            dependencies,
            digest,
        }
    }

    /// Sign + submit the publish transaction. Returns the new package
    /// id and the full effects on success.
    pub async fn execute(mut self) -> Result<DeployResult, ExecuteError> {
        let signer = self.signer.take().ok_or(ExecuteError::NoSigner)?;
        self.execute_with_dyn(&*signer).await
    }

    /// Like [`Self::execute`] but with an explicit signer.
    pub async fn execute_with<S>(self, signer: &S) -> Result<DeployResult, ExecuteError>
    where
        S: iota_sdk_crypto::IotaSigner + Send + Sync,
    {
        self.execute_with_dyn(signer).await
    }

    async fn execute_with_dyn(
        mut self,
        signer: &(dyn DynSigner + '_),
    ) -> Result<DeployResult, ExecuteError> {
        let sender = self.sender.ok_or(ExecuteError::NoSender)?;
        let policy = self.policy.unwrap_or(UpgradePolicy::Transfer(sender));

        let mut tx = TransactionBuilder::new(sender);
        let pkg_data = self.package_data();

        // `publish(...)` returns a builder in `Publish` state; assign
        // names the result so we can refer to the UpgradeCap by the
        // `assigned("upgrade_cap")` reference below.
        tx.publish(pkg_data).assign("upgrade_cap");

        match policy {
            UpgradePolicy::Transfer(recipient) => {
                tx.transfer_objects(recipient, [assigned("upgrade_cap")]);
            }
            UpgradePolicy::MakeImmutable => {
                tx.move_call(
                    ObjectId::from(IOTA_FRAMEWORK_ADDRESS),
                    "package",
                    "make_immutable",
                )
                .arguments([assigned("upgrade_cap")]);
            }
        }

        // Gas plumbing — explicit slots win; fill remainder via the
        // attached oracle if `auto_gas` was set.
        self.apply_gas(&mut tx).await?;

        let submitter = self.submitter.take().ok_or(ExecuteError::NoSubmitter)?;
        let built = tx
            .finish()
            .map_err(|e| ExecuteError::Finish(e.to_string()))?;
        let signature = signer
            .sign_dyn(&built)
            .await
            .map_err(ExecuteError::Sign)?;
        let effects = submitter.submit(&built, &[signature]).await?;

        let package_id = scan_package_id(&effects).ok_or_else(|| {
            ExecuteError::Finish(
                "publish succeeded but effects carry no PackageWrite — \
                 cannot recover the new package id"
                    .into(),
            )
        })?;

        Ok(DeployResult {
            package_id,
            effects,
            cache: ObjectCache::new(),
        })
    }

    async fn apply_gas(&mut self, tx: &mut TransactionBuilder) -> Result<(), ExecuteError> {
        if !self.gas_objects.is_empty() {
            tx.gas(self.gas_objects.iter().cloned());
        }
        if let Some(p) = self.gas_price {
            tx.gas_price(p);
        }
        if let Some(b) = self.gas_budget {
            tx.gas_budget(b);
        }

        if !self.auto_gas {
            return Ok(());
        }
        let sender = self.sender.ok_or(ExecuteError::NoSender)?;
        let oracle = self
            .gas_oracle
            .as_deref()
            .ok_or(ExecuteError::NoGasOracle)?;
        if self.gas_objects.is_empty() {
            let coin = oracle
                .list_gas_coins(sender)
                .await?
                .into_iter()
                .next()
                .ok_or(ExecuteError::Oracle(crate::OracleError::NoGasCoins(sender)))?;
            tx.gas([coin]);
        }
        if self.gas_price.is_none() {
            tx.gas_price(oracle.reference_gas_price().await?);
        }
        if self.gas_budget.is_none() {
            // Conservative fallback — publish gas use is hard to
            // estimate without a dry-run probe, and we don't carry one
            // here. Callers wanting precision set `.gas_budget(...)`
            // explicitly.
            tx.gas_budget(oracle.suggest_gas_budget().await?);
        }
        Ok(())
    }
}

/// Walk `effects.changed_objects` for the lone `PackageWrite` entry and
/// return its `object_id`. Publish creates exactly one new package, so
/// the first match is the answer.
fn scan_package_id(effects: &TransactionEffects) -> Option<Address> {
    for ch in &effects.as_v1().changed_objects {
        if matches!(ch.output_state, ObjectOut::PackageWrite { .. }) {
            return Some(Address::from(ch.object_id));
        }
    }
    None
}
