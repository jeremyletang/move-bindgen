//! Read typed events emitted by a transaction.
//!
//! Run with: `cargo run --example events -- <suiprivkey1...>`.
//! Bumps the counter twice in a single PTB, waits for the indexer, then
//! pulls every `Bumped` event the tx emitted via
//! `EffectsExt::events_of_type::<Bumped>` and prints them.

use std::str::FromStr;

use counter_sui_rs::counter::{self, Bumped};
use move_bindgen_runtime::*;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_rpc::Client;

const COUNTER_ID: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const PACKAGE_ADDR: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example events -- <suiprivkey1...>")?;

    let client = Client::new(Client::TESTNET_FULLNODE)?;
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let package_addr = Address::from_str(PACKAGE_ADDR)?;
    let addrs = PackageRegistry::at::<counter_sui_rs::Package>(package_addr);

    let signer = Ed25519PrivateKey::from_suiprivkey(&private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas()
        .with_package::<counter_sui_rs::Package>(package_addr);

    counter::increment(&mut ptb, counter_id, 1_u64).await;
    counter::increment(&mut ptb, counter_id, 4_u64).await;

    let (effects, _cache) = ptb.execute().await?;
    match &effects {
        TransactionEffects::V2(v2) => println!("tx digest: {}", v2.transaction_digest),
        TransactionEffects::V1(_) => println!("tx digest: (v1 effects)"),
    }

    client
        .wait_for_effects(&effects, WaitOptions::default())
        .await?;

    let events: Vec<Bumped> = effects.events_of_type::<Bumped>(&client, &addrs).await?;
    println!("emitted {} Bumped event(s):", events.len());
    for ev in &events {
        println!(
            "  counter={} value={} by={}",
            ev.counter.bytes, ev.value, ev.by
        );
    }

    Ok(())
}
