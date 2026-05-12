//! Real-world usage of the generated `counter-sui-rs` bindings against testnet.
//!
//! Run with: `cargo run --example ptb -- <suiprivkey1...>`.
//! Reads the on-chain `Counter`, runs a PTB that exercises `u64` and `u256`
//! parameters in a single transaction, then re-reads the `Counter` to
//! show the updated state.

use std::str::FromStr;

use counter_sui_rs::counter::{self, Counter};
use move_bindgen_runtime::*;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_rpc::Client;

/// On-chain id of the shared `Counter`. Replace with the id printed by
/// your own `sui client publish` deployment.
const COUNTER_ID: &str = "0x28b54df6a98b22df5820bfe250e373b4732adf8aa8e1413d4a2bd956b4f6718d";
/// On-chain address the counter package is published at.
const PACKAGE_ADDR: &str = "0x06991aed137283b8d40a3511250a78805460c7dcf1b5d1fbb7c56a534b17825a";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example ptb -- <suiprivkey1...>")?;

    let client = Client::new(Client::TESTNET_FULLNODE)?;
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let package_addr = Address::from_str(PACKAGE_ADDR)?;
    let addrs = PackageRegistry::at::<counter_sui_rs::Package>(package_addr);

    let counter: Counter = client.get_object(counter_id, &addrs).await?;
    println!("counter (before): {counter:#?}");

    let effects = bump_counter(&client, &private_key, counter_id, package_addr).await?;

    // Block until the indexer reflects every changed object in `effects`.
    client
        .wait_for_effects(&effects, WaitOptions::default())
        .await?;

    let counter: Counter = client.get_object(counter_id, &addrs).await?;
    println!("counter (after):  {counter:#?}");

    Ok(())
}

async fn bump_counter(
    client: &Client,
    private_key: &str,
    counter_id: ObjectId,
    package_addr: Address,
) -> Result<TransactionEffects, Box<dyn std::error::Error>> {
    let signer = Ed25519PrivateKey::from_suiprivkey(private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas()
        .with_package::<counter_sui_rs::Package>(package_addr);

    // Two calls in one PTB, exercising u64 and u256 args.
    counter::increment(&mut ptb, counter_id, 1_u64).await;
    counter::set_target_u256(&mut ptb, counter_id, U256::from(123_456_789_u64)).await;

    let (effects, _cache) = ptb.execute().await?;

    match &effects {
        TransactionEffects::V2(v2) => println!("tx digest: {}", v2.transaction_digest),
        TransactionEffects::V1(_) => println!("tx digest: (v1 effects)"),
    }

    Ok(effects)
}
