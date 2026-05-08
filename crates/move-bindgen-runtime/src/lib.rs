//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Provides [`PtbBuilder`] (the high-level builder), the [`Fetcher`] /
//! [`Submitter`] / [`GasOracle`] / [`DynSigner`] backend traits, the
//! per-Move-primitive marker traits (`PureBool`, `PureU64`, …) used as bounds
//! on generated call builders, and re-exports the SDK types generated code
//! needs.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

pub use iota_sdk_transaction_builder::{
    types::{MoveArg, MoveType},
    // `Argument`, `Command`, `MoveCall` here are the "unresolved" variants the
    // builder composes during PTB construction. They get resolved to the
    // `iota_sdk_types::*` counterparts when `TransactionBuilder::finish()` is
    // called.
    unresolved::{Argument, Command, MoveCall},
    PTBArgument,
    PureBytes,
    Receiving,
    Shared,
    SharedMut,
    TransactionBuilder,
    TransactionSigner,
};
pub use iota_sdk_types::{
    Address, Identifier, Input, ObjectId, ObjectReference, SharedObjectReference, StructTag,
    Transaction, TransactionEffects, TypeTag, UserSignature, Version,
};

// -----------------------------------------------------------------------------
// Framework types
// -----------------------------------------------------------------------------

/// `iota::object::ID` — a 32-byte address wrapped in a struct.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ID {
    pub bytes: Address,
}

/// `iota::object::UID` — owns a single `ID`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UID {
    pub id: ID,
}

const IOTA_FRAMEWORK_ADDRESS: Address = {
    let mut bytes = [0u8; 32];
    bytes[31] = 0x02;
    Address::new(bytes)
};

/// Build a `TypeTag::Struct` from string module/name + concrete type params.
/// Panics if `module`/`name` aren't valid Move identifiers — only generated
/// code calls this, with identifiers from already-compiled bytecode.
pub fn make_struct_tag(addr: Address, module: &str, name: &str, params: Vec<TypeTag>) -> TypeTag {
    TypeTag::Struct(Box::new(StructTag::new(
        addr,
        Identifier::new(module).expect("static module name is a valid Move identifier"),
        Identifier::new(name).expect("static type name is a valid Move identifier"),
        params,
    )))
}

impl MoveType for ID {
    fn type_tag() -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    fn type_tag() -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}

// -----------------------------------------------------------------------------
// Fetcher
// -----------------------------------------------------------------------------

/// Cached info for a known shared object.
#[derive(Copy, Clone, Debug)]
pub struct SharedObjectInfo {
    pub initial_shared_version: u64,
    pub mutable: bool,
}

/// Per-`ObjectId` ownership info that [`PtbBuilder`] consults to pick the
/// right [`Input`] variant. Returned by [`PtbBuilder::execute`] so callers
/// can carry it into the next builder via [`PtbBuilder::with_cache`].
#[derive(Clone, Debug, Default)]
pub struct ObjectCache {
    pub owned: HashMap<ObjectId, ObjectReference>,
    pub shared: HashMap<ObjectId, SharedObjectInfo>,
}

impl ObjectCache {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Result of a successful object lookup by a [`Fetcher`].
#[derive(Clone, Debug)]
pub enum FetchedObject {
    Owned(ObjectReference),
    Shared {
        initial_shared_version: u64,
        mutable: bool,
    },
}

/// Errors a [`Fetcher`] can return.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("object {0} not found")]
    NotFound(ObjectId),
    #[error("fetcher backend: {0}")]
    Backend(String),
}

/// Manual desugaring of `async fn fetch(...)` so [`Fetcher`] stays
/// object-safe. Implementers return `Box::pin(async move { … })`.
pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchedObject, FetchError>> + Send + 'a>>;

impl<F: Fetcher + ?Sized> Fetcher for std::sync::Arc<F> {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        F::fetch(self, id)
    }
}
impl<F: Fetcher + ?Sized> Fetcher for Box<F> {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        F::fetch(self, id)
    }
}

