//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Provides [`PtbBuilder`] (the high-level builder), [`DynSigner`], the
//! per-Move-primitive marker traits (`PureBool`, `PureU64`, …) used as bounds
//! on generated call builders, [`EffectsExt`] for post-execution object
//! lookup, and re-exports the SDK types generated code needs. Backend traits
//! ([`Fetcher`], [`Submitter`], [`GasOracle`], [`DryRunner`],
//! [`ObjectTypeFinder`]) and the GraphQL-client integration live in
//! `move-bindgen-ext` and are re-exported here.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

pub use iota_sdk_transaction_builder::{
    // We deliberately *don't* re-export `iota_sdk_transaction_builder::types::MoveType`
    // any more — `move-bindgen-ext-iota` defines its own
    // `MoveType` trait with a runtime-package-id shape, mirroring the
    // Sui side. Use that one.
    types::MoveArg,
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
pub use move_bindgen_ext::{
    ClientExt, DecodeError, DryRunError, DryRunEstimateFuture, DryRunFuture, DryRunner,
    EventReader, EventReaderError, EventsByTxFuture, FetchError, FetchFuture, FetchedObject,
    Fetcher, FindByTypeFuture, FindError, GasOracle, GetError, InspectResult, ListGasCoinsFuture,
    MoveType, NoPackage, ObjectTypeFinder, OracleError, PackageAddrs, PackageRegistry,
    RefGasPriceFuture, SubmitError, SubmitFuture, Submitter, SuggestBudgetFuture, WaitError,
    WaitOptions,
};
pub use primitive_types::U256;

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
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "ID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "UID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}

// `ID` is `copy + drop + store` in Move — pass-by-value as a Move-call arg
// works. Implementing `MoveArg` makes `PTBArgument for ID` available via the
// SDK's blanket, and `PureID` can route through `apply_argument`.
impl MoveArg for ID {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("bcs serialization of ID never fails"))
    }
}

impl From<Address> for ID {
    fn from(bytes: Address) -> Self {
        Self { bytes }
    }
}

impl From<ObjectId> for ID {
    fn from(id: ObjectId) -> Self {
        Self {
            bytes: Address::from(id),
        }
    }
}

// -----------------------------------------------------------------------------
// ObjectCache — per-id ownership info `PtbBuilder` carries between calls
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
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// Object refs of all `T`-typed objects mutated (but not created) in this tx.
    async fn mutated_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// Object refs of all `T`-typed objects either created or mutated in this tx.
    async fn changed_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// `created_in` followed by a typed batch fetch — returns fully decoded
    /// `T`s for every object of type `T` newly created in this tx.
    async fn created_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// `mutated_in` followed by a typed batch fetch.
    async fn mutated_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// `changed_in` followed by a typed batch fetch.
    async fn changed_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// BCS-decode every event of type `E` emitted by this tx.
    async fn events_of_type<E>(
        &self,
        reader: &(impl EventReader + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<E>, EventsError>
    where
        E: MoveType + serde::de::DeserializeOwned;
}

impl EffectsExt for TransactionEffects {
    async fn created_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(addrs), changed_ids(self, ChangeKind::Created))
            .await
    }

    async fn mutated_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(addrs), changed_ids(self, ChangeKind::Mutated))
            .await
    }

    async fn changed_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(addrs), changed_ids(self, ChangeKind::Any))
            .await
    }

    async fn created_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        decoded_for(self, client, ChangeKind::Created, addrs).await
    }

    async fn mutated_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        decoded_for(self, client, ChangeKind::Mutated, addrs).await
    }

    async fn changed_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        decoded_for(self, client, ChangeKind::Any, addrs).await
    }

    async fn events_of_type<E>(
        &self,
        reader: &(impl EventReader + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<E>, EventsError>
    where
        E: MoveType + serde::de::DeserializeOwned,
    {
        let digest = self.as_v1().transaction_digest;
        let payloads = reader
            .events_by_tx(digest, E::type_tag(addrs))
            .await
            .map_err(EventsError::Reader)?;
        payloads
            .iter()
            .map(|b| bcs::from_bytes::<E>(b).map_err(EventsError::Bcs))
            .collect()
    }
}

/// Errors from the `*_decoded` family on [`EffectsExt`].
#[derive(Debug, thiserror::Error)]
pub enum EffectsDecodeError {
    #[error(transparent)]
    Find(#[from] FindError),
    #[error(transparent)]
    Get(#[from] GetError),
}

/// Errors from [`EffectsExt::events_of_type`].
#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error(transparent)]
    Reader(#[from] EventReaderError),
    #[error("bcs decode of event payload: {0}")]
    Bcs(bcs::Error),
}

async fn decoded_for<T>(
    effects: &TransactionEffects,
    client: &(impl ObjectTypeFinder + ClientExt),
    kind: ChangeKind,
    addrs: &impl PackageAddrs,
) -> Result<Vec<T>, EffectsDecodeError>
where
    T: MoveType + serde::de::DeserializeOwned,
{
    let refs = client
        .find_by_type(T::type_tag(addrs), changed_ids(effects, kind))
        .await?;
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<ObjectId> = refs.iter().map(|r| r.object_id).collect();
    Ok(client.get_objects::<T>(&ids, addrs).await?)
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

/// Dyn-compatible wrapper around `iota-sdk-crypto`'s sync `IotaSigner`. The
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
    dry_runner: Option<Box<dyn DryRunner>>,
    cache: ObjectCache,
    auto_gas: bool,
    /// Runtime package-address registry. Generated `move_call*` /
    /// `MoveType::type_tag` callsites resolve their package's on-chain
    /// address through this — callers register addresses once per
    /// PTB with `with_package`.
    packages: std::collections::HashMap<std::any::TypeId, Address>,
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
            dry_runner: None,
            cache: ObjectCache::new(),
            auto_gas: false,
            packages: std::collections::HashMap::new(),
            gas_coin_set: false,
            gas_price_set: false,
            gas_budget_set: false,
        }
    }

    /// Register a Move package's on-chain address against its generated
    /// `Package` marker. Generated `move_call*` and `MoveType::type_tag`
    /// callsites read this map. Call once per package per PTB.
    pub fn with_package<P: 'static>(&mut self, addr: Address) -> &mut Self {
        self.packages.insert(std::any::TypeId::of::<P>(), addr);
        self
    }
}

