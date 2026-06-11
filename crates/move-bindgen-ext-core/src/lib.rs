//! Flavour-agnostic core for the `move-bindgen-ext-*` crates.
//!
//! Provides:
//!
//! - Plain types and helpers that don't reference any chain SDK:
//!   [`WaitOptions`], [`NoPackage`], the `Backend`-only error enums
//!   ([`SubmitError`], [`FindError`], [`EventReaderError`],
//!   [`DryRunError`]), and the [`u256_le`] serde helper.
//! - The [`decl_pure_trait!`] macro for declaring per-primitive
//!   `Pure*` marker traits. Resolves `PTBArgument` / `PtbBuilder` /
//!   `Argument` at the call site, so each invocation must have them
//!   in scope.
//! - The [`define_backend_traits!`] macro. Each flavour invokes it
//!   once with its SDK types and gets the chain-typed trait family
//!   (`PackageAddrs`, `MoveType`, `Fetcher`, `Submitter`,
//!   `GasOracle`, `ObjectTypeFinder`, `EventReader`, `DryRunner`)
//!   declared inside its own namespace.

use std::time::Duration;

// -----------------------------------------------------------------------------
// Plain items: shared verbatim across flavours.
// -----------------------------------------------------------------------------

/// Marker type used by `MoveType` impls that don't belong to a
/// generated package (primitives, framework types, etc.). Looking up
/// a `NoPackage` in `PackageAddrs` is a programmer error — those
/// impls override `type_tag` and never call the trait's default
/// body.
pub struct NoPackage;

/// Rust mirror of Move's `0x1::ascii::String`. Wire-compatible with
/// `0x1::string::String` and Rust `String` (BCS in all three cases is
/// length-prefixed `Vec<u8>`), but a distinct Rust type so generated
/// code can route it to the correct `TypeTag` at generic-instantiation
/// positions — i.e. a `move_call::<AsciiString>` produces
/// `TypeTag::Struct(0x1::ascii::String)`, not `Vector(U8)`.
///
/// Codegen maps `0x1::ascii::String` to this; `0x1::string::String`
/// keeps mapping to Rust's `std::string::String`.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
#[serde(transparent)]
pub struct AsciiString(pub String);

impl From<String> for AsciiString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AsciiString {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<AsciiString> for String {
    fn from(a: AsciiString) -> Self {
        a.0
    }
}

impl ::std::ops::Deref for AsciiString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl ::std::fmt::Display for AsciiString {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        ::std::fmt::Display::fmt(&self.0, f)
    }
}

/// Tunables for `ClientExt::wait_for_effects` and
/// `wait_for_object`. `interval` is the indexer-poll cadence;
/// `timeout` is how long the call waits before erroring.
pub struct WaitOptions {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(250),
            timeout: Duration::from_secs(15),
        }
    }
}

// ---- Backend-only error enums (no SDK type variants) ------------------------

#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error("submit backend: {0}")]
    Backend(String),
}

