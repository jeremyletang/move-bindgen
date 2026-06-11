//! Runtime support for Sui-flavoured generated bindings.
//!
//! Mirror of `move-bindgen-runtime-iota` for the Sui chain. Code that
//! consumes Sui-flavoured generated bindings depends on this crate via
//! the `move-bindgen-runtime` alias (`{ package =
//! "move-bindgen-runtime-sui", … }`).
//!
//! Sui's SDK is shaped differently from IOTA's — there are no
//! `MoveType` / `MoveArg` / `PTBArgument` traits and no
//! `Shared<T>` / `SharedMut<T>` / `Receiving<T>` wrappers; pure inputs
//! go through `builder.pure::<T: serde::Serialize>(&v)` directly. Our
//! codegen targets a trait-based abstraction; this crate provides it
//! and adapts to Sui's serde-based builder under the hood. Trait /
//! type primitives live in `move-bindgen-ext-sui`; this crate adds
//! the higher-level [`PtbBuilder`] and the per-Move-primitive marker
//! traits generated code uses as call-builder bounds.

pub use sui_sdk_types::{
    Address, Argument as OnChainArgument, Command, Digest, Identifier, Input, Mutability, Object,
    ObjectReference, SharedInput, StructTag, Transaction, TransactionEffects, TypeTag,
    UserSignature, Version,
};
pub use sui_transaction_builder::TransactionBuilder;

pub use move_bindgen_ext_sui::{
    pure_bytes_of, u256_le, Argument, AsciiString, ClientExt, DecodeError, DryRunError,
    DryRunEstimateFuture, DryRunFuture, DryRunner, EventReader, EventReaderError, EventsByTxFuture,
    FetchError, FetchFuture, FetchedObject, Fetcher, FindByTypeFuture, FindError, GasOracle,
    GetError, InputKind, InspectResult, ListGasCoinsFuture, MoveArg, MoveType, NoPackage, ObjectId,
    ObjectTypeFinder, OracleError, PTBArgument, PackageAddrs, PackageRegistry, PureBytes,
    Receiving, RefGasPriceFuture, Shared, SharedMut, SubmitError, SubmitFuture, Submitter,
    SuggestBudgetFuture, WaitError, WaitOptions, MOVE_STDLIB_ADDRESS, U256,
};

pub mod arguments;
pub mod builder;
pub mod cache;
pub mod deployer;
pub mod effects;
pub mod framework;
pub mod signer;

pub use arguments::{
    ArgumentObject, PureAddress, PureAsciiString, PureBool, PureID, PureOption, PureString,
    PureU128, PureU16, PureU256, PureU32, PureU64, PureU8, PureVec,
};
pub use builder::{ExecuteError, InnerBuilder, PtbBuilder};
pub use cache::{CachedObject, ObjectCache};
pub use deployer::{DeployResult, PackageDeployer, UpgradePolicy};
pub use effects::{EffectsDecodeError, EffectsExt, EventsError};
pub use framework::{make_struct_tag, ID, SUI_FRAMEWORK_ADDRESS, UID};
pub use signer::{DynSigner, SignError, SignFuture};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_arg_for_u64_round_trips_through_bcs() {
        let bytes = 42u64.pure_bytes().0;
        let back: u64 = bcs::from_bytes(&bytes).unwrap();
        assert_eq!(back, 42);
    }

    #[test]
    fn move_arg_for_u256_uses_le_bytes() {
        let bytes = U256::from(1u64).pure_bytes().0;
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[0], 1, "low-order byte first");
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn id_move_type_targets_sui_framework() {
        let reg = PackageRegistry::new();
        match ID::type_tag(&reg) {
            TypeTag::Struct(s) => {
                assert_eq!(*s.address(), SUI_FRAMEWORK_ADDRESS);
                assert_eq!(s.module().as_str(), "object");
                assert_eq!(s.name().as_str(), "ID");
            }
            other => panic!("ID::type_tag must be a struct tag, got {other:?}"),
        }
    }

    // The two tests below pin down the fix for the
    // `move-bindgen-string-typetag` bug — generic-position string
    // arguments must reach chain as `Struct(0x1::string::String)` /
    // `Struct(0x1::ascii::String)`, never `Vector(U8)`.
    #[test]
    fn string_move_type_targets_move_stdlib_string() {
        let reg = PackageRegistry::new();
        match <String as MoveType>::type_tag(&reg) {
            TypeTag::Struct(s) => {
                assert_eq!(*s.address(), MOVE_STDLIB_ADDRESS);
                assert_eq!(s.module().as_str(), "string");
                assert_eq!(s.name().as_str(), "String");
            }
            other => panic!("String::type_tag must be a struct tag, got {other:?}"),
        }
    }

    #[test]
    fn ascii_string_move_type_targets_move_stdlib_ascii() {
        let reg = PackageRegistry::new();
        match <AsciiString as MoveType>::type_tag(&reg) {
            TypeTag::Struct(s) => {
                assert_eq!(*s.address(), MOVE_STDLIB_ADDRESS);
                assert_eq!(s.module().as_str(), "ascii");
                assert_eq!(s.name().as_str(), "String");
            }
            other => panic!("AsciiString::type_tag must be a struct tag, got {other:?}"),
        }
    }

    #[test]
    fn cache_round_trips_registered_objects() {
        let mut cache = ObjectCache::new();
        let id = Address::TWO;
        cache.register_shared(id, 7);
        assert!(matches!(
            cache.lookup(&id),
            Some(CachedObject::Shared {
                initial_shared_version: 7
            })
        ));
    }
}
