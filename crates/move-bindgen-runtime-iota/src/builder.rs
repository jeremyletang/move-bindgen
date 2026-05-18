//! `PtbBuilder` — wraps the SDK's [`TransactionBuilder`], carries an
//! [`ObjectCache`] across calls, can resolve unknown ids via an
//! attached [`Fetcher`], and (with a signer + submitter) finishes
//! and submits the transaction in one go.
//!
//! Generated call-builders consume bare [`ObjectId`]s and consult
//! [`PtbBuilder`] to pick the right [`Input`] variant; the rest of
//! the typed surface (`PureX`, `ArgumentObject<T>`) lives in
//! [`crate::arguments`].

use std::any::TypeId;
use std::collections::HashMap;

use crate::{
    cache::SharedObjectInfo, Address, Argument, Command, DryRunner, DynSigner, FetchError,
    FetchedObject, Fetcher, GasOracle, Identifier, Input, InspectResult, MoveCall, ObjectCache,
    ObjectId, ObjectReference, OracleError, PackageAddrs, SharedObjectReference, SignError,
    SubmitError, Submitter, Transaction, TransactionBuilder, TransactionEffects, TypeTag, Version,
};

/// Wrapper around the SDK's [`TransactionBuilder`] that remembers per-id
/// ownership info, optionally fetches unknown ids on demand, and can sign +
/// submit the finished transaction. Generated call builders accept bare
/// [`ObjectId`]s and consult this builder to pick the right [`Input`] variant.
pub struct PtbBuilder {
    /// Underlying SDK builder. `pub` because advanced flows reach in for
    /// SDK methods we don't surface (e.g. `transfer_objects`).
    pub inner: TransactionBuilder,
    sender: Address,
    fetcher: Option<Box<dyn Fetcher>>,
    submitter: Option<Box<dyn Submitter>>,
    signer: Option<Box<dyn DynSigner>>,
    gas_oracle: Option<Box<dyn GasOracle>>,
    dry_runner: Option<Box<dyn DryRunner>>,
    cache: ObjectCache,
    auto_gas: bool,
    /// Runtime package-address registry. Generated `move_call*` /
    /// `MoveType::type_tag` callsites resolve their package's on-chain
    /// address through this — callers register addresses once per
    /// PTB with `with_package`.
    packages: HashMap<TypeId, Address>,
    gas_coin_set: bool,
    gas_price_set: bool,
    gas_budget_set: bool,
}

impl PtbBuilder {
    pub fn new(sender: Address) -> Self {
        Self {
            inner: TransactionBuilder::new(sender),
            sender,
            fetcher: None,
            submitter: None,
            signer: None,
            gas_oracle: None,
            dry_runner: None,
            cache: ObjectCache::new(),
            auto_gas: false,
            packages: HashMap::new(),
            gas_coin_set: false,
            gas_price_set: false,
            gas_budget_set: false,
        }
    }

