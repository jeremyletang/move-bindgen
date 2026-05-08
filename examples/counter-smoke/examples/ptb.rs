//! Real-world usage of the generated `counter-rs` bindings against testnet.
//!
//! Run with: `cargo run --example ptb -- <iotaprivkey1...>`.
//! The example reads the on-chain `Counter`, increments it via PTB, then
//! re-reads the `Counter` to show the updated state.

use std::str::FromStr;

use counter_rs::counter::{self, Counter};
use iota_sdk_crypto::{ToFromBech32, ed25519::Ed25519PrivateKey};
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const COUNTER_ID: &str = "0x17b5fb620158dc2d08f9456415314b6f80b62432b28bebc7c7ac906e2509f4ea";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example ptb -- <iotaprivkey1...>")?;

    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;

    let counter: Counter = client.get_object(counter_id).await?;
    println!("counter (before): {counter:#?}");

    increment_counter(&client, &private_key, counter_id).await?;

    // GraphQL indexer lags a bit behind execution; wait before re-reading.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let counter: Counter = client.get_object(counter_id).await?;
    println!("counter (after):  {counter:#?}");

    Ok(())
}

async fn increment_counter(
    client: &Client,
    private_key: &str,
    counter_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let signer = Ed25519PrivateKey::from_bech32(private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas();

    counter::increment(&mut ptb, counter_id, 1_u64).await;

    let (effects, _cache) = ptb.execute().await?;

    println!("tx digest: {}", effects.as_v1().transaction_digest);
    println!("effects:   {effects:#?}");

    Ok(())
}
