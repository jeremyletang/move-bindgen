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
    u256_le, AsciiString, DryRunError, EventReaderError, FindError, NoPackage, SubmitError,
    WaitOptions,
};

/// Canonical address of the `move-stdlib` framework package (`0x1`).
/// Generated code references `0x1::string::String` and
/// `0x1::ascii::String` through this constant rather than the runtime
/// `PackageAddrs` lookup — both are well-known across deployments.
pub const MOVE_STDLIB_ADDRESS: Address = {
    let mut bytes = [0u8; 32];
    bytes[31] = 0x01;
    Address::new(bytes)
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

// `MoveType` impls for the two stdlib string types. Codegen maps Move
// `0x1::string::String` to Rust `String` and `0x1::ascii::String` to
// the dedicated `AsciiString` newtype — both must produce the correct
// struct `TypeTag` at generic-position instantiations, or the VM will
// reject the call with `TypeMismatch`.
impl MoveType for String {
    type Package = NoPackage;
    const MODULE: &'static str = "string";
    const NAME: &'static str = "String";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag_export(MOVE_STDLIB_ADDRESS, "string", "String", Vec::new())
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag_export(MOVE_STDLIB_ADDRESS, "string", "String", Vec::new())
    }
}

impl MoveType for AsciiString {
    type Package = NoPackage;
    const MODULE: &'static str = "ascii";
    const NAME: &'static str = "String";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag_export(MOVE_STDLIB_ADDRESS, "ascii", "String", Vec::new())
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag_export(MOVE_STDLIB_ADDRESS, "ascii", "String", Vec::new())
    }
}

mod client;
mod client_ext;
mod wait;

pub use client_ext::{ClientExt, GetError};
pub use wait::WaitError;
