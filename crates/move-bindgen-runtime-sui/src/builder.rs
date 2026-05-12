//! `PtbBuilder` — wraps Sui's [`TransactionBuilder`], carries an
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

use sui_transaction_builder::ObjectInput;

use crate::{
    cache::CachedObject, Address, Argument, DryRunner, DynSigner, FetchError, FetchedObject,
    Fetcher, GasOracle, Identifier, InputKind, InspectResult, ObjectCache, ObjectId,
    ObjectReference, OracleError, PTBArgument, PackageAddrs, SignError, SubmitError, Submitter,
    Transaction, TransactionBuilder, TransactionEffects, TypeTag,
};

/// Inner builder. Wraps Sui's [`TransactionBuilder`] and exposes the
/// `apply_argument` method generated code calls via
/// `b.inner.apply_argument(self)`.
pub struct InnerBuilder {
    pub tx: TransactionBuilder,
    pub cache: ObjectCache,
}

impl InnerBuilder {
    /// Translate any `PTBArgument`-implementing value into a real
    /// builder argument. Object inputs that came in as bare
    /// `ObjectId`s (no version) are looked up in the cache; misses
    /// panic with a clear message — async lookups go through
    /// [`PtbBuilder::resolve_object`] / [`PtbBuilder::register_from_fetcher`]
    /// instead.
    pub fn apply_argument<P: PTBArgument>(&mut self, arg: P) -> Argument {
        match arg.input() {
            InputKind::Argument(a) => a,
            InputKind::Pure(bytes) => self.tx.pure_bytes(bytes),
            InputKind::ImmutableOrOwned(or) => self.tx.object(ObjectInput::owned(
                *or.object_id(),
                or.version(),
                *or.digest(),
            )),
            InputKind::Shared { object_id, mutable } => {
                let (object_id, initial) = match self.cache.lookup(&object_id) {
                    Some(CachedObject::Shared {
                        initial_shared_version,
                    }) => (object_id, *initial_shared_version),
                    Some(CachedObject::Owned(or)) => {
                        return self.tx.object(ObjectInput::owned(
                            *or.object_id(),
                            or.version(),
                            *or.digest(),
                        ));
                    }
                    Some(CachedObject::Immutable(or)) => {
                        return self.tx.object(ObjectInput::immutable(
                            *or.object_id(),
                            or.version(),
                            *or.digest(),
                        ));
                    }
                    None => panic!(
                        "object {object_id:?} is not in the cache; register it via \
                         `PtbBuilder::register_shared/register_owned/register_immutable` \
                         (or `register_from_fetcher`) before passing the bare ObjectId \
                         to a generated call",
                    ),
                };
                self.tx
                    .object(ObjectInput::shared(object_id, initial, mutable))
            }
            InputKind::SharedRef {
                object_ref,
                mutable,
            } => self.tx.object(ObjectInput::shared(
                *object_ref.object_id(),
                object_ref.version(),
                mutable,
            )),
            InputKind::Receiving(or) => self.tx.object(ObjectInput::receiving(
                *or.object_id(),
                or.version(),
                *or.digest(),
            )),
        }
    }
}

/// High-level PTB builder consumed by generated calls. Wraps
/// [`InnerBuilder`] (which in turn wraps Sui's [`TransactionBuilder`]).
pub struct PtbBuilder {
    pub inner: InnerBuilder,
    sender: Address,
    fetcher: Option<Box<dyn Fetcher>>,
    submitter: Option<Box<dyn Submitter>>,
    signer: Option<Box<dyn DynSigner>>,
    gas_oracle: Option<Box<dyn GasOracle>>,
    dry_runner: Option<Box<dyn DryRunner>>,
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
        let mut tx = TransactionBuilder::new();
        tx.set_sender(sender);
        Self {
            inner: InnerBuilder {
                tx,
                cache: ObjectCache::new(),
            },
            sender,
            fetcher: None,
            submitter: None,
            signer: None,
            gas_oracle: None,
            dry_runner: None,
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
        self.inner.cache = cache;
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
    /// Anything that impls `sui_crypto::SuiSigner` works
    /// (`Ed25519PrivateKey`, `Secp256k1PrivateKey`, …).
    pub fn with_signer<S>(mut self, signer: S) -> Self
    where
        S: sui_crypto::SuiSigner + Send + Sync + 'static,
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
    /// [`GasOracle`], and [`DryRunner`] — e.g. a Sui RPC client. Clones
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
        &self.inner.cache
    }

