//! Backend interfaces consumed by [`move_bindgen_runtime::PtbBuilder`], plus a
//! ready-made integration on top of `iota_sdk_graphql_client::Client`.
//!
//! Provides:
//! - The backend traits ([`Fetcher`], [`Submitter`], [`GasOracle`],
//!   [`DryRunner`], [`ObjectTypeFinder`]) that `PtbBuilder` calls into.
//! - Impls of those traits for `iota_sdk_graphql_client::Client` so
//!   `PtbBuilder::with_client(graphql_client)` "just works".
//! - [`ClientExt`] — typed read helpers on `Client` (e.g. [`ClientExt::get_object`]).

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use iota_sdk_graphql_client::{
    query_types::{EventFilter, ObjectFilter},
    Client, PaginationFilter,
};
use iota_sdk_transaction_builder::types::MoveType;
use iota_sdk_transaction_builder::unresolved::Argument;
use iota_sdk_types::{
    Address, Digest, ExecutionStatus, Object, ObjectId, ObjectOut, ObjectReference, Owner,
    StructTag, Transaction, TransactionEffects, TypeTag, UserSignature, Version,
};

// -----------------------------------------------------------------------------
// Fetcher
// -----------------------------------------------------------------------------

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

/// Resolves an unknown [`ObjectId`] to a [`FetchedObject`]. Consulted by
/// `PtbBuilder` on cache miss.
pub trait Fetcher: Send + Sync {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a>;
}

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

/// Submits a signed transaction. Used by `PtbBuilder::execute`.
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
    #[error("oracle does not support `{0}`")]
    Unsupported(&'static str),
}

pub type ListGasCoinsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ObjectReference>, OracleError>> + Send + 'a>>;
pub type RefGasPriceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u64, OracleError>> + Send + 'a>>;
pub type SuggestBudgetFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u64, OracleError>> + Send + 'a>>;
pub type DryRunEstimateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u64, OracleError>> + Send + 'a>>;

/// Backend capability `PtbBuilder` uses to fill gas slots automatically when
/// `with_auto_gas` is enabled.
pub trait GasOracle: Send + Sync {
    /// Owned gas coin object refs available to `owner`.
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a>;
    /// Network's current reference gas price.
    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a>;
    /// Conservative fallback budget when dry-run isn't viable. Implementations
    /// usually return a generous constant.
    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a>;
    /// Dry-run `tx` and return the gas it actually used. `PtbBuilder` adds a
    /// safety margin and uses this when `with_auto_gas` is on. The default
    /// returns `Err(OracleError::Unsupported)` so backends can opt in.
    fn dry_run_estimate<'a>(&'a self, _tx: &'a Transaction) -> DryRunEstimateFuture<'a> {
        Box::pin(async { Err(OracleError::Unsupported("dry_run_estimate")) })
    }
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
    fn dry_run_estimate<'a>(&'a self, tx: &'a Transaction) -> DryRunEstimateFuture<'a> {
        O::dry_run_estimate(self, tx)
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
    fn dry_run_estimate<'a>(&'a self, tx: &'a Transaction) -> DryRunEstimateFuture<'a> {
        O::dry_run_estimate(self, tx)
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
/// `EffectsExt` to resolve "all created/mutated/changed objects of type T"
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
// EventReader
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum EventReaderError {
    #[error("event-reader backend: {0}")]
    Backend(String),
}

pub type EventsByTxFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Vec<u8>>, EventReaderError>> + Send + 'a>>;

/// Backend capability for reading events emitted by a specific transaction
/// filtered by Move type. Returns the BCS payload of each matching event;
/// callers BCS-decode into the typed Rust struct. Used by `EffectsExt` to
/// surface typed events after a transaction.
pub trait EventReader: Send + Sync {
    fn events_by_tx<'a>(&'a self, digest: Digest, type_tag: TypeTag) -> EventsByTxFuture<'a>;
}

impl<R: EventReader + ?Sized> EventReader for std::sync::Arc<R> {
    fn events_by_tx<'a>(&'a self, digest: Digest, type_tag: TypeTag) -> EventsByTxFuture<'a> {
        R::events_by_tx(self, digest, type_tag)
    }
}
impl<R: EventReader + ?Sized> EventReader for Box<R> {
    fn events_by_tx<'a>(&'a self, digest: Digest, type_tag: TypeTag) -> EventsByTxFuture<'a> {
        R::events_by_tx(self, digest, type_tag)
    }
}