// -----------------------------------------------------------------------------
// Submitter
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("submit backend: {0}")]
    Backend(String),
}

pub type SubmitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TransactionEffects, SubmitError>> + Send + 'a>>;

/// Submits a signed transaction. Used by [`PtbBuilder::execute`].
pub trait Submitter: Send + Sync {
    fn submit<'a>(
        &'a self,
        tx: &'a Transaction,
        signatures: &'a [UserSignature],
    ) -> SubmitFuture<'a>;
}

impl<S: Submitter + ?Sized> Submitter for std::sync::Arc<S> {
    fn submit<'a>(
        &'a self,
        tx: &'a Transaction,
        signatures: &'a [UserSignature],
    ) -> SubmitFuture<'a> {
        S::submit(self, tx, signatures)
    }
}

impl<S: Submitter + ?Sized> Submitter for Box<S> {
    fn submit<'a>(
        &'a self,
        tx: &'a Transaction,
        signatures: &'a [UserSignature],
    ) -> SubmitFuture<'a> {
        S::submit(self, tx, signatures)
    }
}

// -----------------------------------------------------------------------------
// GasOracle
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    #[error("oracle backend: {0}")]
    Backend(String),
    #[error("no gas coins available for {0}")]
    NoGasCoins(Address),
}

pub type ListGasCoinsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ObjectReference>, OracleError>> + Send + 'a>>;
pub type RefGasPriceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u64, OracleError>> + Send + 'a>>;
pub type SuggestBudgetFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u64, OracleError>> + Send + 'a>>;

/// Backend capability the [`PtbBuilder`] uses to fill in gas slots
/// automatically when [`PtbBuilder::with_auto_gas`] is enabled.
pub trait GasOracle: Send + Sync {
    /// Owned gas coin object refs available to `owner`.
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a>;
    /// Network's current reference gas price.
    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a>;
    /// Suggested upper-bound budget for a transaction. Default impls usually
    /// return a sensible constant; smarter implementations can dry-run for a
    /// tighter estimate.
    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a>;
}

impl<O: GasOracle + ?Sized> GasOracle for std::sync::Arc<O> {
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a> {
        O::list_gas_coins(self, owner)
    }
    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
        O::reference_gas_price(self)
    }
    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
        O::suggest_gas_budget(self)
    }
}

impl<O: GasOracle + ?Sized> GasOracle for Box<O> {
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a> {
        O::list_gas_coins(self, owner)
    }
    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
        O::reference_gas_price(self)
    }
    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
        O::suggest_gas_budget(self)
    }
}

// -----------------------------------------------------------------------------
// ObjectTypeFinder
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum FindError {
    #[error("type-finder backend: {0}")]
    Backend(String),
}

pub type FindByTypeFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ObjectReference>, FindError>> + Send + 'a>>;

/// Backend capability for batched type-filtered object lookup. Used by
/// [`EffectsExt`] to resolve "all created/mutated/changed objects of type T"
/// after a transaction. Separate from [`Fetcher`] because the query shape is
/// different (batch + type filter, rather than single id + ownership).
pub trait ObjectTypeFinder: Send + Sync {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a>;
}

impl<O: ObjectTypeFinder + ?Sized> ObjectTypeFinder for std::sync::Arc<O> {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a> {
        O::find_by_type(self, type_tag, ids)
    }
}
impl<O: ObjectTypeFinder + ?Sized> ObjectTypeFinder for Box<O> {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a> {
        O::find_by_type(self, type_tag, ids)
    }
}

// -----------------------------------------------------------------------------
// EffectsExt — type-filtered object lookup after execution
// -----------------------------------------------------------------------------

