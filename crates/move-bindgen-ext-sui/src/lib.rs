//! Backend interfaces for the Sui chain — chain-generic trait shapes
//! `move_bindgen_runtime_sui::PtbBuilder` consumes, plus a place for
//! GraphQL-client integration once `sui-graphql`'s schema is wired up.
//!
//! Mirror of `move-bindgen-ext-iota` for Sui. The trait + type
//! definitions live here (rather than in `move-bindgen-runtime-sui`)
//! so `runtime-sui` and any future ext-side impls can share a single
//! `MoveType` / `MoveArg` / `PTBArgument` definition without a
//! circular dep.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use sui_sdk_types::{
    Address, Digest, ObjectReference, Transaction, TransactionEffects, TypeTag, UserSignature,
};
pub use sui_transaction_builder::Argument;

pub use primitive_types::U256;

/// On Sui, an object's id is just a 32-byte address — `sui-sdk-types`
/// has no separate `ObjectId` type. The alias keeps generated code
/// flavour-agnostic so it can write `ObjectId` regardless of which
/// runtime it's compiled against.
pub type ObjectId = Address;

// -----------------------------------------------------------------------------
// MoveType: maps a Rust type to its Move TypeTag.
// -----------------------------------------------------------------------------

/// Marker type used by `MoveType` impls that don't belong to a
/// generated package (primitives, framework types, etc.). Looking up a
/// `NoPackage` address in `PackageAddrs` is a programmer error — those
/// impls override `type_tag` and never call the trait's default body.
pub struct NoPackage;

/// Runtime map from a generated `Package` marker type to its on-chain
/// address. Both [`PtbBuilder`] (PTB calls) and [`PackageRegistry`]
/// (read-only callers without a builder) implement this so generated
/// code can ask "what address is `MyPackage` at right now?".
pub trait PackageAddrs {
    /// Return the on-chain address registered for `P`. Panics if
    /// the caller never registered `P`.
    fn package_id<P: 'static>(&self) -> Address;
}

/// Free-standing package address store for non-PTB callers (event
/// decoders, object readers, BCS deserialization, etc.).
///
/// PTB call sites use `PtbBuilder::with_package` to register
/// addresses; the same machinery here lets read-only code build a
/// short-lived `PackageRegistry` and pass it to `T::type_tag`.
#[derive(Default)]
pub struct PackageRegistry {
    map: std::collections::HashMap<std::any::TypeId, Address>,
}

impl PackageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style registration. Returns `self` for chaining:
    /// `PackageRegistry::new().with::<P>(addr).with::<Q>(addr2)`.
    pub fn with<P: 'static>(mut self, addr: Address) -> Self {
        self.map.insert(std::any::TypeId::of::<P>(), addr);
        self
    }

    /// Single-package shortcut: `PackageRegistry::at::<P>(addr)`.
    pub fn at<P: 'static>(addr: Address) -> Self {
        Self::new().with::<P>(addr)
    }
}

impl PackageAddrs for PackageRegistry {
    fn package_id<P: 'static>(&self) -> Address {
        *self
            .map
            .get(&std::any::TypeId::of::<P>())
            .unwrap_or_else(|| {
                panic!(
                    "PackageRegistry: no address registered for {}",
                    std::any::type_name::<P>(),
                )
            })
    }
}

/// Maps a Rust type to its Move `TypeTag`.
///
/// Each generated datatype pins itself to a `Package` marker; codegen
/// asks the supplied `PackageAddrs` (a `PtbBuilder` or a free-standing
/// `PackageRegistry`) for the runtime address at the call site, so the
/// same generated bindings can target multiple deployments without
/// rebuilding.
pub trait MoveType {
    /// Package marker. Codegen emits a unit `pub struct Package;` in
    /// each generated crate's `lib.rs` and points all its datatypes at
    /// it. Primitives / framework types use [`NoPackage`].
    type Package: 'static;
    /// Move module name, e.g. `"counter"`. Empty for primitives.
    const MODULE: &'static str;
    /// Move datatype name, e.g. `"Counter"`. Empty for primitives.
    const NAME: &'static str;