// -----------------------------------------------------------------------------
// DryRunner — read-path execution backing `PtbBuilder::inspect()`
// -----------------------------------------------------------------------------

/// Per-command per-slot BCS return-value bytes plus the dry-run effects.
/// Returned by `PtbBuilder::inspect`.
#[derive(Debug, Clone)]
pub struct InspectResult {
    pub effects: TransactionEffects,
    /// Indexed `[command_idx][return_slot]`.
    pub returns: Vec<Vec<Vec<u8>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("argument {0:?} is not a command result")]
    NotAReturn(Argument),
    #[error("no return slot for argument {0:?}")]
    NotFound(Argument),
    #[error("bcs decode: {0}")]
    Bcs(#[from] bcs::Error),
}

impl InspectResult {
    /// Decode the return value referenced by `arg`. `Argument::Result(idx)`
    /// decodes the first slot of command `idx` (correct for single-return
    /// calls); `Argument::NestedResult(idx, sub)` indexes a specific slot of
    /// a multi-return call.
    pub fn decode<T: serde::de::DeserializeOwned>(&self, arg: Argument) -> Result<T, DecodeError> {
        let bytes: &[u8] = match arg {
            Argument::Result(cmd) => self
                .returns
                .get(cmd as usize)
                .and_then(|cmd_returns| cmd_returns.first())
                .map(Vec::as_slice)
                .ok_or(DecodeError::NotFound(arg))?,
            Argument::NestedResult(cmd, sub) => self
                .returns
                .get(cmd as usize)
                .and_then(|cmd_returns| cmd_returns.get(sub as usize))
                .map(Vec::as_slice)
                .ok_or(DecodeError::NotFound(arg))?,
            // `Gas`, `Input(_)`, or any future variant: not a return value.
            _ => return Err(DecodeError::NotAReturn(arg)),
        };
        bcs::from_bytes(bytes).map_err(DecodeError::Bcs)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DryRunError {
    #[error("dry-run backend: {0}")]
    Backend(String),
    #[error("dry-run aborted: {0}")]
    Aborted(String),
}

pub type DryRunFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InspectResult, DryRunError>> + Send + 'a>>;

/// Backend capability for dry-running a transaction and returning its full
/// effects + per-slot return values. Used by `PtbBuilder::inspect`.
pub trait DryRunner: Send + Sync {
    fn dry_run<'a>(&'a self, tx: &'a Transaction) -> DryRunFuture<'a>;
}

impl<D: DryRunner + ?Sized> DryRunner for std::sync::Arc<D> {
    fn dry_run<'a>(&'a self, tx: &'a Transaction) -> DryRunFuture<'a> {
        D::dry_run(self, tx)
    }
}
impl<D: DryRunner + ?Sized> DryRunner for Box<D> {
    fn dry_run<'a>(&'a self, tx: &'a Transaction) -> DryRunFuture<'a> {
        D::dry_run(self, tx)
    }
}

// -----------------------------------------------------------------------------
// Backend trait impls for the GraphQL client
// -----------------------------------------------------------------------------

impl Fetcher for Client {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        Box::pin(async move {
            let obj = self
                .object(id, None)
                .await
                .map_err(|e| FetchError::Backend(e.to_string()))?
                .ok_or(FetchError::NotFound(id))?;
            match obj.owner() {
                Owner::Shared(initial_shared_version) => Ok(FetchedObject::Shared {
                    initial_shared_version: initial_shared_version.as_u64(),
                    // Default to mutable — the safer choice for entry points
                    // that take `&mut`. Pre-`register_shared` if you need it
                    // immutable.
                    mutable: true,
                }),
                Owner::Address(_) | Owner::Object(_) | Owner::Immutable => {
                    Ok(FetchedObject::Owned(obj.object_ref()))
                }
                other => Err(FetchError::Backend(format!("unknown owner: {other:?}"))),
            }
        })
    }
}

impl Submitter for Client {
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

impl ObjectTypeFinder for Client {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let filter = ObjectFilter {
                type_: Some(type_tag.to_string()),
                owner: None,
                object_ids: Some(ids),
            };
            let page = self
                .objects(filter, PaginationFilter::default())
                .await
                .map_err(|e| FindError::Backend(e.to_string()))?;
            Ok(page.data.iter().map(|o| o.object_ref()).collect())
        })
    }
}

