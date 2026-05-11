//! `PtbBuilder` — wraps Sui's [`TransactionBuilder`], carries an
//! [`ObjectCache`] across calls, and exposes the `apply_argument`
//! pathway generated code uses via `b.inner.apply_argument(self)`.
//!
//! The typed surface (`PureX`, `ArgumentObject<T>`) lives in
//! [`crate::arguments`].

use std::any::TypeId;
use std::collections::HashMap;

use sui_transaction_builder::ObjectInput;

use crate::{
    cache::CachedObject, Address, Argument, Identifier, InputKind, ObjectCache, ObjectId,
    ObjectReference, PTBArgument, PackageAddrs, TransactionBuilder, TypeTag,
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
    packages: HashMap<TypeId, Address>,
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
            packages: HashMap::new(),
        }
    }

    /// Register a Move package's on-chain address against its generated
    /// `Package` marker. Generated `move_call*` and `MoveType::type_tag`
    /// callsites read this map. Call once per package per PTB.
    ///
    /// Value-based receiver so it chains with the other builder
    /// methods.
    pub fn with_package<P: 'static>(mut self, addr: Address) -> Self {
        self.packages.insert(TypeId::of::<P>(), addr);
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