    /// Type parameters of the datatype, computed against the supplied
    /// address map. Default = no parameters (non-generic datatypes).
    fn type_params(_addrs: &impl PackageAddrs) -> Vec<TypeTag> {
        Vec::new()
    }

    /// Build the `TypeTag` against the supplied address map. The
    /// default implementation works for any datatype that pins a
    /// `Package`; primitives override it to return their leaf tag
    /// directly.
    fn type_tag(addrs: &impl PackageAddrs) -> TypeTag {
        crate::make_struct_tag_export(
            addrs.package_id::<Self::Package>(),
            Self::MODULE,
            Self::NAME,
            Self::type_params(addrs),
        )
    }

    /// Convenience for callers who already have the package address
    /// in hand. Defaults to building a single-package registry on the
    /// fly; impls with `Package = NoPackage` (primitives, framework
    /// types) override this — there's no package to register.
    fn type_tag_at(addr: Address) -> TypeTag
    where
        Self: Sized,
    {
        let reg = PackageRegistry::at::<Self::Package>(addr);
        Self::type_tag(&reg)
    }
}

/// Helper used by the default `MoveType::type_tag` impl. Lives here so
/// the trait can refer to it without runtime-sui having to re-export
/// an extra symbol. Mirrors `runtime-sui::make_struct_tag`.
pub fn make_struct_tag_export(
    addr: Address,
    module: &str,
    name: &str,
    params: Vec<TypeTag>,
) -> TypeTag {
    TypeTag::Struct(Box::new(sui_sdk_types::StructTag::new(
        addr,
        sui_sdk_types::Identifier::new(module)
            .expect("static module name is a valid Move identifier"),
        sui_sdk_types::Identifier::new(name)
            .expect("static datatype name is a valid Move identifier"),
        params,
    )))
}

macro_rules! impl_move_type_primitive {
    ($($ty:ty => $tag:ident),* $(,)?) => {
        $(
            impl MoveType for $ty {
                type Package = NoPackage;
                const MODULE: &'static str = "";
                const NAME: &'static str = "";
                fn type_tag(_: &impl PackageAddrs) -> TypeTag { TypeTag::$tag }
                fn type_tag_at(_: Address) -> TypeTag { TypeTag::$tag }
            }
        )*
    };
}

impl_move_type_primitive! {
    bool => Bool,
    u8 => U8,
    u16 => U16,
    u32 => U32,
    u64 => U64,
    u128 => U128,
}

impl MoveType for U256 {
    type Package = NoPackage;
    const MODULE: &'static str = "";
    const NAME: &'static str = "";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        TypeTag::U256
    }
    fn type_tag_at(_: Address) -> TypeTag {
        TypeTag::U256
    }
}

impl MoveType for Address {
    type Package = NoPackage;
    const MODULE: &'static str = "";
    const NAME: &'static str = "";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        TypeTag::Address
    }
    fn type_tag_at(_: Address) -> TypeTag {
        TypeTag::Address
    }
}

impl MoveType for String {
    // Move's `0x1::string::String` is `vector<u8>` on the wire; the SDK's
    // intent for String→type_tag is the same.
    type Package = NoPackage;
    const MODULE: &'static str = "";
    const NAME: &'static str = "";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        TypeTag::Vector(Box::new(TypeTag::U8))
    }
    fn type_tag_at(_: Address) -> TypeTag {
        TypeTag::Vector(Box::new(TypeTag::U8))
    }
}

impl<T: MoveType> MoveType for Vec<T> {
    type Package = NoPackage;
    const MODULE: &'static str = "";
    const NAME: &'static str = "";
    fn type_tag(addrs: &impl PackageAddrs) -> TypeTag {
        TypeTag::Vector(Box::new(T::type_tag(addrs)))
    }
}

// -----------------------------------------------------------------------------
// MoveArg: BCS-encode a value into pure-input bytes.
// -----------------------------------------------------------------------------

/// Pure (BCS-serialised) bytes destined for a PTB `Input::Pure` slot.
#[derive(Clone, Debug, Default)]
pub struct PureBytes(pub Vec<u8>);

