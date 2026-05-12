//! Backend interfaces consumed by [`move_bindgen_runtime::PtbBuilder`], plus a
//! ready-made integration on top of `iota_sdk_graphql_client::Client`.
//!
//! The chain-generic trait surface — [`Fetcher`], [`Submitter`],
//! [`GasOracle`], [`ObjectTypeFinder`], [`EventReader`], [`DryRunner`],
//! [`MoveType`], [`PackageAddrs`], [`PackageRegistry`] — is emitted
//! into this crate by `move_bindgen_ext_core::define_backend_traits!`.
//! What lives here on top:
//!
//! - Impls of those traits for `iota_sdk_graphql_client::Client` so
//!   `PtbBuilder::with_client(graphql_client)` "just works".
//! - [`ClientExt`] — typed read helpers on `Client` (e.g. [`ClientExt::get_object`]).
//! - Wait support ([`WaitOptions`] re-exported from core, plus the
//!   iota-typed [`WaitError`] / [`GetError`] enums).

use iota_sdk_types::{
    Address, Digest, ObjectId, StructTag, Transaction, TransactionEffects, TypeTag, UserSignature,
};

pub use move_bindgen_ext_core::{
    u256_le, DryRunError, EventReaderError, FindError, NoPackage, SubmitError, WaitOptions,
};

move_bindgen_ext_core::define_backend_traits! {
    address              = Address,
    type_tag             = TypeTag,
    object_id            = ObjectId,
    object_reference     = iota_sdk_types::ObjectReference,
    transaction          = Transaction,
    transaction_effects  = TransactionEffects,
    user_signature       = UserSignature,
    digest               = Digest,
    struct_tag           = StructTag,
    identifier           = iota_sdk_types::Identifier,
}

mod client;
mod client_ext;
mod wait;

pub use client_ext::{ClientExt, GetError};
pub use wait::WaitError;