    /// Register a Move package's on-chain address against its generated
    /// `Package` marker. Generated `move_call*` and `MoveType::type_tag`
    /// callsites read this map. Call once per package per PTB.
    ///
    /// Value-based receiver so it chains with the other builder
    /// methods (`with_client`, `with_signer`, `with_auto_gas`, …):
    ///
    /// ```ignore
    /// let mut ptb = PtbBuilder::new(sender)
    ///     .with_client(client)
    ///     .with_signer(signer)
    ///     .with_auto_gas()
    ///     .with_package::<MyPkg>(addr);
    /// ```
    pub fn with_package<P: 'static>(mut self, addr: Address) -> Self {
        self.packages.insert(TypeId::of::<P>(), addr);
        self
    }

    /// Seed the builder with a previously-collected [`ObjectCache`] (e.g. one
    /// returned by an earlier [`Self::execute`]).
    pub fn with_cache(mut self, cache: ObjectCache) -> Self {
        self.cache = cache;
        self
    }

    /// Attach a [`Fetcher`] for on-demand lookup of unknown object ids.
    pub fn with_fetcher(mut self, f: Box<dyn Fetcher>) -> Self {
        self.fetcher = Some(f);
        self
    }

    /// Attach a [`Submitter`] used by [`Self::execute`] to send the signed tx.
    pub fn with_submitter(mut self, s: Box<dyn Submitter>) -> Self {
        self.submitter = Some(s);
        self
    }

    /// Store a signer so [`Self::execute`] can be called with no arguments.
    /// Anything that impls `iota_sdk_crypto::IotaSigner` works
    /// (`Ed25519PrivateKey`, `Secp256k1PrivateKey`, …).
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

    /// Attach a [`DryRunner`] used by [`Self::inspect`].
    pub fn with_dry_runner(mut self, d: Box<dyn DryRunner>) -> Self {
        self.dry_runner = Some(d);
        self
    }

    /// Convenience for any backend that's a [`Fetcher`], [`Submitter`],
    /// [`GasOracle`], and [`DryRunner`] — e.g. the GraphQL client. Clones
    /// `c` to fill all four slots, so most callers don't need an explicit
    /// `Arc`.
    pub fn with_client<C>(mut self, c: C) -> Self
    where
        C: Fetcher + Submitter + GasOracle + DryRunner + Clone + 'static,
    {
        self.fetcher = Some(Box::new(c.clone()));
        self.submitter = Some(Box::new(c.clone()));
        self.gas_oracle = Some(Box::new(c.clone()));
        self.dry_runner = Some(Box::new(c));
        self
    }

    /// Read access to the cache (e.g. to inspect what's been registered).
    pub fn cache(&self) -> &ObjectCache {
        &self.cache
    }

    /// Auto-fill any gas slot the user didn't set explicitly, using the
    /// attached [`GasOracle`]. Calling `gas(...)` / `gas_price(...)` /
    /// `gas_budget(...)` later overrides only that one slot — the others are
    /// still auto-filled.
    pub fn with_auto_gas(mut self) -> Self {
        self.auto_gas = true;
        self
    }

    /// Cache `id` as an owned (or immutable-by-id) object.
    pub fn register_owned(&mut self, id: ObjectId, r: ObjectReference) {
        self.cache.owned.insert(id, r);
    }

    /// Cache `id` as a shared object with the given initial shared
    /// version. Mutability is decided per call site (the codegen
    /// path wraps as `Shared` / `SharedMut`; manual users go through
    /// [`Self::resolve_object_shared`]).
    pub fn register_shared(&mut self, id: ObjectId, initial_shared_version: u64) {
        self.cache.shared.insert(
            id,
            SharedObjectInfo {
                initial_shared_version,
            },
        );
    }

    /// Fetch `id` via the attached [`Fetcher`] and register the result. Errors
    /// if no fetcher is attached or the fetch fails.
    pub async fn register_from_fetcher(&mut self, id: ObjectId) -> Result<(), FetchError> {
        let fetched = match self.fetcher.as_deref() {
            Some(f) => f.fetch(id).await?,
            None => return Err(FetchError::Backend("no fetcher configured".into())),
        };
        match fetched {
            FetchedObject::Owned(r) => self.register_owned(id, r),
            FetchedObject::Shared {
                initial_shared_version,
                ..
            } => self.register_shared(id, initial_shared_version),
        }
        Ok(())
    }

    /// Cache hit → corresponding [`Input`] variant.
    /// Cache miss with [`Fetcher`] attached → fetch, cache, return.
    /// Cache miss without fetcher → bare-id input (SDK errors at finish time
    /// if it never gets resolved).
    ///
    /// By-value entrypoint — defaults shared objects to `mutable=true`
    /// (the permissive lock). Codegen-driven `&T` / `&mut T` go
    /// through [`Self::resolve_object_shared`] for per-call mutability.
    pub async fn resolve_object(&mut self, id: ObjectId) -> Argument {
        self.resolve_object_inner(id, true).await
    }

    /// Like [`Self::resolve_object`] but pins the shared-input
    /// mutability for this call only. Codegen routes
    /// `into_argument_ref` / `into_argument_mut` on `ObjectId`
    /// through here so each Move `&T` / `&mut T` parameter gets the
    /// right on-chain lock independent of how the object was cached.
    pub async fn resolve_object_shared(&mut self, id: ObjectId, mutable: bool) -> Argument {
        self.resolve_object_inner(id, mutable).await
    }

    async fn resolve_object_inner(&mut self, id: ObjectId, shared_mutable: bool) -> Argument {
        if let Some(r) = self.cache.owned.get(&id).cloned() {
            return self.inner.input(Input::ImmutableOrOwned(r));
        }
        if let Some(info) = self.cache.shared.get(&id).copied() {
            return self.inner.input(Input::Shared(SharedObjectReference {
                object_id: id,
                initial_shared_version: Version::from_u64(info.initial_shared_version),
                mutable: shared_mutable,
            }));
        }

        let fetched = match self.fetcher.as_deref() {
            Some(f) => Some(f.fetch(id).await),
            None => None,
        };
        match fetched {
            Some(Ok(FetchedObject::Owned(r))) => {
                self.cache.owned.insert(id, r);
                self.inner.input(Input::ImmutableOrOwned(r))
            }
            Some(Ok(FetchedObject::Shared {
                initial_shared_version,
                ..
            })) => {
                self.cache.shared.insert(
                    id,
                    SharedObjectInfo {
                        initial_shared_version,
                    },
                );
                self.inner.input(Input::Shared(SharedObjectReference {
                    object_id: id,
                    initial_shared_version: Version::from_u64(initial_shared_version),
                    mutable: shared_mutable,
                }))
            }
            _ => self.inner.apply_argument(id),
        }
    }

    /// Append a `MoveCall` command. Returns the `Argument::Result` handle.
    pub fn move_call(
        &mut self,
        package: Address,
        module: &str,
        function: &str,
        type_arguments: Vec<TypeTag>,
        arguments: Vec<Argument>,
    ) -> Argument {
        let cmd = Command::MoveCall(MoveCall {
            package: ObjectId::from(package),
            module: Identifier::new(module).expect("static module name is a valid Move identifier"),
            function: Identifier::new(function).expect("static fn name is a valid Move identifier"),
            type_arguments,
            arguments,
        });
        self.inner.command(cmd)
    }

    /// Append a `MoveCall` and split its multi-value result into `count`
    /// `Argument::NestedResult(idx, i)` handles. Used by generated
    /// bindings for Move functions that return more than one value.
    pub fn move_call_n(
        &mut self,
        package: Address,
        module: &str,
        function: &str,
        type_arguments: Vec<TypeTag>,
        arguments: Vec<Argument>,
        count: u16,
    ) -> Vec<Argument> {
        match self.move_call(package, module, function, type_arguments, arguments) {
            Argument::Result(idx) => (0..count).map(|i| Argument::NestedResult(idx, i)).collect(),
            _ => unreachable!("move_call always returns Argument::Result"),
        }
    }

    /// Add a BCS-encoded value as a `Pure` input.
    pub fn pure<T: serde::Serialize>(&mut self, value: T) -> Argument {
        self.inner.pure(value)
    }

    /// Set the gas-coin object refs. Disables auto-gas selection of the coin.
    pub fn gas(&mut self, refs: impl IntoIterator<Item = ObjectReference>) -> &mut Self {
        self.inner.gas(refs);
        self.gas_coin_set = true;
        self
    }

    /// Set the gas price (in nanos). Disables auto-gas pricing.
    pub fn gas_price(&mut self, price: u64) -> &mut Self {
        self.inner.gas_price(price);
        self.gas_price_set = true;
        self
    }

    /// Set the gas budget (in nanos). Disables auto-budget suggestion.
    pub fn gas_budget(&mut self, budget: u64) -> &mut Self {
        self.inner.gas_budget(budget);
        self.gas_budget_set = true;
        self
    }

    /// Convert this builder into a finalised [`Transaction`]. Forwards to the
    /// SDK's `TransactionBuilder::finish` for the no-client mode.
    pub fn finish(self) -> Result<Transaction, iota_sdk_transaction_builder::error::Error> {
        self.inner.finish()
    }

    /// Dry-run without signing or submitting. Use the same builder you'd use
    /// for [`Self::execute`] — the only difference is the terminal action.
    /// Returns an [`InspectResult`] you can call `.decode::<T>(arg)` on to
    /// pull typed values out of the dry-run.
    pub async fn inspect(mut self) -> Result<InspectResult, ExecuteError> {
        if self.auto_gas {
            self.fill_auto_gas().await?;
        }
        let runner = self.dry_runner.take().ok_or(ExecuteError::NoDryRunner)?;
        let tx = self.inner.finish().map_err(ExecuteError::finish)?;
        runner
            .dry_run(&tx)
            .await
            .map_err(|e| ExecuteError::DryRun(e.to_string()))
    }

    /// Finalise, sign with the stored signer, and submit via the attached
    /// [`Submitter`]. Returns the resulting effects plus the builder's
    /// [`ObjectCache`] (so callers can carry it into the next builder via
    /// [`Self::with_cache`]). Errors if no signer was attached via
    /// [`Self::with_signer`] — use [`Self::execute_with`] for an explicit
    /// signer override.
    pub async fn execute(mut self) -> Result<(TransactionEffects, ObjectCache), ExecuteError> {
        let signer = self.signer.take().ok_or(ExecuteError::NoSigner)?;
        self.execute_with_dyn(&*signer).await
    }

    /// Like [`Self::execute`] but takes an explicit signer. Overrides any
    /// signer set via [`Self::with_signer`].
    pub async fn execute_with<S>(
        self,
        signer: &S,
    ) -> Result<(TransactionEffects, ObjectCache), ExecuteError>
    where
        S: iota_sdk_crypto::IotaSigner + Send + Sync,
    {
        self.execute_with_dyn(signer).await
    }

    async fn execute_with_dyn(
        mut self,
        signer: &(dyn DynSigner + '_),
    ) -> Result<(TransactionEffects, ObjectCache), ExecuteError> {
        if self.auto_gas {
            self.fill_auto_gas().await?;
        }
        let submitter = self.submitter.take().ok_or(ExecuteError::NoSubmitter)?;
        let tx = self.inner.finish().map_err(ExecuteError::finish)?;
        let signature = signer.sign_dyn(&tx).await?;
        let effects = submitter.submit(&tx, &[signature]).await?;
        Ok((effects, self.cache))
    }

    async fn fill_auto_gas(&mut self) -> Result<(), ExecuteError> {
        let oracle = self
            .gas_oracle
            .as_deref()
            .ok_or(ExecuteError::NoGasOracle)?;
        let sender = self.sender;

        if !self.gas_coin_set {
            let coin = oracle
                .list_gas_coins(sender)
                .await?
                .into_iter()
                .find(|c| !self.cache.owned.contains_key(&c.object_id))
                .ok_or(ExecuteError::Oracle(OracleError::NoGasCoins(sender)))?;
            self.inner.gas([coin]);
        }
        if !self.gas_price_set {
            self.inner.gas_price(oracle.reference_gas_price().await?);
        }
        if !self.gas_budget_set {
            let budget = match self.dry_run_budget(oracle).await {
                Ok(estimated) => estimated,
                Err(_) => oracle.suggest_gas_budget().await?,
            };
            self.inner.gas_budget(budget);
        }
        Ok(())
    }

    /// Clone `inner`, set a temporary high budget on the clone so it can
    /// finish into a complete `Transaction`, dry-run it, and return
    /// `gas_used + ~20% margin`.
    async fn dry_run_budget(&self, oracle: &(dyn GasOracle + '_)) -> Result<u64, OracleError> {
        // 1 IOTA in nanos — generous for the simulation. With `skip_checks`
        // the network usually won't reject this regardless.
        const SIM_BUDGET: u64 = 1_000_000_000;
        let mut draft = self.inner.clone();
        draft.gas_budget(SIM_BUDGET);
        let tx = draft
            .finish()
            .map_err(|e| OracleError::Backend(format!("draft finish: {e}")))?;
        let gas_used = oracle.dry_run_estimate(&tx).await?;
        // 20% safety margin, capped at the simulation budget so we never
        // over-suggest beyond what the user is willing to spend on a sim.
        Ok(gas_used.saturating_add(gas_used / 5).min(SIM_BUDGET))
    }
}

impl PackageAddrs for PtbBuilder {
    fn package_id<P: 'static>(&self) -> Address {
        *self.packages.get(&TypeId::of::<P>()).unwrap_or_else(|| {
            panic!(
                "PtbBuilder: no address registered for package `{}` — \
                 call `b.with_package::<{}>(addr)` before building the PTB",
                std::any::type_name::<P>(),
                std::any::type_name::<P>(),
            )
        })
    }
}