/// Convert a value to its BCS-pure-bytes representation. Codegen emits
/// `MoveArg` for every non-`key` datatype; the blanket `PTBArgument`
/// impl below routes `MoveArg` values through `Pure` inputs.
pub trait MoveArg: Sized {
    fn pure_bytes(self) -> PureBytes;
}

impl MoveArg for PureBytes {
    fn pure_bytes(self) -> PureBytes {
        self
    }
}

/// Convenience helper for tests/call-site code that wants to BCS-encode
/// an arbitrary `Serialize` value into `PureBytes`.
pub fn pure_bytes_of<T: Serialize>(v: &T) -> PureBytes {
    PureBytes(bcs::to_bytes(v).expect("BCS serialization should not fail"))
}

// `MoveArg` impls for the Move primitive types. Each delegates to
// `bcs::to_bytes` — Move's wire format is BCS, and serde+BCS agree
// for these types. Generated codegen-emitted `MoveArg` impls for user
// datatypes follow the same pattern.
//
// `U256` is a special case: `primitive_types::U256`'s default
// `impl-serde` encoding is a hex string, which doesn't match Move's
// 32-LE-bytes wire format. Push the bytes directly.
macro_rules! impl_move_arg_via_bcs {
    ($($ty:ty),* $(,)?) => {
        $(
            impl MoveArg for $ty {
                fn pure_bytes(self) -> PureBytes {
                    PureBytes(bcs::to_bytes(&self).expect("BCS serialization should not fail"))
                }
            }
        )*
    };
}

impl_move_arg_via_bcs!(bool, u8, u16, u32, u64, u128, Address, String);

impl MoveArg for U256 {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(self.to_little_endian().to_vec())
    }
}

impl<T: MoveArg + Serialize> MoveArg for Vec<T> {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("BCS serialization should not fail"))
    }
}

impl<T: MoveArg + Serialize> MoveArg for Option<T> {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("BCS serialization should not fail"))
    }
}

// -----------------------------------------------------------------------------
// Object kinds: Shared / SharedMut / Receiving — typed wrappers around
// ObjectId / ObjectReference.
// -----------------------------------------------------------------------------

/// Wrap an object id/ref to mark it as a *shared, read-only* input.
pub struct Shared<T>(pub T);

/// Wrap an object id/ref to mark it as a *shared, mutable* input.
pub struct SharedMut<T>(pub T);

/// Wrap an object id/ref to mark it as a `Receiving<T>` input
/// (transfer-to-object pattern).
pub struct Receiving<T>(pub T);

// -----------------------------------------------------------------------------
// PTBArgument + InputKind.
// -----------------------------------------------------------------------------

/// What a `PTBArgument`-implementing value resolves to when the
/// builder needs to materialise it. Closely mirrors the variants of
/// Sui's `Input` enum we actually use, plus an "already-an-Argument"
/// pass-through case.
#[derive(Clone, Debug)]
pub enum InputKind {
    /// Already-resolved builder argument — no input materialisation
    /// needed, just reuse the handle.
    Argument(Argument),
    /// `Pure` BCS bytes (BCS-serialised primitive or value type).
    Pure(Vec<u8>),
    /// Owned or immutable object — needs a full `ObjectReference`.
    ImmutableOrOwned(ObjectReference),
    /// Shared object input. `initial_shared_version` is filled by the
    /// applier (cache lookup + fetcher fallback); it isn't known at
    /// the call site when only an `ObjectId` is supplied.
    Shared { object_id: ObjectId, mutable: bool },
    /// Shared object input where the caller already knows the initial
    /// version (e.g. a `Shared<ObjectReference>`).
    SharedRef {
        object_ref: ObjectReference,
        mutable: bool,
    },
    /// `Receiving<T>` input — uses an `ObjectReference`.
    Receiving(ObjectReference),
}

/// A value that can serve as a single PTB argument. Generated per-type
/// `ArgumentX` traits extend this; every concrete `T` for which we
/// have a corresponding impl can be passed to a generated call.
pub trait PTBArgument: Sized {
    /// What this value resolves to. The builder turns it into a real
    /// `Argument` handle.
    fn input(self) -> InputKind;
}

impl PTBArgument for Argument {
    fn input(self) -> InputKind {
        InputKind::Argument(self)
    }
}

