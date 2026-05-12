//! `PTBArgument` + `InputKind` — the trait-driven funnel every Sui-side
//! generated call uses to turn a value into something the underlying
//! `TransactionBuilder` can append. The `Shared` / `SharedMut` /
//! `Receiving` wrappers tag bare ids/refs at the call site so the
//! applier picks the right `ObjectInput` constructor.

use sui_sdk_types::ObjectReference;

use crate::{Argument, MoveArg, ObjectId};

/// Wrap an object id/ref to mark it as a *shared, read-only* input.
pub struct Shared<T>(pub T);

/// Wrap an object id/ref to mark it as a *shared, mutable* input.
pub struct SharedMut<T>(pub T);

/// Wrap an object id/ref to mark it as a `Receiving<T>` input
/// (transfer-to-object pattern).
pub struct Receiving<T>(pub T);

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
