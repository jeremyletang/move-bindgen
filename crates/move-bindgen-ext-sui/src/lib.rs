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

use sui_sdk_types::Address;
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

mod input_kind;
mod move_arg;

pub use input_kind::{InputKind, PTBArgument, Receiving, Shared, SharedMut};
pub use move_arg::{pure_bytes_of, MoveArg, PureBytes};
