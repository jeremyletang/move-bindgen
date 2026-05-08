//! Real-world usage of the generated `counter-rs` bindings.
//!
//! Fill in `PRIVATE_KEY_HEX`, `COUNTER_ID`, and `ADMIN_CAP_ID`, then
//! `cargo run --example usage`.

use std::str::FromStr;

use counter_rs::counter;
use hex::FromHex;
use iota_sdk_crypto::ed25519::Ed25519PrivateKey;
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const PRIVATE_KEY_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";
const COUNTER_ID: &str = "0xc0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
const ADMIN_CAP_ID: &str = "0xadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let signer = Ed25519PrivateKey::new(<[u8; 32]>::from_hex(PRIVATE_KEY_HEX)?);
    let sender = signer.public_key().derive_address();

    let client = Client::new_testnet();
    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas();

    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let admin_id = ObjectId::from_str(ADMIN_CAP_ID)?;

    counter::increment(&mut ptb, counter_id, 5_u64).await;
    counter::value(&mut ptb, counter_id).await;
    counter::reset(&mut ptb, admin_id, counter_id).await;

    let (effects, _cache) = ptb.execute().await?;

    // Decode-after-execution: ask the chain for type-filtered changed objects.
    let updated_counters = effects.mutated_in::<counter::Counter>(&client).await?;
    println!("counters mutated: {updated_counters:?}");

    return Ok(());
}
