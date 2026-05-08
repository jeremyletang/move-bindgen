//! Real-world usage of the generated `counter-rs` bindings against testnet.
//!
//! Fill in `PRIVATE_KEY_HEX` and `COUNTER_ID`, then `cargo run --example ptb`.
//! The example reads the on-chain `Counter`, increments it via PTB, then
//! re-reads the `Counter` to show the updated state.

use std::str::FromStr;

use counter_rs::counter::{self, Counter};
use hex::FromHex;
use iota_sdk_crypto::ed25519::Ed25519PrivateKey;
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const PRIVATE_KEY_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";
const COUNTER_ID: &str = "0xc0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;

    let counter: Counter = client.get_object(counter_id).await?;
    println!("counter (before): {counter:#?}");

    increment_counter(&client, counter_id).await?;

    let counter: Counter = client.get_object(counter_id).await?;
    println!("counter (after):  {counter:#?}");

    Ok(())
}

async fn increment_counter(
    client: &Client,
    counter_id: ObjectId,
) -> Result<(), Box<dyn std::error::Error>> {
    let signer = Ed25519PrivateKey::new(<[u8; 32]>::from_hex(PRIVATE_KEY_HEX)?);
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