/// Extension trait on [`TransactionEffects`] that decodes objects of a given
/// Move type from a transaction's changed-object list. Always used via static
/// dispatch (we never store an `EffectsExt` as `dyn`), so `async fn` is fine.
#[allow(async_fn_in_trait)]
pub trait EffectsExt {
    /// Object refs of all `T`-typed objects newly created in this tx.
    async fn created_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// Object refs of all `T`-typed objects mutated (but not created) in this tx.
    async fn mutated_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// Object refs of all `T`-typed objects either created or mutated in this tx.
    async fn changed_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
    ) -> Result<Vec<ObjectReference>, FindError>;
}

impl EffectsExt for TransactionEffects {
    async fn created_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(), changed_ids(self, ChangeKind::Created))
            .await
    }

    async fn mutated_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(), changed_ids(self, ChangeKind::Mutated))
            .await
    }

    async fn changed_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(), changed_ids(self, ChangeKind::Any))
            .await
    }
}

#[derive(Copy, Clone)]
enum ChangeKind {
    Created,
    Mutated,
    Any,
}

fn changed_ids(effects: &TransactionEffects, kind: ChangeKind) -> Vec<ObjectId> {
    use iota_sdk_types::IdOperation;
    let v1 = effects.as_v1();
    v1.changed_objects
        .iter()
        .filter(|c| {
            // Only count objects that were actually written (created/mutated).
            // `Missing` outputs are deletes; we ignore those here.
            if c.output_state.is_missing() {
                return false;
            }
            match kind {
                ChangeKind::Created => matches!(c.id_operation, IdOperation::Created),
                ChangeKind::Mutated => !matches!(c.id_operation, IdOperation::Created),
                ChangeKind::Any => true,
            }
        })
        .map(|c| c.object_id)
        .collect()
}

// -----------------------------------------------------------------------------
// DynSigner — dyn-compatible wrapper over the SDK's TransactionSigner
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("sign backend: {0}")]
    Backend(String),
}

pub type SignFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UserSignature, SignError>> + Send + 'a>>;

/// Dyn-compatible wrapper around `iota-sdk-crypto`'s sync [`IotaSigner`]. The
/// blanket impl lets any `T: IotaSigner + Send + Sync` (e.g. `Ed25519PrivateKey`)
/// be stored as `Box<dyn DynSigner>`. We use the sync trait rather than the
/// async [`TransactionSigner`] because the latter's returned future isn't
/// promised `Send`, which would break multi-threaded tokio usage.
pub trait DynSigner: Send + Sync {
    fn sign_dyn<'a>(&'a self, tx: &'a Transaction) -> SignFuture<'a>;
}

impl<T> DynSigner for T
where
    T: iota_sdk_crypto::IotaSigner + Send + Sync,
{
    fn sign_dyn<'a>(&'a self, tx: &'a Transaction) -> SignFuture<'a> {
        let result = self
            .sign_transaction(tx)
            .map_err(|e| SignError::Backend(e.to_string()));
        Box::pin(async move { result })
    }
}

/// Resolves an unknown [`ObjectId`] to a [`FetchedObject`]. Consulted by
/// [`PtbBuilder`] on cache miss. With the `graphql-client` feature, the SDK's
/// `Client` already implements this.
pub trait Fetcher: Send + Sync {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a>;
}

