//! Runtime support for Sui-flavoured generated bindings.
//!
//! Mirror of `move-bindgen-runtime-iota` for the Sui chain. Code that
//! consumes Sui-flavoured generated bindings depends on this crate via
//! the `move-bindgen-runtime` alias (`{ package =
//! "move-bindgen-runtime-sui", … }`).
//!
//! Sui's SDK is shaped differently from IOTA's — there are no
//! `MoveType` / `MoveArg` / `PTBArgument` traits and no
//! `Shared<T>` / `SharedMut<T>` / `Receiving<T>` wrappers; pure inputs
//! go through `builder.pure::<T: serde::Serialize>(&v)` directly. Our
//! codegen targets a trait-based abstraction; this crate provides it
//! and adapts to Sui's serde-based builder under the hood. Trait /
//! type primitives live in `move-bindgen-ext-sui`; this crate adds
//! the higher-level [`PtbBuilder`] and the per-Move-primitive marker
//! traits generated code uses as call-builder bounds.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sui_transaction_builder::ObjectInput;

// SDK-type re-exports. Generated code imports these via the
// `move-bindgen-runtime` alias.
pub use sui_sdk_types::{
    Address, Argument as OnChainArgument, Command, Digest, Identifier, Input, Mutability, Object,
    ObjectReference, SharedInput, StructTag, Transaction, TransactionEffects, TypeTag,
    UserSignature, Version,
};
pub use sui_transaction_builder::TransactionBuilder;

// Trait + type primitives defined in ext-sui — re-exported through
// here so generated code's `use move_bindgen_runtime::*;` keeps
// finding them.
pub use move_bindgen_ext_sui::{
    pure_bytes_of, u256_le, Argument, DecodeError, DryRunError, DryRunEstimateFuture, DryRunFuture,
    DryRunner, EventReader, EventReaderError, EventsByTxFuture, FetchError, FetchFuture,
    FetchedObject, Fetcher, FindByTypeFuture, FindError, GasOracle, InputKind, InspectResult,
    ListGasCoinsFuture, MoveArg, MoveType, NoPackage, ObjectId, ObjectTypeFinder, OracleError,
    PTBArgument, PackageAddrs, PackageRegistry, PureBytes, Receiving, RefGasPriceFuture, Shared,
    SharedMut, SubmitError, SubmitFuture, Submitter, SuggestBudgetFuture, U256,
};

// -----------------------------------------------------------------------------
// Sui framework types.
// -----------------------------------------------------------------------------

/// Sui's framework `Sui`/`object`/etc. lives at the canonical 0x2
/// address. Generated `MoveType` impls for `ID`/`UID` reference this.
const SUI_FRAMEWORK_ADDRESS: Address = Address::TWO;

/// `sui::object::ID` — a 32-byte address wrapped in a struct so Move's
/// type system can distinguish "an object's identity" from a plain
/// `address`. BCS layout matches `Address`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ID {
    pub bytes: Address,
}

/// `sui::object::UID` — owns a single `ID`. `key`-only in Move
/// (no copy/drop), so it can't be passed by value at the PTB layer
/// the way `ID` can. Generated code treats this as a non-pure type.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UID {
    pub id: ID,
}

/// Build a `TypeTag::Struct` from string module/name + concrete type
/// params. Panics if `module`/`name` aren't valid Move identifiers —
/// only generated code calls this, with identifiers from
/// already-compiled bytecode.
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
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "UID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}

// `ID` is `copy + drop + store` in Move — it can be passed as a Move
// call arg by value. Implementing `MoveArg` opts it into the blanket
// `PTBArgument for T: MoveArg` impl in ext-sui so it BCS-encodes as a
// `Pure` input. `UID` is *not* MoveArg by design: it has no `drop` in
// Move and can't be constructed off-chain.
impl MoveArg for ID {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("BCS serialization of ID never fails"))
    }
}

impl From<Address> for ID {
    fn from(bytes: Address) -> Self {
        Self { bytes }
    }
}

// -----------------------------------------------------------------------------
// ObjectCache + PtbBuilder: wraps Sui's `TransactionBuilder` and
// translates `InputKind` → builder calls. The cache lets generated
// code pass bare `ObjectId`s (without versions) and have them resolve
// into the right input variant — owned, shared, or immutable —
// without a network round-trip per call.
// -----------------------------------------------------------------------------

/// Per-object metadata the builder needs to materialise an input.
/// Populated by the user via the `register_*` methods, or fetched
/// lazily by a `Fetcher` (added once a Sui-side fetcher impl lands).
#[derive(Clone, Debug)]
pub enum CachedObject {
    /// Owned — the builder needs the full ref.
    Owned(ObjectReference),
    /// Immutable — same shape as Owned, separate variant so the
    /// builder picks the right `ObjectInput` constructor.
    Immutable(ObjectReference),
    /// Shared — the builder needs the initial shared version + the
    /// per-call mutability flag (filled in at the call site by the
    /// `Shared<T>` / `SharedMut<T>` wrapper).
    Shared { initial_shared_version: u64 },
}

