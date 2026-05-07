//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Re-exports the small set of SDK types generated code needs (so the user's
//! crate doesn't have to depend on `iota-sdk-types` directly), defines the
//! well-known framework types `UID` / `ID`, and provides:
//!
//! - [`PtbBuilder`] — a wrapper around `iota-sdk-transaction-builder`'s
//!   `TransactionBuilder` with caches that turn bare `ObjectId`s into the
//!   correct `Input` variant at use time. With a [`Fetcher`] attached,
//!   unknown ids are fetched on demand.
//! - Per-Move-primitive marker traits (`PureBool`, `PureU64`, …) supertyped
//!   by `PTBArgument`. Generated call builders bound their parameters on
//!   these so passing the wrong type fails at compile time.

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
// Fetcher
// -----------------------------------------------------------------------------

/// Cached info for a known shared object.
#[derive(Copy, Clone, Debug)]
pub struct SharedObjectInfo {
    pub initial_shared_version: u64,
    pub mutable: bool,
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

/// Boxed-future return type used by [`Fetcher`] implementations.
///
/// This is the manual desugaring of `async fn fetch(...)` and exists so the
/// trait stays object-safe (so we can store it as `Box<dyn Fetcher>`).
/// Implementers return `Box::pin(async move { … })`.
pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchedObject, FetchError>> + Send + 'a>>;

/// Forwarding impl so an `Arc<F: Fetcher>` is itself a `Fetcher`. Lets users
/// share a single fetcher between the builder and other code paths without
/// cloning state.
impl<F: Fetcher + ?Sized> Fetcher for std::sync::Arc<F> {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        F::fetch(self, id)
    }
}

/// Same forwarding for `Box<F>`, in case users want to pass a `Box<dyn Fetcher>`
/// through `with_fetcher` from a context where they already had it boxed.
impl<F: Fetcher + ?Sized> Fetcher for Box<F> {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        F::fetch(self, id)
    }
}

/// Pluggable backend the [`PtbBuilder`] consults on cache miss.
///
/// ```ignore
/// struct MyClient { /* … */ }
///
/// impl move_bindgen_runtime::Fetcher for MyClient {
///     fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
///         Box::pin(async move {
///             // …call your client…
///             Ok(FetchedObject::Shared { initial_shared_version: 1, mutable: true })
///         })
///     }
/// }
/// ```
pub trait Fetcher: Send + Sync {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a>;
}

// -----------------------------------------------------------------------------
// PtbBuilder
// -----------------------------------------------------------------------------

/// Wrapper around the SDK's `TransactionBuilder` that remembers per-`ObjectId`
/// ownership info, and (optionally) lazily fetches unknown ids via a
/// [`Fetcher`]. Generated call builders accept bare `ObjectId`s and consult
/// the cache (then the fetcher) to pick the right `Input` variant.
pub struct PtbBuilder {
    /// Underlying SDK builder. Public because some advanced flows need to
    /// reach in (for example, calling SDK convenience methods like
    /// `transfer_objects` directly).
    pub inner: TransactionBuilder,
    fetcher: Option<Box<dyn Fetcher>>,
    owned: HashMap<ObjectId, ObjectReference>,
    shared: HashMap<ObjectId, SharedObjectInfo>,
}

impl PtbBuilder {
    pub fn new(sender: Address) -> Self {
        Self {
            inner: TransactionBuilder::new(sender),
            fetcher: None,
            owned: HashMap::new(),
            shared: HashMap::new(),
        }
    }

    /// Attach a [`Fetcher`] for on-demand lookup of unknown object ids.
    pub fn with_fetcher(mut self, f: Box<dyn Fetcher>) -> Self {
        self.fetcher = Some(f);
        self
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

    /// Cache-aware: picks `ImmutableOrOwned` (cached owned) or `Shared`
    /// (cached shared). On miss, calls the attached [`Fetcher`] (if any),
    /// caches the result, and returns the corresponding `Argument`. Falls
    /// back to a bare-id input on miss-with-no-fetcher (which the SDK rejects
    /// at finish time without a client).
    pub async fn resolve_object(&mut self, id: ObjectId) -> Argument {
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

        // Cache miss: try the fetcher, if any.
        let fetched = match self.fetcher.as_deref() {
            Some(f) => Some(f.fetch(id).await),
            None => None,
        };
        match fetched {
            Some(Ok(FetchedObject::Owned(r))) => {
                self.owned.insert(id, r);
                self.inner.input(Input::ImmutableOrOwned(r))
            }
            Some(Ok(FetchedObject::Shared {
                initial_shared_version,
                mutable,
            })) => {
                self.shared.insert(
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
            // Fetcher present but fetch failed — fall through to bare id.
            // Without a fetcher we go straight here. Either way the SDK will
            // surface a clearer error at finish() time if this id never gets
            // resolved.
            _ => self.inner.apply_argument(id),
        }
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

    /// Set the gas-coin object refs.
    pub fn gas(&mut self, refs: impl IntoIterator<Item = ObjectReference>) -> &mut Self {
        self.inner.gas(refs);
        self
    }

    /// Set the gas price (in nanos).
    pub fn gas_price(&mut self, price: u64) -> &mut Self {
        self.inner.gas_price(price);
        self
    }

    /// Set the gas budget (in nanos).
    pub fn gas_budget(&mut self, budget: u64) -> &mut Self {
        self.inner.gas_budget(budget);
        self
    }

    /// Convert this builder into a finalised [`Transaction`]. Forwards to the
    /// SDK's `TransactionBuilder::finish` for the no-client mode.
    pub fn finish(self) -> Result<Transaction, iota_sdk_transaction_builder::error::Error> {
        self.inner.finish()
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

// -----------------------------------------------------------------------------
// Per-Move-primitive marker traits
// -----------------------------------------------------------------------------
//
// Each trait is a closed-impl set bounded by `PTBArgument`. The single method
// `into_argument(self, b)` is async because for object-shaped inputs the
// implementation may need to hit a [`Fetcher`]; primitive impls don't await
// anything, but the async-fn signature is uniform across markers.

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