// -----------------------------------------------------------------------------
// PtbBuilder
// -----------------------------------------------------------------------------

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
    cache: ObjectCache,
    auto_gas: bool,
    // Each `gas*_set` flag is flipped by the corresponding setter so auto-gas
    // skips slots the user filled in explicitly.
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
            cache: ObjectCache::new(),
            auto_gas: false,
            gas_coin_set: false,
            gas_price_set: false,
            gas_budget_set: false,
        }
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

    /// Convenience for any backend that's a [`Fetcher`], [`Submitter`], and
    /// [`GasOracle`] — e.g. the GraphQL client. Clones `c` to fill all three
    /// slots, so most callers don't need an explicit `Arc`.
    pub fn with_client<C>(mut self, c: C) -> Self
    where
        C: Fetcher + Submitter + GasOracle + Clone + 'static,
    {
        self.fetcher = Some(Box::new(c.clone()));
        self.submitter = Some(Box::new(c.clone()));
        self.gas_oracle = Some(Box::new(c));
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

    /// Cache `id` as a shared object with the given initial shared version
    /// and default mutability.
    pub fn register_shared(&mut self, id: ObjectId, initial_shared_version: u64, mutable: bool) {
        self.cache.shared.insert(
            id,
            SharedObjectInfo {
                initial_shared_version,
                mutable,
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
                mutable,
            } => self.register_shared(id, initial_shared_version, mutable),
        }
        Ok(())
    }

    /// Cache hit → corresponding [`Input`] variant.
    /// Cache miss with [`Fetcher`] attached → fetch, cache, return.
    /// Cache miss without fetcher → bare-id input (SDK errors at finish time
    /// if it never gets resolved).
    pub async fn resolve_object(&mut self, id: ObjectId) -> Argument {
        if let Some(r) = self.cache.owned.get(&id).cloned() {
            return self.inner.input(Input::ImmutableOrOwned(r));
        }
        if let Some(info) = self.cache.shared.get(&id).copied() {
            return self.inner.input(Input::Shared(SharedObjectReference {
                object_id: id,
                initial_shared_version: Version::from_u64(info.initial_shared_version),
                mutable: info.mutable,
            }));
        }

        // Cache miss: try the fetcher, if any.
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
                mutable,
            })) => {
                self.cache.shared.insert(
                    id,
                    SharedObjectInfo {
                        initial_shared_version,
                        mutable,
                    },
                );
                self.inner.input(Input::Shared(SharedObjectReference {
                    object_id: id,
                    initial_shared_version: Version::from_u64(initial_shared_version),
                    mutable,
                }))
            }
            // No fetcher, or fetch failed: SDK will error at finish-time if
            // the bare id never gets resolved.
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

    /// Add a BCS-encoded value as a `Pure` input.
    pub fn pure<T: serde::Serialize>(&mut self, value: T) -> Argument {
        self.inner.pure(value)
    }

    // ---- Convenience pass-throughs to the inner SDK builder ----------------

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
            // Skip coins already used as command args — same as the SDK's
            // `resolve_ptb(default_gas=true)`.
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
            self.inner.gas_budget(oracle.suggest_gas_budget().await?);
        }
        Ok(())
    }
}

/// Errors from [`PtbBuilder::execute`].
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
    fn finish(e: iota_sdk_transaction_builder::error::Error) -> Self {
        Self::Finish(e.to_string())
    }
}

// -----------------------------------------------------------------------------
// Built-in Fetcher impl for the GraphQL client (feature-gated)
// -----------------------------------------------------------------------------

#[cfg(feature = "graphql-client")]
impl Fetcher for iota_sdk_graphql_client::Client {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        Box::pin(async move {
            let obj = self
                .object(id, None)
                .await
                .map_err(|e| FetchError::Backend(e.to_string()))?
                .ok_or(FetchError::NotFound(id))?;
            match obj.owner() {
                iota_sdk_types::Owner::Shared(initial_shared_version) => {
                    Ok(FetchedObject::Shared {
                        initial_shared_version: initial_shared_version.as_u64(),
                        // Default to mutable — the safer choice for entry
                        // points that take `&mut`. Pre-`register_shared` if
                        // you need it immutable.
                        mutable: true,
                    })
                }
                iota_sdk_types::Owner::Address(_)
                | iota_sdk_types::Owner::Object(_)
                | iota_sdk_types::Owner::Immutable => Ok(FetchedObject::Owned(obj.object_ref())),
                other => Err(FetchError::Backend(format!("unknown owner: {other:?}"))),
            }
        })
    }
}

