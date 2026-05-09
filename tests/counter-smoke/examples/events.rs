//! Read typed events emitted by a transaction.
//!
//! Run with: `cargo run --example events -- <iotaprivkey1...>`.
//! Bumps the counter twice in a single PTB, waits for the indexer, then
//! pulls every `Bumped` event the tx emitted via
//! `EffectsExt::events_of_type::<Bumped>` and prints them.

use std::str::FromStr;

use counter_rs::counter::{self, Bumped};
use iota_sdk_crypto::{ed25519::Ed25519PrivateKey, ToFromBech32};
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const COUNTER_ID: &str = "0xf2850c3a3b6a4abccb9602ee454a578ad4f01bafeb492833621f05f72f97fd36";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example events -- <iotaprivkey1...>")?;

    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;

    let signer = Ed25519PrivateKey::from_bech32(&private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas();

    counter::increment(&mut ptb, counter_id, 1_u64).await;
    counter::increment(&mut ptb, counter_id, 4_u64).await;

    let (effects, _cache) = ptb.execute().await?;
    println!("tx digest: {}", effects.as_v1().transaction_digest);

    client
        .wait_for_effects(&effects, WaitOptions::default())
        .await?;

    let events: Vec<Bumped> = effects.events_of_type::<Bumped>(&client).await?;
    println!("emitted {} Bumped event(s):", events.len());
    for ev in &events {
        println!("  counter={} value={} by={}", ev.counter.bytes, ev.value, ev.by);
    }

    Ok(())
}
