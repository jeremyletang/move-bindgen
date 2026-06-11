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

use std::collections::HashMap;

use iota_sdk_transaction_builder::{assigned, TransactionBuilder};
use iota_sdk_types::{
    execution_status::ExecutionStatus, Address, MovePackageData, ObjectId, ObjectOut,
    ObjectReference, TransactionEffects,
};
use move_binary_format::CompiledModule;
use move_core_types::account_address::AccountAddress;

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

/// Type alias for an optional log sink the deployer routes its
/// gas-estimation / dep-patching trace through. Silent by default —
/// users attach a callback via [`PackageDeployer::with_log`] when they
/// want the play-by-play.
pub type LogFn = Box<dyn Fn(&str) + Send + Sync>;

/// Chainable builder produced by `Package::deployer(network)`. Holds
/// references to `'static` bytecode tables emitted into
/// `bytecode/<network>.rs` plus the per-call config slots (sender,
/// client, signer, gas policy).
pub struct PackageDeployer {
    modules: &'static [&'static [u8]],
    dependencies: &'static [Address],
    digest: [u8; 32],
    /// `(synthetic_address, move_name)` pairs for workspace-internal
    /// deps. The user supplies real addresses via [`Self::resolve_dep`],
    /// looked up by name; `execute` then patches every module's
    /// `address_identifiers` pool and the dependency list before
    /// publish.
    dep_labels: &'static [(Address, &'static str)],
    dep_overrides: HashMap<String, Address>,
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
    log: Option<LogFn>,
}