impl EventReader for Client {
    fn events_by_tx<'a>(&'a self, digest: Digest, type_tag: TypeTag) -> EventsByTxFuture<'a> {
        Box::pin(async move {
            let filter = EventFilter {
                emitting_module: None,
                event_type: Some(type_tag.to_string()),
                sender: None,
                transaction_digest: Some(digest.to_string()),
            };
            let mut bcs_payloads: Vec<Vec<u8>> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = self
                    .events(
                        filter.clone(),
                        PaginationFilter {
                            cursor: cursor.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| EventReaderError::Backend(e.to_string()))?;
                for ev in &page.data {
                    let bytes = base64ct_decode(&ev.bcs.0)
                        .map_err(|e| EventReaderError::Backend(format!("base64: {e}")))?;
                    bcs_payloads.push(bytes);
                }
                if !page.page_info.has_next_page {
                    break;
                }
                cursor = page.page_info.end_cursor;
            }
            Ok(bcs_payloads)
        })
    }
}

fn base64ct_decode(s: &str) -> Result<Vec<u8>, base64ct::Error> {
    use base64ct::Encoding;
    base64ct::Base64::decode_vec(s)
}

impl GasOracle for Client {
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a> {
        Box::pin(async move {
            let page = self
                .gas_coins(owner, PaginationFilter::default())
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?;
            // `Coin` carries only id+balance. Re-fetch each as an `Object` to
            // get the full reference (id + version + digest).
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
        // Conservative fallback only used when `dry_run_estimate` fails.
        Box::pin(async move { Ok(50_000_000u64) })
    }

    fn dry_run_estimate<'a>(&'a self, tx: &'a Transaction) -> DryRunEstimateFuture<'a> {
        Box::pin(async move {
            let result = self
                .dry_run_tx(tx, /* skip_checks */ true)
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?;
            if let Some(err) = result.error {
                return Err(OracleError::Backend(format!("dry-run aborted: {err}")));
            }
            let effects = result
                .effects
                .ok_or_else(|| OracleError::Backend("dry-run returned no effects".into()))?;
            Ok(effects.gas_summary().gas_used())
        })
    }
}

impl DryRunner for Client {
    fn dry_run<'a>(&'a self, tx: &'a Transaction) -> DryRunFuture<'a> {
        Box::pin(async move {
            let result = self
                .dry_run_tx(tx, /* skip_checks */ true)
                .await
                .map_err(|e| DryRunError::Backend(e.to_string()))?;
            if let Some(err) = result.error {
                return Err(DryRunError::Aborted(err));
            }
            let effects = result
                .effects
                .ok_or_else(|| DryRunError::Backend("dry-run returned no effects".into()))?;
            let returns = result
                .results
                .into_iter()
                .map(|cmd| cmd.return_values.into_iter().map(|r| r.bcs).collect())
                .collect();
            Ok(InspectResult { effects, returns })
        })
    }
}

// -----------------------------------------------------------------------------
// ClientExt — typed read helpers on the GraphQL client
// -----------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GetError {
    #[error("client backend: {0}")]
    Backend(String),
    #[error("object {0} not found")]
    NotFound(ObjectId),
    #[error("object {id} is not a Move struct (looks like a package)")]
    NotAStruct { id: ObjectId },
    #[error("type mismatch for {id}: expected {expected}, got {actual}")]
    TypeMismatch {
        id: ObjectId,
        expected: Box<StructTag>,
        actual: Box<StructTag>,
    },
    #[error("bcs decode for {id}: {source}")]
    Bcs {
        id: ObjectId,
        #[source]
        source: bcs::Error,
    },
}

/// Typed read helpers on `iota_sdk_graphql_client::Client`. Sibling methods
/// will follow the `get_X` / `list_X` pattern as we add them
/// (`get_dynamic_field`, `list_objects_owned_by`, …).
#[allow(async_fn_in_trait)] // static dispatch only — never used as `dyn`
pub trait ClientExt {
    /// Fetch the object at `id` and BCS-decode its contents as `T`.
    /// Verifies the on-chain type matches `T::type_tag()` before decoding.
    async fn get_object<T>(&self, id: ObjectId) -> Result<T, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// Batch variant of [`ClientExt::get_object`]. Walks every page returned
    /// by the GraphQL backend, type-checks and decodes each object as `T`.
    /// Returns the objects in the order the indexer chose, which is *not*
    /// necessarily input order. Errors if any id is missing or has the wrong
    /// type.
    async fn get_objects<T>(&self, ids: &[ObjectId]) -> Result<Vec<T>, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// Read the dynamic field at `(parent, key)` and decode its value as `V`.
    /// `K::type_tag()` is sent as the field's name type; `V::type_tag()` is
    /// checked against the indexer's reported value type before decoding.
    async fn get_dynamic_field<K, V>(&self, parent: ObjectId, key: K) -> Result<V, GetError>
    where
        K: MoveType + serde::Serialize,
        V: MoveType + serde::de::DeserializeOwned;

