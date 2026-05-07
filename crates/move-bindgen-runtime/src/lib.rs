//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Re-exports the small set of SDK types generated code needs (so the user's
//! crate doesn't have to depend on `iota-sdk-types` directly), defines the
//! well-known framework types `UID` / `ID`, and provides:
//!
//! - [`PtbBuilder`] — a wrapper around `iota-sdk-transaction-builder`'s
//!   `TransactionBuilder` with caches that turn bare `ObjectId`s into the
//!   correct `Input` variant at use time.
//! - Per-Move-primitive marker traits (`PureBool`, `PureU64`, …) supertyped
//!   by `PTBArgument`. Generated call builders bound their parameters on
//!   these so passing the wrong type fails at compile time.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub use iota_sdk_transaction_builder::{
    PTBArgument, PureBytes, Receiving, Shared, SharedMut, TransactionBuilder,
    types::{MoveArg, MoveType},
    // `Argument`, `Command`, `MoveCall` here are the "unresolved" variants the
    // builder composes during PTB construction. They get resolved to the
    // `iota_sdk_types::*` counterparts when `TransactionBuilder::finish()` is
    // called.
    unresolved::{Argument, Command, MoveCall},
};
pub use iota_sdk_types::{
    Address, Identifier, Input, ObjectId, ObjectReference, SharedObjectReference, StructTag,
    Transaction, TypeTag, Version,
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
///
/// Panics if `module` or `name` aren't valid Move identifiers — fine because
/// every caller is generated code baking in identifiers extracted from
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
// PtbBuilder
// -----------------------------------------------------------------------------

/// Cached info for a known shared object.
#[derive(Copy, Clone, Debug)]
pub struct SharedObjectInfo {
    pub initial_shared_version: u64,
    pub mutable: bool,
}

/// Wrapper around the SDK's `TransactionBuilder` that remembers per-`ObjectId`
/// ownership info. Once `register_owned` / `register_shared` populate the
/// cache, generated call builders can accept a bare `ObjectId` and the cache
/// picks the right `Input` variant.
pub struct PtbBuilder {
    /// Underlying SDK builder. Public because some advanced flows need to
    /// reach in (for example, calling SDK convenience methods like
    /// `transfer_objects` directly).
    pub inner: TransactionBuilder,
    owned: HashMap<ObjectId, ObjectReference>,
    shared: HashMap<ObjectId, SharedObjectInfo>,
}

impl PtbBuilder {
    pub fn new(sender: Address) -> Self {
        Self {
            inner: TransactionBuilder::new(sender),
            owned: HashMap::new(),
            shared: HashMap::new(),
        }
    }

    /// Tell the cache that `id` is an owned (or immutable-by-id) object with
    /// the given ref. Used to wrap subsequent passes of `id` as
    /// `Input::ImmutableOrOwned`.
    pub fn register_owned(&mut self, id: ObjectId, r: ObjectReference) {
        self.owned.insert(id, r);
    }

    /// Tell the cache that `id` is a shared object with the given initial
    /// shared version and default mutability. Used to wrap subsequent passes
    /// of `id` as `Input::Shared`.
    pub fn register_shared(&mut self, id: ObjectId, initial_shared_version: u64, mutable: bool) {
        self.shared.insert(
            id,
            SharedObjectInfo {
                initial_shared_version,
                mutable,
            },
        );
    }

    /// Cache-aware: picks `ImmutableOrOwned` (cached owned) or `Shared`
    /// (cached shared); falls back to delegating to the SDK's default
    /// `PTBArgument` impl for `ObjectId` (which yields a version-less
    /// `ImmutableOrOwned` requiring a client to resolve at `finish()` time).
    pub fn resolve_object(&mut self, id: ObjectId) -> Argument {
        if let Some(r) = self.owned.get(&id).cloned() {
            return self.inner.input(Input::ImmutableOrOwned(r));
        }
        if let Some(info) = self.shared.get(&id).copied() {
            return self.inner.input(Input::Shared(SharedObjectReference {
                object_id: id,
                initial_shared_version: Version::from_u64(info.initial_shared_version),
                mutable: info.mutable,
            }));
        }
        // Unknown — let the SDK try (will fail at finish() without a client).
        self.inner.apply_argument(id)
    }

    /// Build a `MoveCall` command from already-resolved args. Returns the
    /// `Argument::Result` for the call's output.
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
            function: Identifier::new(function)
                .expect("static fn name is a valid Move identifier"),
            type_arguments,
            arguments,
        });
        self.inner.command(cmd)
    }

    /// Add a BCS-encoded value as a `Pure` input.
    pub fn pure<T: serde::Serialize>(&mut self, value: T) -> Argument {
        self.inner.pure(value)
    }
}

// -----------------------------------------------------------------------------
// Per-Move-primitive marker traits
// -----------------------------------------------------------------------------
//
// Each trait is a closed-impl set bounded by `PTBArgument`. The single method
// `into_argument(self, b)` is what generated call builders invoke. The default
// body delegates to `PTBArgument::arg` via the SDK's `apply_argument`.
//
// Cache-aware override happens *only* on `ArgumentCounter for ObjectId` (and
// the analogous codegen'd traits) — primitives have no cache need.

macro_rules! decl_pure_trait {
    ($trait_name:ident, $ty:ty) => {
        pub trait $trait_name: PTBArgument {
            fn into_argument(self, b: &mut PtbBuilder) -> Argument
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
    fn into_argument(self, b: &mut PtbBuilder) -> Argument
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
    fn into_argument(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        b.inner.apply_argument(self)
    }
}
impl<T: MoveArg> PureOption<T> for Option<T> {}
impl<T> PureOption<T> for Argument {}