/// Cache of object metadata indexed by `ObjectId`. Lookups feed
/// `apply_argument` so generated code can pass bare ids.
#[derive(Clone, Debug, Default)]
pub struct ObjectCache {
    pub entries: HashMap<ObjectId, CachedObject>,
}

impl ObjectCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_owned(&mut self, id: ObjectId, reference: ObjectReference) {
        self.entries.insert(id, CachedObject::Owned(reference));
    }

    pub fn register_immutable(&mut self, id: ObjectId, reference: ObjectReference) {
        self.entries.insert(id, CachedObject::Immutable(reference));
    }

    pub fn register_shared(&mut self, id: ObjectId, initial_shared_version: u64) {
        self.entries.insert(
            id,
            CachedObject::Shared {
                initial_shared_version,
            },
        );
    }

    pub fn lookup(&self, id: &ObjectId) -> Option<&CachedObject> {
        self.entries.get(id)
    }
}

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
    /// panic with a clear message — once a Sui `Fetcher` impl lands,
    /// the lookup falls back to a network call here.
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
                         before passing the bare ObjectId to a generated call",
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
    /// Runtime package-address registry. Generated `move_call*` /
    /// `MoveType::type_tag` callsites resolve their package's on-chain
    /// address through this — callers register addresses once per
    /// PTB with `with_package`.
    packages: std::collections::HashMap<std::any::TypeId, Address>,
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
            packages: std::collections::HashMap::new(),
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

    /// Resolve an `ObjectId` to an `Argument` by consulting the cache.
    /// Cache miss panics — callers must register the object first.
    pub async fn resolve_object(&mut self, id: ObjectId) -> Argument {
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
            }) => self
                .inner
                .tx
                .object(ObjectInput::shared(id, initial_shared_version, false)),
            None => panic!(
                "object {id:?} is not in the cache; register it via \
                 `PtbBuilder::register_shared/register_owned/register_immutable` \
                 before passing the bare ObjectId to a generated call",
            ),
        }
    }
}

// -----------------------------------------------------------------------------
// Per-Move-primitive marker traits. Each gives generated call builders
// a closed impl set so the type system rejects unrelated value types
// at the call site.
// -----------------------------------------------------------------------------

macro_rules! decl_pure_trait {
    ($trait_name:ident, $ty:ty) => {
        pub trait $trait_name: PTBArgument {
            #[allow(async_fn_in_trait)] // consumed by async generated fns; not used as `dyn`
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

/// `PureU256` lives outside `decl_pure_trait` because Move's wire
/// format is 32 LE bytes, while `primitive_types::U256`'s default
/// serde uses hex strings (`impl-serde`). Bypass the SDK route and
/// push the LE bytes directly.
pub trait PureU256 {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized;
}

impl PureU256 for U256 {
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument {
        b.inner.tx.pure_bytes(self.to_little_endian().to_vec())
    }
}

impl PureU256 for Argument {
    async fn into_argument(self, _b: &mut PtbBuilder) -> Argument {
        self
    }
}

/// Closed-impl marker for `vector<T>` parameters. Emitted by codegen
/// for any `Vec<T>` where T is itself `MoveArg`.
pub trait PureVec<T>: PTBArgument {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        b.inner.apply_argument(self)
    }
}

impl<T: MoveArg + Serialize> PureVec<T> for Vec<T> {}
impl<T> PureVec<T> for Argument {}

/// Closed-impl marker for `Option<T>` parameters.
pub trait PureOption<T>: PTBArgument {
    #[allow(async_fn_in_trait)]
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        b.inner.apply_argument(self)
    }
}

impl<T: MoveArg + Serialize> PureOption<T> for Option<T> {}
impl<T> PureOption<T> for Argument {}

/// Generic fallback marker used by codegen for generic value
/// parameters (`fun foo<T>(x: T)`) and for foreign-framework types
/// whose specific marker traits aren't available (e.g.
/// `iota::object::UID`). Same closed-impl shape as the per-package
/// `ArgumentX` traits, so generic-position args stay just as
/// type-safe.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_arg_for_u64_round_trips_through_bcs() {
        let bytes = 42u64.pure_bytes().0;
        let back: u64 = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(back, 42);
    }

    #[test]
    fn move_arg_for_u256_uses_le_bytes() {
        let bytes = U256::from(1u64).pure_bytes().0;
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[0], 1, "low-order byte first");
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn id_move_type_targets_sui_framework() {
        // `ID`'s package is `NoPackage`; the addrs map is never
        // consulted, so an empty registry is fine.
        let reg = PackageRegistry::new();
        match ID::type_tag(&reg) {
            TypeTag::Struct(s) => {
                assert_eq!(*s.address(), SUI_FRAMEWORK_ADDRESS);
                assert_eq!(s.module().as_str(), "object");
                assert_eq!(s.name().as_str(), "ID");
            }
            other => panic!("ID::type_tag must be a struct tag, got {other:?}"),
        }
    }

    #[test]
    fn cache_round_trips_registered_objects() {
        let mut cache = ObjectCache::new();
        let id = Address::TWO;
        cache.register_shared(id, 7);
        assert!(matches!(
            cache.lookup(&id),
            Some(CachedObject::Shared {
                initial_shared_version: 7
            })
        ));
    }
}
