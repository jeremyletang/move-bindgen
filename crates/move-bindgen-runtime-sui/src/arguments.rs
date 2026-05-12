//! Per-Move-primitive marker traits used as bounds on generated call
//! builders (`PureBool`, `PureU64`, `PureU256`, `PureAddress`,
//! `PureID`, `PureString`, `PureVec<T>`, `PureOption<T>`), plus the
//! [`ArgumentObject<T>`] generic fallback used wherever codegen
//! would otherwise emit a bare `impl PTBArgument`.
//!
//! `into_argument` bodies are async to match the codegen'd
//! `Argument*` traits; primitive impls themselves don't await.

use move_bindgen_ext_core::decl_pure_trait;
use serde::Serialize;

use crate::{
    framework::ID, Address, Argument, MoveArg, ObjectId, ObjectReference, PTBArgument, PtbBuilder,
    Receiving, Shared, SharedMut, U256,
};

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
    // Pure inputs have no on-chain mutability distinction; codegen
    // still emits `_ref` / `_mut` for `&T` / `&mut T` params, so both
    // default to `into_argument`.
    #[allow(async_fn_in_trait)]
    async fn into_argument_ref(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
    }
    #[allow(async_fn_in_trait)]
    async fn into_argument_mut(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
    }
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
    #[allow(async_fn_in_trait)]
    async fn into_argument_ref(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
    }
    #[allow(async_fn_in_trait)]
    async fn into_argument_mut(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
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
    #[allow(async_fn_in_trait)]
    async fn into_argument_ref(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
    }
    #[allow(async_fn_in_trait)]
    async fn into_argument_mut(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
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
    #[allow(async_fn_in_trait)]
    async fn into_argument_ref(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
    }
    #[allow(async_fn_in_trait)]
    async fn into_argument_mut(self, b: &mut PtbBuilder) -> Argument
    where
        Self: Sized,
    {
        self.into_argument(b).await
    }
}

impl<T> ArgumentObject<T> for Argument {}
impl<T> ArgumentObject<T> for ObjectId {
    async fn into_argument(self, b: &mut PtbBuilder) -> Argument {
        b.resolve_object(self).await
    }
    async fn into_argument_ref(self, b: &mut PtbBuilder) -> Argument {
        // Goes through the async resolver so the Fetcher can lazily
        // populate the cache. `apply_argument(Shared(self))` would
        // require the cache to already have the entry — that
        // synchronous path can't await a fetch.
        b.resolve_object_shared(self, false).await
    }
    async fn into_argument_mut(self, b: &mut PtbBuilder) -> Argument {
        b.resolve_object_shared(self, true).await
    }
}
impl<T> ArgumentObject<T> for ObjectReference {}
impl<T> ArgumentObject<T> for Shared<ObjectId> {}
impl<T> ArgumentObject<T> for SharedMut<ObjectId> {}
impl<T> ArgumentObject<T> for Receiving<ObjectId> {}