#[cfg(feature = "graphql-client")]
impl Submitter for iota_sdk_graphql_client::Client {
    fn submit<'a>(
        &'a self,
        tx: &'a Transaction,
        signatures: &'a [UserSignature],
    ) -> SubmitFuture<'a> {
        Box::pin(async move {
            self.execute_tx(signatures, tx, None)
                .await
                .map_err(|e| SubmitError::Backend(e.to_string()))
        })
    }
}

#[cfg(feature = "graphql-client")]
impl ObjectTypeFinder for iota_sdk_graphql_client::Client {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let filter = iota_sdk_graphql_client::query_types::ObjectFilter {
                type_: Some(type_tag.to_string()),
                owner: None,
                object_ids: Some(ids),
            };
            let page = self
                .objects(filter, iota_sdk_graphql_client::PaginationFilter::default())
                .await
                .map_err(|e| FindError::Backend(e.to_string()))?;
            Ok(page.data.iter().map(|o| o.object_ref()).collect())
        })
    }
}

#[cfg(feature = "graphql-client")]
impl GasOracle for iota_sdk_graphql_client::Client {
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a> {
        Box::pin(async move {
            let page = self
                .gas_coins(owner, iota_sdk_graphql_client::PaginationFilter::default())
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?;
            // `Coin` has only id+balance. Re-fetch each as an Object so we get
            // the full reference (id + version + digest).
            let mut refs = Vec::with_capacity(page.data.len());
            for coin in &page.data {
                let obj = self
                    .object(*coin.id(), None)
                    .await
                    .map_err(|e| OracleError::Backend(e.to_string()))?
                    .ok_or_else(|| {
                        OracleError::Backend(format!("coin {} disappeared", coin.id()))
                    })?;
                refs.push(obj.object_ref());
            }
            Ok(refs)
        })
    }

    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
        Box::pin(async move {
            self.reference_gas_price(None)
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?
                .ok_or_else(|| OracleError::Backend("reference gas price unavailable".into()))
        })
    }

    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
        // Generous default upper bound. Real dry-run-based estimation is
        // future work — see PLAN.md.
        Box::pin(async move { Ok(50_000_000u64) })
    }
}

// -----------------------------------------------------------------------------
// Per-Move-primitive marker traits
// -----------------------------------------------------------------------------
//
// Closed-impl sets bounded by `PTBArgument`. `into_argument` is async to match
// the codegen'd `Argument*` traits — primitive impls themselves don't await.

macro_rules! decl_pure_trait {
    ($trait_name:ident, $ty:ty) => {
        pub trait $trait_name: PTBArgument {
            #[allow(async_fn_in_trait)] // the trait is consumed by async generated fns; not used as `dyn`
            async fn into_argument(self, b: &mut PtbBuilder) -> Argument
            where
                Self: Sized,
            {
                b.inner.apply_argument(self)
            }
        }
        impl $trait_name for $ty {}
        impl $trait_name for Argument {}
    };
}

decl_pure_trait!(PureBool, bool);
decl_pure_trait!(PureU8, u8);
decl_pure_trait!(PureU16, u16);
decl_pure_trait!(PureU32, u32);
decl_pure_trait!(PureU64, u64);
decl_pure_trait!(PureU128, u128);
decl_pure_trait!(PureAddress, Address);
decl_pure_trait!(PureString, String);

/// Generic marker for `vector<T>` — closed to `Vec<T>` (where T:MoveArg) and
/// `Argument`.
pub trait PureVec<T>: PTBArgument {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        b.inner.apply_argument(self)
    }
}
impl<T: MoveArg> PureVec<T> for Vec<T> {}
impl<T> PureVec<T> for Argument {}

/// Generic marker for `Option<T>` — closed to `Option<T>` (where T:MoveArg)
/// and `Argument`.
pub trait PureOption<T>: PTBArgument {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        b.inner.apply_argument(self)
    }
}
impl<T: MoveArg> PureOption<T> for Option<T> {}
impl<T> PureOption<T> for Argument {}