    /// Block until the indexer reflects every changed object in `effects`.
    ///
    /// `execute()` returns once the validators have processed the tx, but the
    /// GraphQL indexer ingests checkpoints asynchronously, so a subsequent
    /// `get_object` may briefly serve the *pre-tx* state. Call this between
    /// `execute()` and any read that depends on the new state.
    ///
    /// Errors if the tx didn't succeed or any object hasn't appeared at its
    /// post-execution version before [`WaitOptions::timeout`].
    async fn wait_for_effects(
        &self,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<(), WaitError>;

    /// Like [`ClientExt::wait_for_effects`] for a single object id, returning
    /// the BCS-decoded object once the indexer is caught up. `id` must appear
    /// in `effects.changed_objects` and must not be a deletion.
    async fn wait_for_object<T>(
        &self,
        id: ObjectId,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<T, WaitError>
    where
        T: MoveType + serde::de::DeserializeOwned;
}

impl ClientExt for Client {
    async fn get_object<T>(&self, id: ObjectId) -> Result<T, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        let obj = self
            .object(id, None)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .ok_or(GetError::NotFound(id))?;
        decode_object_as::<T>(id, &obj)
    }

    async fn get_objects<T>(&self, ids: &[ObjectId]) -> Result<Vec<T>, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let filter = ObjectFilter {
            type_: None,
            owner: None,
            object_ids: Some(ids.to_vec()),
        };
        let mut objs: Vec<Object> = Vec::with_capacity(ids.len());
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .objects(
                    filter.clone(),
                    PaginationFilter {
                        cursor: cursor.clone(),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| GetError::Backend(e.to_string()))?;
            objs.extend(page.data);
            if !page.page_info.has_next_page {
                break;
            }
            cursor = page.page_info.end_cursor;
        }

        // Verify every requested id came back. The indexer silently drops ids
        // it doesn't know — turn that into a typed error.
        for id in ids {
            if !objs.iter().any(|o| o.object_id() == *id) {
                return Err(GetError::NotFound(*id));
            }
        }

        objs.iter()
            .map(|o| decode_object_as::<T>(o.object_id(), o))
            .collect()
    }

    async fn get_dynamic_field<K, V>(&self, parent: ObjectId, key: K) -> Result<V, GetError>
    where
        K: MoveType + serde::Serialize,
        V: MoveType + serde::de::DeserializeOwned,
    {
        let parent_addr: Address = *parent.as_address();
        let output = self
            .dynamic_field(parent_addr, K::type_tag(), key)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .ok_or(GetError::NotFound(parent))?;
        let dfv = output.value.as_ref().ok_or(GetError::NotFound(parent))?;
        let expected = V::type_tag();
        if dfv.type_ != expected {
            // We don't have a struct tag for the actual type unconditionally
            // (it could be a primitive). Reuse `Backend` for the message.
            return Err(GetError::Backend(format!(
                "dynamic field on {parent}: expected value type {expected}, got {actual}",
                actual = dfv.type_,
            )));
        }
        bcs::from_bytes::<V>(&dfv.bcs).map_err(|source| GetError::Bcs { id: parent, source })
    }

