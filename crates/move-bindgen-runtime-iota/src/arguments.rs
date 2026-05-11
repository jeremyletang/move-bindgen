//! Per-Move-primitive marker traits used as bounds on generated call
//! builders (`PureBool`, `PureU64`, `PureU256`, `PureAddress`,
//! `PureID`, `PureString`, `PureVec<T>`, `PureOption<T>`), plus the
//! [`ArgumentObject<T>`] generic fallback used wherever codegen
//! would otherwise emit a bare `impl PTBArgument`.
//!
//! `into_argument` bodies are async to match the codegen'd
//! `Argument*` traits; primitive impls themselves don't await.

use move_bindgen_ext_core::decl_pure_trait;

use crate::{
    framework::ID, Address, Argument, Input, MoveArg, ObjectId, ObjectReference, PTBArgument,
    PtbBuilder, Receiving, Shared, SharedMut, U256,
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

/// `primitive_types::U256` ships a serde impl (`impl-serde`) that always uses
/// hex strings, which is incompatible with Move's BCS-as-32-LE-bytes wire
/// format. The codegen attaches `#[serde(with = "u256_le")]` to every U256
/// struct/enum field, and the `PureU256` impl below pushes a `Pure` input with
/// the correct LE bytes — bypassing the SDK's broken `MoveArg for U256`.
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

/// `U256` marker. Custom impl bypasses the SDK's `MoveArg for U256`
/// (which uses hex-string serde) and pushes a 32-LE-bytes `Input::Pure`
/// — matches Move's wire format. Codegen attaches
/// `#[serde(with = "move_bindgen_runtime::u256_le")]` on every struct
/// field that holds a `U256` so the two paths agree.
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
