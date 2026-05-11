//! Real-world usage of the generated `counter-rs` bindings against testnet.
//!
//! Run with: `cargo run --example ptb -- <iotaprivkey1...>`.
//! Reads the on-chain `Counter`, runs a PTB that exercises `u64`, `u256`, and
//! `ID` value parameters in a single transaction, then re-reads the `Counter`
//! to show the updated state.

use std::str::FromStr;

use counter_iota_rs::counter::{self, Counter};
use iota_sdk_crypto::{ToFromBech32, ed25519::Ed25519PrivateKey};
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const COUNTER_ID: &str = "0xf2850c3a3b6a4abccb9602ee454a578ad4f01bafeb492833621f05f72f97fd36";
/// On-chain address the counter package is published at. Replace with
/// the address from your own `iota client publish` deployment.
const PACKAGE_ADDR: &str = "0xc27f4d59eb52aee53c5398037de235adecc9642407a9a7c67d99178de65ad368";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example ptb -- <iotaprivkey1...>")?;

    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let package_addr = Address::from_str(PACKAGE_ADDR)?;
    let addrs = PackageRegistry::at::<counter_iota_rs::Package>(package_addr);

    let counter: Counter = client.get_object(counter_id, &addrs).await?;
    println!("counter (before): {counter:#?}");

    let effects = bump_counter(&client, &private_key, counter_id, package_addr).await?;

    // Block until the indexer has ingested the new state of every changed
    // object in `effects`, so the next `get_object` doesn't race the indexer.
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
    let signer = Ed25519PrivateKey::from_bech32(private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas()
        .with_package::<counter_iota_rs::Package>(package_addr);

    // Three calls in one PTB, exercising u64, u256, and ID params.
    counter::increment(&mut ptb, counter_id, 1_u64).await;
    counter::increment_big(&mut ptb, counter_id, U256::from(123_456_789_u64)).await;
    counter::set_target(&mut ptb, counter_id, ID::from(sender)).await;

    let (effects, _cache) = ptb.execute().await?;

    println!("tx digest: {}", effects.as_v1().transaction_digest);
    println!("effects:   {effects:#?}");

    Ok(effects)
}