    async fn wait_for_effects(
        &self,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<(), WaitError> {
        require_success(effects)?;
        let deadline = Instant::now() + opts.timeout;
        // Sequential is fine: indexer ingests one checkpoint at a time, so
        // once the first id is visible the rest typically are too.
        for (id, version) in target_versions(effects) {
            poll_for_version(self, id, version, deadline, opts.interval).await?;
        }
        Ok(())
    }

    async fn wait_for_object<T>(
        &self,
        id: ObjectId,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<T, WaitError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        require_success(effects)?;
        let version = target_versions(effects)
            .into_iter()
            .find(|(oid, _)| *oid == id)
            .map(|(_, v)| v)
            .ok_or(WaitError::NotInEffects(id))?;
        let deadline = Instant::now() + opts.timeout;
        let obj = poll_for_version(self, id, version, deadline, opts.interval).await?;
        decode_object_as::<T>(id, &obj).map_err(WaitError::Decode)
    }
}

// -----------------------------------------------------------------------------
// Wait support
// -----------------------------------------------------------------------------

/// Tunables for [`ClientExt::wait_for_effects`] / [`ClientExt::wait_for_object`].
/// `Default` polls every 250ms with a 10s timeout — enough for normal indexer lag.
#[derive(Clone, Copy, Debug)]
pub struct WaitOptions {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(250),
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    #[error("transaction did not succeed: {0:?}")]
    TxFailed(ExecutionStatus),
    #[error("object {0} is not in transaction effects")]
    NotInEffects(ObjectId),
    #[error("timed out waiting for {id} at version {expected}")]
    Timeout { id: ObjectId, expected: Version },
    #[error("client backend: {0}")]
    Backend(String),
    #[error(transparent)]
    Decode(#[from] GetError),
}

#[allow(clippy::result_large_err)]
fn require_success(effects: &TransactionEffects) -> Result<(), WaitError> {
    match effects.status() {
        ExecutionStatus::Success => Ok(()),
        other => Err(WaitError::TxFailed(other.clone())),
    }
}

/// `(id, expected indexer version)` for every non-deletion in `effects`.
/// `ObjectWrite` entries inherit `lamport_version`; `PackageWrite` carries
/// its own version. `Missing` (deletion/wrap) is skipped.
fn target_versions(effects: &TransactionEffects) -> Vec<(ObjectId, Version)> {
    let v1 = effects.as_v1();
    let lamport = v1.lamport_version;
    v1.changed_objects
        .iter()
        .filter_map(|ch| match &ch.output_state {
            ObjectOut::ObjectWrite { .. } => Some((ch.object_id, lamport)),
            ObjectOut::PackageWrite { version, .. } => Some((ch.object_id, *version)),
            ObjectOut::Missing => None,
            _ => None,
        })
        .collect()
}

async fn poll_for_version(
    client: &Client,
    id: ObjectId,
    version: Version,
    deadline: Instant,
    interval: Duration,
) -> Result<Object, WaitError> {
    loop {
        match client.object(id, Some(version)).await {
            Ok(Some(obj)) => return Ok(obj),
            Ok(None) => {}
            Err(e) => return Err(WaitError::Backend(e.to_string())),
        }
        if Instant::now() >= deadline {
            return Err(WaitError::Timeout {
                id,
                expected: version,
            });
        }
        tokio::time::sleep(interval).await;
    }
}

fn decode_object_as<T>(id: ObjectId, obj: &Object) -> Result<T, GetError>
where
    T: MoveType + serde::de::DeserializeOwned,
{
    let move_struct = obj.as_struct_opt().ok_or(GetError::NotAStruct { id })?;

    let expected = match T::type_tag() {
        TypeTag::Struct(s) => s,
        // T isn't a struct type → can't be the contents of an object.
        _ => {
            return Err(GetError::TypeMismatch {
                id,
                expected: Box::new(StructTag::new(
                    Address::ZERO,
                    iota_sdk_types::Identifier::new("∅").expect("placeholder"),
                    iota_sdk_types::Identifier::new("∅").expect("placeholder"),
                    Vec::new(),
                )),
                actual: Box::new(move_struct.struct_tag().clone()),
            });
        }
    };

    let actual = move_struct.struct_tag();
    if &*expected != actual {
        return Err(GetError::TypeMismatch {
            id,
            expected,
            actual: Box::new(actual.clone()),
        });
    }

    bcs::from_bytes::<T>(move_struct.contents()).map_err(|source| GetError::Bcs { id, source })
}