impl PackageDeployer {
    /// Construct from the `bytecode/<network>.rs` tables. Generated
    /// `Package::deployer(network)` does the dispatch and hands `'static`
    /// slices in.
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
            dep_overrides: HashMap::new(),
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
            log: None,
        }
    }

    /// Attach a log sink. Receives one line per gas-estimation /
    /// dep-patching event during [`Self::execute`]. Silent by default;
    /// pass `|s| eprintln!("{s}")` to see the trace.
    pub fn with_log<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) + Send + Sync + 'static,
    {
        self.log = Some(Box::new(f));
        self
    }

    /// Internal: emit a single line through the configured log sink,
    /// or drop it on the floor if no sink was attached.
    fn log_line(&self, msg: impl AsRef<str>) {
        if let Some(f) = &self.log {
            f(msg.as_ref());
        }
    }

    /// Read-only view of `(synthetic_address, move_name)` pairs for
    /// every workspace-internal dep this package's bytecode references.
    /// Useful for auto-resolving — iterate, look each name up in your
    /// deployed-package map, and call [`Self::resolve_dep`] per match.
    pub fn dep_labels(&self) -> &'static [(Address, &'static str)] {
        self.dep_labels
    }

    /// Supply the real on-chain address for a workspace-internal
    /// dependency. `name` matches one of the entries in the
    /// `DEP_LABELS` static emitted into `bytecode/<network>.rs` — most
    /// commonly the lowercased / snake-cased Move package name (e.g.
    /// `"fixed18"`, `"ring_buffer"`).
    ///
    /// At [`Self::execute`] time every module's `address_identifiers`
    /// pool has the synthetic address swapped for the real one, the
    /// dependency list is updated, and the digest recomputed before
    /// publish. Unresolved synthetic deps will cause a
    /// `PublishUpgradeMissingDependency` on the chain side — surface
    /// them by inspecting `bytecode::<network>::DEP_LABELS`.
    pub fn resolve_dep(mut self, name: impl Into<String>, real_address: Address) -> Self {
        self.dep_overrides.insert(name.into(), real_address);
        self
    }

    /// Bulk variant of [`Self::resolve_dep`]: for every entry in this
    /// deployer's `DEP_LABELS`, look the name up in `resolved` and
    /// apply [`Self::resolve_dep`] if found. Entries not in the map
    /// are left unresolved (and will fail publish with a clear error).
    ///
    /// Standard usage in a multi-package deploy loop:
    ///
    /// ```ignore
    /// let mut on_chain: HashMap<&'static str, Address> = HashMap::new();
    /// // After each successful publish:
    /// on_chain.insert(my_crate::Package::ADDRESS_NAME, result.package_id);
    /// // Subsequent deployers pick up everything already deployed:
    /// next_crate::Package::deployer(net)
    ///     .resolve_from(&on_chain)
    ///     .sender(sender)
    ///     // ...
    /// ```
    pub fn resolve_from(mut self, resolved: &HashMap<&'static str, Address>) -> Self {
        for (_synth, name) in self.dep_labels {
            if let Some(addr) = resolved.get(name) {
                self.dep_overrides.insert(name.to_string(), *addr);
            }
        }
        self
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

    /// Build the [`MovePackageData`] payload, applying any dep
    /// overrides supplied via [`Self::resolve_dep`]. Pure path (no
    /// overrides): copies the embedded slices verbatim and uses the
    /// codegen-time digest. Patched path: deserializes each module,
    /// rewrites the `address_identifiers` pool, re-serializes, then
    /// lets [`MovePackageData::new`] recompute the digest.
    fn package_data(&self) -> Result<MovePackageData, ExecuteError> {
        // Build synth→real for the active overrides only. Synthetics
        // the user didn't resolve are left as-is — the chain will
        // reject the publish with a clear `MissingDependency`.
        let mut subs: HashMap<AccountAddress, AccountAddress> = HashMap::new();
        let mut requested_but_unknown: Vec<&str> = Vec::new();
        for (name, real) in &self.dep_overrides {
            let synth = self
                .dep_labels
                .iter()
                .find_map(|(s, n)| (*n == name.as_str()).then_some(*s));
            match synth {
                Some(s) => {
                    subs.insert(addr_to_core(s), addr_to_core(*real));
                }
                None => requested_but_unknown.push(name.as_str()),
            }
        }
        if !requested_but_unknown.is_empty() {
            self.log_line(format!(
                "[deploy/patch] warning: resolve_dep({:?}) does not match any \
                 entry in DEP_LABELS — available names: {:?}",
                requested_but_unknown,
                self.dep_labels.iter().map(|(_, n)| *n).collect::<Vec<_>>()
            ));
        }

        if subs.is_empty() {
            self.log_line(format!(
                "[deploy/patch] no overrides active — using codegen-time bytes verbatim ({} dep_labels available)",
                self.dep_labels.len(),
            ));
            // Fast path: no patching needed, reuse precomputed digest.
            let modules = self.modules.iter().map(|m| m.to_vec()).collect();
            let dependencies = self
                .dependencies
                .iter()
                .map(|a| ObjectId::from(*a))
                .collect();
            let digest = iota_sdk_types::Digest::from_bytes(self.digest)
                .expect("32-byte digest is the only shape `from_bytes` accepts here");
            return Ok(MovePackageData {
                modules,
                dependencies,
                digest,
            });
        }

        self.log_line(format!(
            "[deploy/patch] applying {} dep substitution(s): {:?}",
            subs.len(),
            self.dep_overrides
                .iter()
                .filter(|(n, _)| self.dep_labels.iter().any(|(_, dn)| dn == n))
                .map(|(n, a)| format!("{n} → {a}"))
                .collect::<Vec<_>>()
        ));

        // Patched path. Walk each module, rewrite its address_identifiers
        // pool, then re-serialize. The chain re-derives module ids from
        // the same pool, so this is the canonical substitution point.
        let mut total_slots_patched = 0;
        let mut unresolved_addrs = std::collections::BTreeSet::new();
        let mut modules = Vec::with_capacity(self.modules.len());
        for (i, bytes) in self.modules.iter().enumerate() {
            let mut m = CompiledModule::deserialize_with_defaults(bytes).map_err(|e| {
                ExecuteError::Finish(format!("deserializing module #{i} for patching: {e:?}"))
            })?;
            for slot in m.address_identifiers.iter_mut() {
                if let Some(real) = subs.get(&*slot) {
                    *slot = *real;
                    total_slots_patched += 1;
                } else if *slot != AccountAddress::ZERO {
                    // Non-zero, non-substituted — a real on-chain
                    // address (e.g., framework 0x1/0x2) OR an
                    // unresolved synthetic that'll trip the chain.
                    unresolved_addrs.insert(*slot);
                }
            }
            let mut out = Vec::with_capacity(bytes.len());
            m.serialize_with_version(m.version, &mut out).map_err(|e| {
                ExecuteError::Finish(format!("re-serializing patched module #{i}: {e:?}"))
            })?;
            modules.push(out);
        }
        self.log_line(format!(
            "[deploy/patch] patched {total_slots_patched} address-identifier slot(s) across {} module(s); {} other non-zero address(es) left as-is: {:?}",
            modules.len(),
            unresolved_addrs.len(),
            unresolved_addrs
                .iter()
                .map(|a| format!("{a}"))
                .collect::<Vec<_>>(),
        ));

        let dependencies: Vec<ObjectId> = self
            .dependencies
            .iter()
            .map(|a| {
                let core = addr_to_core(*a);
                let mapped = subs.get(&core).copied().unwrap_or(core);
                ObjectId::from(core_to_addr(mapped))
            })
            .collect();

        // `MovePackageData::new` recomputes the digest over the new
        // (modules, deps) — required for the chain to accept the
        // patched package.
        Ok(MovePackageData::new(modules, dependencies))
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

        // Some packages compile to zero root modules (e.g. test-only
        // helper packages whose sources are all gated behind
        // `#[test_only]`). The chain rejects empty `Publish` commands
        // with a generic "empty arguments" error; surface a clearer
        // one instead.
        if self.modules.is_empty() {
            return Err(ExecuteError::Finish(
                "package has no publishable modules — nothing to deploy. \
                 (If the package's sources are all #[test_only], it shouldn't \
                 appear in `[publish] networks`.)"
                    .into(),
            ));
        }

        let mut tx = TransactionBuilder::new(sender);
        let pkg_data = self.package_data()?;

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
        let signature = signer.sign_dyn(&built).await.map_err(ExecuteError::Sign)?;
        let effects = submitter.submit(&built, &[signature]).await?;

        // The submit RPC returns Ok for both successful and *aborted*
        // transactions — the chain accepts the tx then reports the
        // outcome via the effects' status. Translate failures into a
        // typed error before scanning for PackageWrite (which won't be
        // there on abort).
        let gas = &effects.as_v1().gas_used;
        self.log_line(format!(
            "[deploy/gas] actual on-chain: gas_used = {} nanos, net = {} nanos (after storage rebate)",
            gas.gas_used(),
            gas.net_gas_usage(),
        ));

        if let ExecutionStatus::Failure { error, command } = effects.status() {
            return Err(ExecuteError::OnChain {
                message: format!("{error:?}"),
                command: *command,
            });
        }

        let package_id = scan_package_id(&effects).ok_or_else(|| {
            ExecuteError::Finish(
                "publish completed but effects carry no PackageWrite — \
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
            // Dry-run-based probe — clone the in-progress builder, set
            // a temporary sim budget so the clone can `.finish()`, ask
            // the oracle for actual gas usage, then set the real budget
            // to `used + 20%`. The probe adapts to low-balance wallets
            // by halving its sim budget on gas-related rejections (see
            // `dry_run_budget`). Falls back to the oracle's
            // conservative suggest if the dry-run still fails (e.g. on
            // networks that don't expose dry-run). For full control,
            // set `.gas_budget(N)` explicitly — that skips all of this.
            let log = |s: &str| self.log_line(s);
            let budget = match dry_run_budget(tx, oracle, &log).await {
                Ok((estimate, budget)) => {
                    self.log_line(format!(
                        "[deploy/gas] dry-run estimated {estimate} nanos → budget {budget} (+20% margin)",
                    ));
                    budget
                }
                Err(e) => {
                    let fallback = oracle.suggest_gas_budget().await?;
                    self.log_line(format!(
                        "[deploy/gas] dry-run estimate failed ({e}); falling back to oracle suggest = {fallback} nanos. \
                         If the publish needs more than this, set an explicit `.gas_budget(N)`.",
                    ));
                    fallback
                }
            };
            tx.gas_budget(budget);
        }
        Ok(())
    }
}

/// Starting sim budget for the dry-run probe: 1 IOTA in nanos. The
/// draft tx needs *a* budget for `.finish()` to succeed; the value
/// itself only has to pass the chain's input checker (selected gas
/// coin's balance ≥ budget).
const SIM_BUDGET_START: u64 = 1_000_000_000;

/// Floor for the adaptive halving — below ~8M nanos no publish fits
/// anyway, so keep retrying past this point is pointless.
const SIM_BUDGET_FLOOR: u64 = 8_000_000;

/// Clone the in-progress builder, set a sim budget so the clone can
/// `.finish()`, dry-run, return `(gas_used, gas_used + 20%)`.
///
/// The chain's input checker rejects the dry-run when the selected
/// gas coin holds less than the sim budget — even with `skip_checks`.
/// To stay usable on wallets below 1 IOTA (common after a few
/// publishes in a workspace deploy), gas-related rejections trigger a
/// retry with the budget halved, down to [`SIM_BUDGET_FLOOR`].
///
/// The returned budget is deliberately **not** capped at the sim
/// value: if real usage exceeds it, an under-capped budget would send
/// the publish on-chain doomed to `InsufficientGas` (burning real
/// gas), while an honest `used + 20%` either succeeds or is rejected
/// by the input checker pre-execution at zero cost.
async fn dry_run_budget(
    tx: &TransactionBuilder,
    oracle: &(dyn GasOracle + '_),
    log: &dyn Fn(&str),
) -> Result<(u64, u64), crate::OracleError> {
    let mut sim = SIM_BUDGET_START;
    loop {
        let mut draft = tx.clone();
        draft.gas_budget(sim);
        let built = draft
            .finish()
            .map_err(|e| crate::OracleError::Backend(format!("draft finish: {e}")))?;
        match oracle.dry_run_estimate(&built).await {
            Ok(gas_used) => {
                let budget = gas_used.saturating_add(gas_used / 5);
                return Ok((gas_used, budget));
            }
            // Heuristic: the input checker's rejection reads
            // "Transaction input checker should check that there is
            // enough gas". Other gas-ish failures retry too — a few
            // wasted round-trips at worst; anything else (network
            // errors, real aborts) bails straight to the caller's
            // fallback.
            Err(e) if sim / 2 >= SIM_BUDGET_FLOOR && is_gas_related(&e) => {
                log(&format!(
                    "[deploy/gas] dry-run at sim budget {sim} rejected ({e}); retrying at {}",
                    sim / 2,
                ));
                sim /= 2;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Does this oracle error look like a budget/balance rejection (as
/// opposed to a network failure or a genuine execution abort)?
fn is_gas_related(e: &crate::OracleError) -> bool {
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("gas") || msg.contains("balance")
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

/// `iota_sdk_types::Address` (32-byte newtype) and
/// `move_core_types::AccountAddress` (same 32 bytes, different type
/// identity) are wire-compatible. These helpers bridge them so the
/// bytecode-patching code can speak `AccountAddress` (what
/// `move-binary-format` exposes) without leaking that into the public
/// API.
fn addr_to_core(a: Address) -> AccountAddress {
    AccountAddress::new(a.into_bytes())
}

fn core_to_addr(a: AccountAddress) -> Address {
    Address::new(a.into_bytes())
}
