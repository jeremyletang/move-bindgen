//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Provides [`PtbBuilder`] (the high-level builder), [`DynSigner`], the
//! per-Move-primitive marker traits (`PureBool`, `PureU64`, …) used as bounds
//! on generated call builders, [`EffectsExt`] for post-execution object
//! lookup, and re-exports the SDK types generated code needs. Backend traits
//! ([`Fetcher`], [`Submitter`], [`GasOracle`], [`DryRunner`],
//! [`ObjectTypeFinder`]) and the GraphQL-client integration live in
//! `move-bindgen-ext` and are re-exported here.

pub use iota_sdk_transaction_builder::{
    // We deliberately *don't* re-export `iota_sdk_transaction_builder::types::MoveType`
    // any more — `move-bindgen-ext-iota` defines its own
    // `MoveType` trait with a runtime-package-id shape, mirroring the
    // Sui side. Use that one.
    types::MoveArg,
    unresolved::{Argument, Command, MoveCall},
    PTBArgument,
    PureBytes,
    Receiving,
    Shared,
    SharedMut,
    TransactionBuilder,
    TransactionSigner,
};
pub use iota_sdk_types::{
    Address, Identifier, Input, ObjectId, ObjectReference, SharedObjectReference, StructTag,
    Transaction, TransactionEffects, TypeTag, UserSignature, Version,
};
pub use move_bindgen_ext::{
    ClientExt, DecodeError, DryRunError, DryRunEstimateFuture, DryRunFuture, DryRunner,
    EventReader, EventReaderError, EventsByTxFuture, FetchError, FetchFuture, FetchedObject,
    Fetcher, FindByTypeFuture, FindError, GasOracle, GetError, InspectResult, ListGasCoinsFuture,
    MoveType, NoPackage, ObjectTypeFinder, OracleError, PackageAddrs, PackageRegistry,
    RefGasPriceFuture, SubmitError, SubmitFuture, Submitter, SuggestBudgetFuture, WaitError,
    WaitOptions,
};
pub use primitive_types::U256;

pub mod arguments;
pub mod builder;
pub mod cache;
pub mod deployer;
pub mod effects;
pub mod framework;
pub mod signer;

pub use arguments::{
    u256_le, ArgumentObject, PureAddress, PureBool, PureID, PureOption, PureString, PureU128,
    PureU16, PureU256, PureU32, PureU64, PureU8, PureVec,
};
pub use builder::{ExecuteError, PtbBuilder};
pub use cache::{ObjectCache, SharedObjectInfo};
pub use deployer::{DeployResult, PackageDeployer, UpgradePolicy};
pub use effects::{EffectsDecodeError, EffectsExt, EventsError};
pub use framework::{make_struct_tag, ID, IOTA_FRAMEWORK_ADDRESS, UID};
pub use signer::{DynSigner, SignError, SignFuture};
