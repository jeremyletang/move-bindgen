//! Backend interfaces for the Sui chain — chain-generic trait shapes
//! `move_bindgen_runtime_sui::PtbBuilder` consumes, plus a place for
//! GraphQL-client integration once `sui-graphql`'s schema is wired up.
//!
//! The chain-generic trait surface ([`Fetcher`], [`Submitter`],
//! [`GasOracle`], [`ObjectTypeFinder`], [`EventReader`], [`DryRunner`],
//! [`MoveType`], [`PackageAddrs`], [`PackageRegistry`]) is emitted into
//! this crate by `move_bindgen_ext_core::define_backend_traits!`. What
//! lives here on top is Sui-specific: `MoveArg` / `PTBArgument` /
//! `InputKind` / `Shared` / `SharedMut` / `Receiving` wrappers, an
//! `ObjectId = Address` alias, and a re-export of
//! `sui_transaction_builder::Argument`.

use serde::Serialize;
use sui_sdk_types::{Address, ObjectReference};
pub use sui_transaction_builder::Argument;

pub use primitive_types::U256;

pub use move_bindgen_ext_core::{
    u256_le, DryRunError, EventReaderError, FindError, NoPackage, SubmitError, WaitOptions,
};

/// On Sui, an object's id is just a 32-byte address — `sui-sdk-types`
/// has no separate `ObjectId` type. The alias keeps generated code
/// flavour-agnostic so it can write `ObjectId` regardless of which
/// runtime it's compiled against.
pub type ObjectId = Address;

move_bindgen_ext_core::define_backend_traits! {
    address              = Address,
    type_tag             = sui_sdk_types::TypeTag,
    object_id            = ObjectId,
    object_reference     = sui_sdk_types::ObjectReference,
    transaction          = sui_sdk_types::Transaction,
    transaction_effects  = sui_sdk_types::TransactionEffects,
    user_signature       = sui_sdk_types::UserSignature,
    digest               = sui_sdk_types::Digest,
    struct_tag           = sui_sdk_types::StructTag,
    identifier           = sui_sdk_types::Identifier,
    argument             = Argument,
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