impl PackageAddrs for PtbBuilder {
    fn package_id<P: 'static>(&self) -> Address {
        *self.packages.get(&std::any::TypeId::of::<P>()).unwrap_or_else(|| {
            panic!(
                "PtbBuilder: no address registered for package `{}` — \
                 call `b.with_package::<{}>(addr)` before building the PTB",
                std::any::type_name::<P>(),
                std::any::type_name::<P>(),
            )
        })
    }
}

impl PtbBuilder {

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
            // Try a real dry-run-based estimate; fall back to the constant
            // suggestion if the oracle doesn't support dry-run.
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
    fn finish(e: iota_sdk_transaction_builder::error::Error) -> Self {
        Self::Finish(e.to_string())
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
decl_pure_trait!(PureID, ID);

/// Generic fallback trait used by codegen wherever it would otherwise
/// emit a bare `impl PTBArgument` — i.e. for generic type parameters
/// (`fun foo<T>(x: T)`) and for foreign-framework types whose specific
/// marker traits aren't available (e.g. `iota::object::UID`). Has the
/// same closed-impl shape as the per-package `ArgumentX` traits, so
/// callers can pass `Argument`, `ObjectId` (cache-aware), `ObjectReference`,
/// `Shared<ObjectId>`, `SharedMut<ObjectId>`, or `Receiving<ObjectId>`.
/// Loses per-type safety in arg position but keeps `.into_argument(b)`
/// resolvable from generated code.
pub trait ArgumentObject<T>: PTBArgument {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        b.inner.apply_argument(self)
    }
}
impl<T> ArgumentObject<T> for Argument {}
impl<T> ArgumentObject<T> for ObjectId {
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument {
        b.resolve_object(self).await
    }
}
impl<T> ArgumentObject<T> for ObjectReference {}
impl<T> ArgumentObject<T> for Shared<ObjectId> {}
impl<T> ArgumentObject<T> for SharedMut<ObjectId> {}
impl<T> ArgumentObject<T> for Receiving<ObjectId> {}

// `primitive_types::U256` ships a serde impl (`impl-serde`) that always uses
// hex strings, which is incompatible with Move's BCS-as-32-LE-bytes wire
// format. The codegen attaches `#[serde(with = "u256_le")]` to every U256
// struct/enum field, and the `PureU256` impl below pushes a `Pure` input with
// the correct LE bytes — bypassing the SDK's broken `MoveArg for U256`.
pub mod u256_le {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::U256;

    pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
        let bytes = v.to_little_endian();
        bytes.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        let bytes = <[u8; 32]>::deserialize(d)?;
        Ok(U256::from_little_endian(&bytes))
    }
}

pub trait PureU256 {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized;
}

impl PureU256 for U256 {
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument {
        b.inner.input(Input::Pure(self.to_little_endian().to_vec()))
    }
}

impl PureU256 for Argument {
    async fn into_argument(self, _b: &mut PtbBuilder) -> Argument {
        self
    }
}

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