// Blanket impl: any `MoveArg` value flows through as a `Pure` input.
// This covers all generated-code datatypes (which `impl MoveArg for X`)
// plus the primitive `Pure*` marker traits added in runtime-sui.
impl<T: MoveArg> PTBArgument for T {
    fn input(self) -> InputKind {
        InputKind::Pure(self.pure_bytes().0)
    }
}

// `ObjectId = Address`, and `Address` already gets `PTBArgument` via
// the blanket above (`MoveArg for Address`). Generated code's
// `impl ArgumentX for ObjectId { async fn into_argument(...) ->
// b.resolve_object(self).await }` overrides the default body anyway,
// so the blanket's `Pure`-flavoured `input()` is never reached for
// `ObjectId`-typed values that come through a per-type trait.

impl PTBArgument for ObjectReference {
    fn input(self) -> InputKind {
        InputKind::ImmutableOrOwned(self)
    }
}

impl PTBArgument for Shared<ObjectId> {
    fn input(self) -> InputKind {
        InputKind::Shared {
            object_id: self.0,
            mutable: false,
        }
    }
}

impl PTBArgument for Shared<ObjectReference> {
    fn input(self) -> InputKind {
        InputKind::SharedRef {
            object_ref: self.0,
            mutable: false,
        }
    }
}

impl PTBArgument for SharedMut<ObjectId> {
    fn input(self) -> InputKind {
        InputKind::Shared {
            object_id: self.0,
            mutable: true,
        }
    }
}

impl PTBArgument for SharedMut<ObjectReference> {
    fn input(self) -> InputKind {
        InputKind::SharedRef {
            object_ref: self.0,
            mutable: true,
        }
    }
}

impl PTBArgument for Receiving<ObjectId> {
    fn input(self) -> InputKind {
        InputKind::Shared {
            object_id: self.0,
            mutable: false,
        }
    }
}

impl PTBArgument for Receiving<ObjectReference> {
    fn input(self) -> InputKind {
        InputKind::Receiving(self.0)
    }
}

// -----------------------------------------------------------------------------
// `U256` ↔ Move's 32-LE-bytes wire format. Generated `Serialize` /
// `Deserialize` impls attach `#[serde(with = "u256_le")]` on every
// `U256` field so structs round-trip cleanly through BCS. Without
// this, `primitive_types`'s default `impl-serde` would hex-string
// encode the value and break the wire format.
// -----------------------------------------------------------------------------

pub mod u256_le {
    use super::U256;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &U256, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = value.to_little_endian();
        serializer.serialize_bytes(&bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<U256, D::Error> {
        let bytes: &[u8] = <&[u8]>::deserialize(deserializer)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "U256 expects 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; 32];
        buf.copy_from_slice(bytes);
        Ok(U256::from_little_endian(&buf))
    }
}

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

/// Resolves an unknown [`ObjectId`] to a [`FetchedObject`]. Consulted
/// by `PtbBuilder` on cache miss.
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

/// Submits a signed transaction.
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

/// Backend capability `PtbBuilder` uses to fill gas slots automatically
/// when `with_auto_gas` is enabled.
pub trait GasOracle: Send + Sync {
    /// Owned gas coin object refs available to `owner`.
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a>;
    /// Network's current reference gas price.
    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a>;
    /// Conservative fallback budget when dry-run isn't viable.
    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a>;
    /// Dry-run `tx` and return the gas it actually used. The default
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
/// `EffectsExt` to resolve "all created/mutated/changed objects of
/// type T" after a transaction.
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

/// Backend capability for reading events emitted by a specific
/// transaction filtered by Move type. Returns the BCS payload of each
/// matching event; callers BCS-decode into the typed Rust struct.
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

#[derive(Debug, thiserror::Error)]
pub enum DryRunError {
    #[error("dry-run backend: {0}")]
    Backend(String),
    #[error("dry-run aborted: {0}")]
    Aborted(String),
}

pub type DryRunFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InspectResult, DryRunError>> + Send + 'a>>;

/// Backend capability for dry-running a transaction and returning its
/// full effects + per-slot return values.
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