/// Errors from [`PtbBuilder::execute`] / [`PtbBuilder::inspect`] /
/// [`crate::PackageDeployer::execute`].
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error("no sender configured — call `.sender(addr)` before `.execute()`")]
    NoSender,
    #[error("no submitter configured — call `.with_submitter(...)` or `.with_client(...)`")]
    NoSubmitter,
    #[error("no signer configured — call `.with_signer(...)` or use `execute_with(&signer)`")]
    NoSigner,
    #[error(
        "auto-gas requires a gas oracle — call `.with_gas_oracle(...)` or `.with_client(...)`"
    )]
    NoGasOracle,
    #[error("inspect requires a dry-runner — call `.with_dry_runner(...)` or `.with_client(...)`")]
    NoDryRunner,
    #[error("dry-run: {0}")]
    DryRun(String),
    #[error("oracle: {0}")]
    Oracle(#[from] OracleError),
    #[error("finishing the transaction: {0}")]
    Finish(String),
    #[error("signing the transaction: {0}")]
    Sign(#[from] SignError),
    #[error("submitting the transaction: {0}")]
    Submit(#[from] SubmitError),
    #[error("transaction failed on-chain: {message} (command {command:?})")]
    OnChain {
        message: String,
        command: Option<u64>,
    },
}

impl ExecuteError {
    fn finish(e: iota_sdk_transaction_builder::error::Error) -> Self {
        Self::Finish(e.to_string())
    }
}