    /// Auto-fill any gas slot the user didn't set explicitly, using the
    /// attached [`GasOracle`]. Calling `gas(...)` / `gas_price(...)` /
    /// `gas_budget(...)` later overrides only that one slot — the others are
    /// still auto-filled.
    pub fn with_auto_gas(mut self) -> Self {
        self.auto_gas = true;
        self
    }

    /// Convenience wrapper around [`ObjectCache::register_owned`].
    pub fn register_owned(&mut self, id: ObjectId, reference: ObjectReference) -> &mut Self {
        self.inner.cache.register_owned(id, reference);
        self
    }

    /// Convenience wrapper around [`ObjectCache::register_immutable`].
    pub fn register_immutable(&mut self, id: ObjectId, reference: ObjectReference) -> &mut Self {
        self.inner.cache.register_immutable(id, reference);
        self
    }

    /// Convenience wrapper around [`ObjectCache::register_shared`].
    pub fn register_shared(&mut self, id: ObjectId, initial_shared_version: u64) -> &mut Self {
        self.inner.cache.register_shared(id, initial_shared_version);
        self
    }

    /// Fetch `id` via the attached [`Fetcher`] and register the result. Errors
    /// if no fetcher is attached or the fetch fails. The cached entry maps
    /// `FetchedObject::Owned` → owned (with full ref) and
    /// `FetchedObject::Shared` → shared (initial version only — mutability
    /// is decided per call site via `Shared<T>` / `SharedMut<T>`).
    pub async fn register_from_fetcher(&mut self, id: ObjectId) -> Result<(), FetchError> {
        let fetched = match self.fetcher.as_deref() {
            Some(f) => f.fetch(id).await?,
            None => return Err(FetchError::Backend("no fetcher configured".into())),
        };
        match fetched {
            FetchedObject::Owned(r) => self.inner.cache.register_owned(id, r),
            FetchedObject::Shared {
                initial_shared_version,
                ..
            } => self.inner.cache.register_shared(id, initial_shared_version),
        }
        Ok(())
    }

    /// Append a `MoveCall` command, returning the `Argument` handle
    /// for its result.
    pub fn move_call(
        &mut self,
        package: Address,
        module: &str,
        function: &str,
        type_arguments: Vec<TypeTag>,
        arguments: Vec<Argument>,
    ) -> Argument {
        let mut f = sui_transaction_builder::Function::new(
            package,
            Identifier::new(module).expect("static module name is a valid Move identifier"),
            Identifier::new(function).expect("static function name is a valid Move identifier"),
        );
        if !type_arguments.is_empty() {
            f = f.with_type_args(type_arguments);
        }
        self.inner.tx.move_call(f, arguments)
    }

    /// Append a `MoveCall` and split its multi-value result into `count`
    /// handles. Used by generated bindings for Move functions that
    /// return more than one value.
    pub fn move_call_n(
        &mut self,
        package: Address,
        module: &str,
        function: &str,
        type_arguments: Vec<TypeTag>,
        arguments: Vec<Argument>,
        count: u16,
    ) -> Vec<Argument> {
        self.move_call(package, module, function, type_arguments, arguments)
            .to_nested(count as usize)
    }

    /// Add a BCS-encoded value as a `Pure` input.
    pub fn pure<T: serde::Serialize>(&mut self, value: &T) -> Argument {
        self.inner.tx.pure(value)
    }

    /// Resolve an `ObjectId` to an `Argument` by consulting the cache.
    /// Cache miss with a [`Fetcher`] attached fetches+caches first;
    /// without one, panics. By-value entrypoint — defaults shared
    /// objects to `mutable=true` (the permissive lock).
    pub async fn resolve_object(&mut self, id: ObjectId) -> Argument {
        self.resolve_object_inner(id, /* shared_mutable */ true)
            .await
    }

    /// Like [`Self::resolve_object`] but lets the caller pick the
    /// shared-input mutability per call. Codegen routes
    /// `into_argument_ref` / `into_argument_mut` on `ObjectId`
    /// through here so each Move `&T` / `&mut T` parameter gets the
    /// right on-chain lock independent of how the object was cached.
    pub async fn resolve_object_shared(&mut self, id: ObjectId, mutable: bool) -> Argument {
        self.resolve_object_inner(id, mutable).await
    }

