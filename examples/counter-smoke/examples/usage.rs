//! Real-world usage of the generated `counter-rs` bindings.
//!
//! Builds a PTB that calls `counter::increment`, signs it with an Ed25519
//! private key, and submits it to testnet. Object ids are looked up on
//! demand via the runtime's built-in `Fetcher` impl on the GraphQL client
//! (enabled by the `graphql-client` feature on `move-bindgen-runtime`).
//!
//! Before running, fill in:
//! - `PRIVATE_KEY_HEX` — 32-byte raw Ed25519 secret. In real code, read this
//!   from a keystore / env / KMS, never hardcode.
//! - `COUNTER_ID` / `ADMIN_CAP_ID` — object ids you got from a previous
//!   transaction's effects (e.g. the `create` call) or from RPC.
//! - `GAS_COIN_ID` — an IOTA coin you own on testnet; ask the faucet if
//!   the account is fresh.
//!
//! Run with: `cargo run --example usage`

use std::str::FromStr;
use std::sync::Arc;

use counter_rs::counter;
use hex::FromHex;
use iota_sdk_crypto::{IotaSigner, ed25519::Ed25519PrivateKey};
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

// -----------------------------------------------------------------------------
// Inputs you'd swap out for real values.
// -----------------------------------------------------------------------------

/// Replace with your 32-byte Ed25519 secret as a hex string (no `0x`).
const PRIVATE_KEY_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";

/// The shared `Counter` object created by an earlier `counter::create` call.
const COUNTER_ID: &str = "0xc0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

/// The `AdminCap` object you own.
const ADMIN_CAP_ID: &str = "0xadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad";

/// An IOTA coin you own to pay for gas.
const GAS_COIN_ID: &str = "0x6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a6a";

const GAS_PRICE: u64 = 1_000;
const GAS_BUDGET: u64 = 10_000_000;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Signer + sender.
    let signer = Ed25519PrivateKey::new(<[u8; 32]>::from_hex(PRIVATE_KEY_HEX)?);
    let sender = signer.public_key().derive_address();

    // 2. Client. Wrapped in Arc so we can both attach it to the builder as
    //    a fetcher and use it directly to look up the gas coin.
    let client = Arc::new(Client::new_testnet());

    // 3. Builder with eager-fetch on cache miss. The `Fetcher` impl on
    //    `iota_sdk_graphql_client::Client` is provided by
    //    `move-bindgen-runtime`'s `graphql-client` feature.
    let mut ptb = PtbBuilder::new(sender).with_fetcher(Box::new(client.clone()));

    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let admin_id = ObjectId::from_str(ADMIN_CAP_ID)?;

    // 4. Move calls. The first pass on each id triggers an RPC fetch (cache
    //    miss); subsequent calls hit the populated cache.
    counter::increment(&mut ptb, counter_id, 5_u64).await;
    let _value = counter::value(&mut ptb, counter_id).await;
    counter::reset(&mut ptb, admin_id, counter_id).await;

    // 5. Gas.
    let gas_obj = client
        .object(ObjectId::from_str(GAS_COIN_ID)?, None)
        .await?
        .ok_or("gas coin not found")?;
    ptb.gas([gas_obj.object_ref()])
        .gas_price(GAS_PRICE)
        .gas_budget(GAS_BUDGET);

    // 6. Finalize, sign, submit.
    let tx = ptb.finish()?;
    let signature = signer.sign_transaction(&tx)?;
    let effects = client.execute_tx(&[signature], &tx, None).await?;

    println!("digest: {}", tx.digest());
    println!("effects: {effects:#?}");
    Ok(())
}