#[derive(Debug, thiserror::Error)]
pub enum FindError {
    #[error("type-finder backend: {0}")]
    Backend(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EventReaderError {
    #[error("event-reader backend: {0}")]
    Backend(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DryRunError {
    #[error("dry-run backend: {0}")]
    Backend(String),
    #[error("dry-run aborted: {0}")]
    Aborted(String),
}

// ---- decl_pure_trait! macro -------------------------------------------------

/// Declare a `Pure<Primitive>` marker trait pairing one Rust type
/// (the primitive) with `Argument`. Generated call builders take
/// `impl PureU64`, `impl PureBool`, etc. as bounds for value
/// parameters; this macro emits the trait and its closed impl set.
///
/// `$trait_name` is the trait identifier, `$ty` is the Rust shape
/// the trait accepts. Extra accepted shapes can be tacked on:
/// `decl_pure_trait!(PureString, String, &str);` makes `&str` an
/// accepted form too.
///
/// The macro body references `PTBArgument`, `PtbBuilder`, and
/// `Argument` by bare name — they're resolved at the call site, so
/// each invocation must have those three in scope.
#[macro_export]
macro_rules! decl_pure_trait {
    ($trait_name:ident, $ty:ty $(, $extra:ty)* $(,)?) => {
        pub trait $trait_name: PTBArgument {
            #[allow(async_fn_in_trait)]
            async fn into_argument(self, b: &mut PtbBuilder) -> Argument
            where Self: Sized,
            {
                b.inner.apply_argument(self)
            }
            // Pure (BCS) inputs have no on-chain mutability distinction;
            // codegen still calls `_ref` / `_mut` for Move `&T` /
            // `&mut T` parameters of pure shape, so the trait must
            // expose them. Both default to `into_argument`.
            #[allow(async_fn_in_trait)]
            async fn into_argument_ref(self, b: &mut PtbBuilder) -> Argument
            where Self: Sized,
            {
                self.into_argument(b).await
            }
            #[allow(async_fn_in_trait)]
            async fn into_argument_mut(self, b: &mut PtbBuilder) -> Argument
            where Self: Sized,
            {
                self.into_argument(b).await
            }
        }
        impl $trait_name for $ty {}
        impl $trait_name for Argument {}
        $( impl $trait_name for $extra {} )*
    };
}

// ---- u256_le serde helper ---------------------------------------------------

/// `#[serde(with = "move_bindgen_ext_core::u256_le")]` field
/// attribute: BCS-serialise a `primitive_types::U256` as 32
/// little-endian bytes (no length prefix). The crate's default
/// `impl-serde` feature uses a hex string instead, which doesn't
/// match Move's wire format — generated code annotates every U256
/// field with this serde override.
pub mod u256_le {
    use primitive_types::U256;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
        let bytes = v.to_little_endian();
        bytes.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        let bytes = <[u8; 32]>::deserialize(d)?;
        Ok(U256::from_little_endian(&bytes))
    }
}

// -----------------------------------------------------------------------------
// SDK-typed trait family — emitted by `define_backend_traits!`.
//
// Invoke once per flavour with the SDK types as macro arguments:
//
// ```ignore
// move_bindgen_ext_core::define_backend_traits! {
//     address              = my_sdk::Address,
//     type_tag             = my_sdk::TypeTag,
//     object_id            = my_sdk::ObjectId,
//     object_reference     = my_sdk::ObjectReference,
//     transaction          = my_sdk::Transaction,
//     transaction_effects  = my_sdk::TransactionEffects,
//     user_signature       = my_sdk::UserSignature,
//     digest               = my_sdk::Digest,
//     struct_tag           = my_sdk::StructTag,
//     identifier           = my_sdk::Identifier,
// }
// ```
//
// The macro emits, in the call site's namespace:
//
//   PackageAddrs trait + PackageRegistry struct + impls
//   MoveType trait + primitive impls (bool, u8..u128, U256, Address, Vec<T>)
//   (NB: `String` / `AsciiString` MoveType impls are added by each flavour's
//   ext crate, since they need the flavour-specific `0x1` constructor.)
//   make_struct_tag_export helper
//   FetchedObject enum + FetchError + Fetcher trait + Arc/Box blankets + FetchFuture<'a>
//   Submitter trait + SubmitFuture<'a>
//   OracleError + ListGasCoinsFuture + RefGasPriceFuture + SuggestBudgetFuture
//   GasOracle trait + Arc/Box blankets
//   ObjectTypeFinder trait + FindByTypeFuture + Arc/Box blankets
//   EventReader trait + EventsByTxFuture + Arc/Box blankets
//   InspectResult struct + DecodeError + DryRunEstimateFuture + DryRunFuture
//   DryRunner trait + Arc/Box blankets
// -----------------------------------------------------------------------------

#[macro_export]
macro_rules! define_backend_traits {
    (
        address              = $Address:ty,
        type_tag             = $TypeTag:ty,
        object_id            = $ObjectId:ty,
        object_reference     = $ObjectReference:ty,
        transaction          = $Transaction:ty,
        transaction_effects  = $TransactionEffects:ty,
        user_signature       = $UserSignature:ty,
        digest               = $Digest:ty,
        struct_tag           = $StructTag:ty,
        identifier           = $Identifier:ty,
        $(,)?
    ) => {
        // ---- PackageAddrs + PackageRegistry -------------------------------

        /// Runtime map from a generated `Package` marker to its
        /// on-chain address. `PtbBuilder` and `PackageRegistry` both
        /// implement this; generated code reads through it at
        /// `move_call*` / `type_tag` callsites.
        pub trait PackageAddrs {
            fn package_id<P: 'static>(&self) -> $Address;
        }

        /// Free-standing package address store for non-PTB callers
        /// (event decoders, BCS deserialization, object readers).
        /// Build one with `PackageRegistry::at::<Package>(addr)` for
        /// the common single-package case, or chain `.with::<P>(addr)`
        /// for multi-package registries.
        #[derive(Default)]
        pub struct PackageRegistry {
            map: ::std::collections::HashMap<::std::any::TypeId, $Address>,
        }

        impl PackageRegistry {
            pub fn new() -> Self {
                Self::default()
            }
            pub fn with<P: 'static>(mut self, addr: $Address) -> Self {
                self.map.insert(::std::any::TypeId::of::<P>(), addr);
                self
            }
            pub fn at<P: 'static>(addr: $Address) -> Self {
                Self::new().with::<P>(addr)
            }
        }

        impl PackageAddrs for PackageRegistry {
            fn package_id<P: 'static>(&self) -> $Address {
                *self
                    .map
                    .get(&::std::any::TypeId::of::<P>())
                    .unwrap_or_else(|| {
                        panic!(
                            "PackageRegistry: no address registered for {}",
                            ::std::any::type_name::<P>(),
                        )
                    })
            }
        }

        // ---- MoveType + make_struct_tag_export + primitive impls ----------

        /// Maps a Rust type to its Move `TypeTag`. Generated
        /// datatypes pin themselves to a `Package` marker; codegen
        /// looks up the runtime address through `PackageAddrs` at
        /// the call site so the same bindings work across
        /// deployments without rebuilding.
        pub trait MoveType {
            type Package: 'static;
            const MODULE: &'static str;
            const NAME: &'static str;
            fn type_params(_addrs: &impl PackageAddrs) -> Vec<$TypeTag> {
                Vec::new()
            }
            fn type_tag(addrs: &impl PackageAddrs) -> $TypeTag {
                make_struct_tag_export(
                    addrs.package_id::<Self::Package>(),
                    Self::MODULE,
                    Self::NAME,
                    Self::type_params(addrs),
                )
            }
            fn type_tag_at(addr: $Address) -> $TypeTag
            where
                Self: Sized,
            {
                let reg = PackageRegistry::at::<Self::Package>(addr);
                Self::type_tag(&reg)
            }
        }

        /// Build a `TypeTag::Struct` from string module/name + concrete
        /// type params. Panics if `module`/`name` aren't valid Move
        /// identifiers — only generated code calls this, with names
        /// extracted from compiled bytecode.
        pub fn make_struct_tag_export(
            addr: $Address,
            module: &str,
            name: &str,
            params: Vec<$TypeTag>,
        ) -> $TypeTag {
            <$TypeTag>::Struct(Box::new(<$StructTag>::new(
                addr,
                <$Identifier>::new(module).expect("static module name is a valid Move identifier"),
                <$Identifier>::new(name).expect("static datatype name is a valid Move identifier"),
                params,
            )))
        }

        impl MoveType for bool {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::Bool
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::Bool
            }
        }
        impl MoveType for u8 {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::U8
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::U8
            }
        }
        impl MoveType for u16 {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::U16
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::U16
            }
        }
        impl MoveType for u32 {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::U32
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::U32
            }
        }
        impl MoveType for u64 {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::U64
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::U64
            }
        }
        impl MoveType for u128 {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::U128
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::U128
            }
        }

        impl MoveType for ::primitive_types::U256 {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::U256
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::U256
            }
        }

        impl MoveType for $Address {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(_: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::Address
            }
            fn type_tag_at(_: $Address) -> $TypeTag {
                <$TypeTag>::Address
            }
        }

        // NB: `impl MoveType for String` and `impl MoveType for
        // AsciiString` are **not** emitted here — they live in each
        // flavour's ext crate (`move-bindgen-ext-iota` /
        // `-sui`) so the `0x1` move_stdlib address can be constructed
        // with the flavour's own `Address` constructor.

        impl<T: MoveType> MoveType for Vec<T> {
            type Package = $crate::NoPackage;
            const MODULE: &'static str = "";
            const NAME: &'static str = "";
            fn type_tag(addrs: &impl PackageAddrs) -> $TypeTag {
                <$TypeTag>::Vector(Box::new(T::type_tag(addrs)))
            }
        }

        // ---- Fetcher ------------------------------------------------------

        /// Result of a successful object lookup by a `Fetcher`.
        #[derive(Clone, Debug)]
        pub enum FetchedObject {
            Owned($ObjectReference),
            Shared {
                initial_shared_version: u64,
                mutable: bool,
            },
        }

        #[derive(Debug, ::thiserror::Error)]
        pub enum FetchError {
            #[error("object {0} not found")]
            NotFound($ObjectId),
            #[error("fetcher backend: {0}")]
            Backend(String),
        }

        pub type FetchFuture<'a> = ::std::pin::Pin<
            Box<dyn ::std::future::Future<Output = Result<FetchedObject, FetchError>> + Send + 'a>,
        >;

        pub trait Fetcher: Send + Sync {
            fn fetch<'a>(&'a self, id: $ObjectId) -> FetchFuture<'a>;
        }

        impl<F: Fetcher + ?Sized> Fetcher for ::std::sync::Arc<F> {
            fn fetch<'a>(&'a self, id: $ObjectId) -> FetchFuture<'a> {
                F::fetch(self, id)
            }
        }
        impl<F: Fetcher + ?Sized> Fetcher for Box<F> {
            fn fetch<'a>(&'a self, id: $ObjectId) -> FetchFuture<'a> {
                F::fetch(self, id)
            }
        }

        // ---- Submitter ----------------------------------------------------

        pub type SubmitFuture<'a> = ::std::pin::Pin<
            Box<
                dyn ::std::future::Future<Output = Result<$TransactionEffects, $crate::SubmitError>>
                    + Send
                    + 'a,
            >,
        >;

        /// Submits a signed transaction.
        pub trait Submitter: Send + Sync {
            fn submit<'a>(
                &'a self,
                tx: &'a $Transaction,
                signatures: &'a [$UserSignature],
            ) -> SubmitFuture<'a>;
        }

        impl<S: Submitter + ?Sized> Submitter for ::std::sync::Arc<S> {
            fn submit<'a>(
                &'a self,
                tx: &'a $Transaction,
                signatures: &'a [$UserSignature],
            ) -> SubmitFuture<'a> {
                S::submit(self, tx, signatures)
            }
        }
        impl<S: Submitter + ?Sized> Submitter for Box<S> {
            fn submit<'a>(
                &'a self,
                tx: &'a $Transaction,
                signatures: &'a [$UserSignature],
            ) -> SubmitFuture<'a> {
                S::submit(self, tx, signatures)
            }
        }

        // ---- GasOracle ----------------------------------------------------

        #[derive(Debug, ::thiserror::Error)]
        pub enum OracleError {
            #[error("oracle backend: {0}")]
            Backend(String),
            #[error("no gas coins available for {0}")]
            NoGasCoins($Address),
            #[error("oracle does not support `{0}`")]
            Unsupported(&'static str),
        }

        pub type ListGasCoinsFuture<'a> = ::std::pin::Pin<
            Box<
                dyn ::std::future::Future<Output = Result<Vec<$ObjectReference>, OracleError>>
                    + Send
                    + 'a,
            >,
        >;
        pub type RefGasPriceFuture<'a> = ::std::pin::Pin<
            Box<dyn ::std::future::Future<Output = Result<u64, OracleError>> + Send + 'a>,
        >;
        pub type SuggestBudgetFuture<'a> = ::std::pin::Pin<
            Box<dyn ::std::future::Future<Output = Result<u64, OracleError>> + Send + 'a>,
        >;
        pub type DryRunEstimateFuture<'a> = ::std::pin::Pin<
            Box<dyn ::std::future::Future<Output = Result<u64, OracleError>> + Send + 'a>,
        >;

        /// Backend capability `PtbBuilder` uses to fill gas slots
        /// automatically when `with_auto_gas` is enabled.
        pub trait GasOracle: Send + Sync {
            /// Owned gas coin object refs available to `owner`.
            fn list_gas_coins<'a>(&'a self, owner: $Address) -> ListGasCoinsFuture<'a>;
            /// Network's current reference gas price.
            fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a>;
            /// Conservative fallback budget when dry-run isn't viable.
            /// Implementations usually return a generous constant.
            fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a>;
            /// Dry-run `tx` and return the gas it actually used.
            /// `PtbBuilder` adds a safety margin and uses this when
            /// `with_auto_gas` is on. Default returns
            /// `Err(OracleError::Unsupported)` so backends can opt in.
            fn dry_run_estimate<'a>(&'a self, _tx: &'a $Transaction) -> DryRunEstimateFuture<'a> {
                Box::pin(async { Err(OracleError::Unsupported("dry_run_estimate")) })
            }
        }

        impl<O: GasOracle + ?Sized> GasOracle for ::std::sync::Arc<O> {
            fn list_gas_coins<'a>(&'a self, owner: $Address) -> ListGasCoinsFuture<'a> {
                O::list_gas_coins(self, owner)
            }
            fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
                O::reference_gas_price(self)
            }
            fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
                O::suggest_gas_budget(self)
            }
            fn dry_run_estimate<'a>(&'a self, tx: &'a $Transaction) -> DryRunEstimateFuture<'a> {
                O::dry_run_estimate(self, tx)
            }
        }
        impl<O: GasOracle + ?Sized> GasOracle for Box<O> {
            fn list_gas_coins<'a>(&'a self, owner: $Address) -> ListGasCoinsFuture<'a> {
                O::list_gas_coins(self, owner)
            }
            fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
                O::reference_gas_price(self)
            }
            fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
                O::suggest_gas_budget(self)
            }
            fn dry_run_estimate<'a>(&'a self, tx: &'a $Transaction) -> DryRunEstimateFuture<'a> {
                O::dry_run_estimate(self, tx)
            }
        }

        // ---- ObjectTypeFinder ---------------------------------------------

        pub type FindByTypeFuture<'a> = ::std::pin::Pin<
            Box<
                dyn ::std::future::Future<Output = Result<Vec<$ObjectReference>, $crate::FindError>>
                    + Send
                    + 'a,
            >,
        >;

        /// Backend capability for batched type-filtered object lookup.
        /// Used by `EffectsExt` to resolve "all created/mutated/changed
        /// objects of type T" after a transaction.
        pub trait ObjectTypeFinder: Send + Sync {
            fn find_by_type<'a>(
                &'a self,
                type_tag: $TypeTag,
                ids: Vec<$ObjectId>,
            ) -> FindByTypeFuture<'a>;
        }

        impl<O: ObjectTypeFinder + ?Sized> ObjectTypeFinder for ::std::sync::Arc<O> {
            fn find_by_type<'a>(
                &'a self,
                type_tag: $TypeTag,
                ids: Vec<$ObjectId>,
            ) -> FindByTypeFuture<'a> {
                O::find_by_type(self, type_tag, ids)
            }
        }
        impl<O: ObjectTypeFinder + ?Sized> ObjectTypeFinder for Box<O> {
            fn find_by_type<'a>(
                &'a self,
                type_tag: $TypeTag,
                ids: Vec<$ObjectId>,
            ) -> FindByTypeFuture<'a> {
                O::find_by_type(self, type_tag, ids)
            }
        }

        // ---- EventReader --------------------------------------------------

        pub type EventsByTxFuture<'a> = ::std::pin::Pin<
            Box<
                dyn ::std::future::Future<Output = Result<Vec<Vec<u8>>, $crate::EventReaderError>>
                    + Send
                    + 'a,
            >,
        >;

        /// Backend capability for reading events emitted by a specific
        /// transaction filtered by Move type. Returns the BCS payload of
        /// each matching event; callers BCS-decode into the typed Rust
        /// struct.
        pub trait EventReader: Send + Sync {
            fn events_by_tx<'a>(
                &'a self,
                digest: $Digest,
                type_tag: $TypeTag,
            ) -> EventsByTxFuture<'a>;
        }

        impl<R: EventReader + ?Sized> EventReader for ::std::sync::Arc<R> {
            fn events_by_tx<'a>(
                &'a self,
                digest: $Digest,
                type_tag: $TypeTag,
            ) -> EventsByTxFuture<'a> {
                R::events_by_tx(self, digest, type_tag)
            }
        }
        impl<R: EventReader + ?Sized> EventReader for Box<R> {
            fn events_by_tx<'a>(
                &'a self,
                digest: $Digest,
                type_tag: $TypeTag,
            ) -> EventsByTxFuture<'a> {
                R::events_by_tx(self, digest, type_tag)
            }
        }

        // ---- DryRunner + InspectResult + DecodeError ----------------------

        /// Per-command per-slot BCS return-value bytes plus the dry-run
        /// effects. Returned by `PtbBuilder::inspect`.
        #[derive(Debug, Clone)]
        pub struct InspectResult {
            pub effects: $TransactionEffects,
            /// Indexed `[command_idx][return_slot]`.
            pub returns: Vec<Vec<Vec<u8>>>,
        }

        #[derive(Debug, ::thiserror::Error)]
        pub enum DecodeError {
            #[error("argument {0} is not a command result")]
            NotAReturn(String),
            #[error("no return slot for {0}")]
            NotFound(String),
            #[error("bcs decode: {0}")]
            Bcs(#[from] ::bcs::Error),
        }

        pub type DryRunFuture<'a> = ::std::pin::Pin<
            Box<
                dyn ::std::future::Future<Output = Result<InspectResult, $crate::DryRunError>>
                    + Send
                    + 'a,
            >,
        >;

        /// Backend capability for dry-running a transaction and
        /// returning its full effects + per-slot return values. Used by
        /// `PtbBuilder::inspect`.
        pub trait DryRunner: Send + Sync {
            fn dry_run<'a>(&'a self, tx: &'a $Transaction) -> DryRunFuture<'a>;
        }

        impl<D: DryRunner + ?Sized> DryRunner for ::std::sync::Arc<D> {
            fn dry_run<'a>(&'a self, tx: &'a $Transaction) -> DryRunFuture<'a> {
                D::dry_run(self, tx)
            }
        }
        impl<D: DryRunner + ?Sized> DryRunner for Box<D> {
            fn dry_run<'a>(&'a self, tx: &'a $Transaction) -> DryRunFuture<'a> {
                D::dry_run(self, tx)
            }
        }
    };
}