    async fn resolve_object_inner(&mut self, id: ObjectId, shared_mutable: bool) -> Argument {
        if self.inner.cache.lookup(&id).is_none() {
            // Best-effort: if a fetcher is configured, populate the
            // cache so the lookup below succeeds. If the fetch fails
            // we fall through to the panic — the user can call
            // `register_*` explicitly to avoid the network round-trip.
            if self.fetcher.is_some() {
                let _ = self.register_from_fetcher(id).await;
            }
        }
        match self.inner.cache.lookup(&id).cloned() {
            Some(CachedObject::Owned(or)) => self.inner.tx.object(ObjectInput::owned(
                *or.object_id(),
                or.version(),
                *or.digest(),
            )),
            Some(CachedObject::Immutable(or)) => self.inner.tx.object(ObjectInput::immutable(
                *or.object_id(),
                or.version(),
                *or.digest(),
            )),
            Some(CachedObject::Shared {
                initial_shared_version,
            }) => self.inner.tx.object(ObjectInput::shared(
                id,
                initial_shared_version,
                shared_mutable,
            )),
            None => panic!(
                "object {id:?} is not in the cache; register it via \
                 `PtbBuilder::register_shared/register_owned/register_immutable` \
                 before passing the bare ObjectId to a generated call",
            ),
        }
    }

    /// Set the gas-coin object refs. Disables auto-gas selection of the coin.
    pub fn gas(&mut self, refs: impl IntoIterator<Item = ObjectReference>) -> &mut Self {
        self.inner.tx.add_gas_objects(
            refs.into_iter()
                .map(|or| ObjectInput::owned(*or.object_id(), or.version(), *or.digest())),
        );
        self.gas_coin_set = true;
        self
    }

    /// Set the gas price (in MIST). Disables auto-gas pricing.
    pub fn gas_price(&mut self, price: u64) -> &mut Self {
        self.inner.tx.set_gas_price(price);
        self.gas_price_set = true;
        self
    }

    /// Set the gas budget (in MIST). Disables auto-budget suggestion.
    pub fn gas_budget(&mut self, budget: u64) -> &mut Self {
        self.inner.tx.set_gas_budget(budget);
        self.gas_budget_set = true;
        self
    }

    /// Convert this builder into a finalised [`Transaction`].
    pub fn finish(self) -> Result<Transaction, sui_transaction_builder::Error> {
        self.inner.tx.try_build()
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
        let tx = self.inner.tx.try_build().map_err(ExecuteError::finish)?;
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
        S: sui_crypto::SuiSigner + Send + Sync,
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
        let tx = self.inner.tx.try_build().map_err(ExecuteError::finish)?;
        let signature = signer.sign_dyn(&tx).await?;
        let effects = submitter.submit(&tx, &[signature]).await?;
        Ok((effects, self.inner.cache))
    }

    async fn fill_auto_gas(&mut self) -> Result<(), ExecuteError> {
        let oracle = self
            .gas_oracle
            .as_deref()
            .ok_or(ExecuteError::NoGasOracle)?;
        let sender = self.sender;

        if !self.gas_coin_set {
            // Skip coins already used as command args — same as iota's
            // auto-gas. Sui's cache holds owned/immutable refs in
            // separate variants; only `Owned` would clash with gas use.
            let coin = oracle
                .list_gas_coins(sender)
                .await?
                .into_iter()
                .find(|c| {
                    !matches!(
                        self.inner.cache.lookup(c.object_id()),
                        Some(CachedObject::Owned(_))
                    )
                })
                .ok_or(ExecuteError::Oracle(OracleError::NoGasCoins(sender)))?;
            self.inner.tx.add_gas_objects([ObjectInput::owned(
                *coin.object_id(),
                coin.version(),
                *coin.digest(),
            )]);
        }
        if !self.gas_price_set {
            self.inner
                .tx
                .set_gas_price(oracle.reference_gas_price().await?);
        }
        if !self.gas_budget_set {
            // Sui's `TransactionBuilder` isn't `Clone`, so the iota-style
            // dry-run-for-budget probe (build a draft, dry-run, measure
            // gas) doesn't translate. Stick to the oracle's conservative
            // suggestion — callers wanting a tighter budget can call
            // `gas_budget(...)` explicitly.
            self.inner
                .tx
                .set_gas_budget(oracle.suggest_gas_budget().await?);
        }
        Ok(())
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

/// Errors from [`PtbBuilder::execute`] / [`PtbBuilder::inspect`].
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
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
}

impl ExecuteError {
    fn finish(e: sui_transaction_builder::Error) -> Self {
        Self::Finish(e.to_string())
    }
}
