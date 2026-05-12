//! `DynSigner` — dyn-compatible wrapper over `iota-sdk-crypto`'s
//! synchronous `IotaSigner`. Lets a `Box<dyn DynSigner>` slot hold any
//! signer the SDK exposes (`Ed25519PrivateKey`, `Secp256k1PrivateKey`,
//! …).
//!
//! We use the sync trait rather than the async `TransactionSigner`
//! because the async one's returned future isn't promised `Send`,
//! which would break multi-threaded tokio usage.

use std::future::Future;
use std::pin::Pin;

use crate::{Transaction, UserSignature};

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("sign backend: {0}")]
    Backend(String),
}

pub type SignFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UserSignature, SignError>> + Send + 'a>>;

pub trait DynSigner: Send + Sync {
    fn sign_dyn<'a>(&'a self, tx: &'a Transaction) -> SignFuture<'a>;
}

impl<T> DynSigner for T
where
    T: iota_sdk_crypto::IotaSigner + Send + Sync,
{
    fn sign_dyn<'a>(&'a self, tx: &'a Transaction) -> SignFuture<'a> {
        let result = self
            .sign_transaction(tx)
            .map_err(|e| SignError::Backend(e.to_string()));
        Box::pin(async move { result })
    }
}
