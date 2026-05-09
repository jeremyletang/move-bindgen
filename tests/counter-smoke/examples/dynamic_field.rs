//! Round-trip a dynamic field on the `Counter` shared object.
//!
//! Run with: `cargo run --example dynamic_field -- <iotaprivkey1...>`.
//! Calls `counter::set_note` to attach a `Note` under a `NoteKey { slot }`
//! on the existing `Counter`, waits for the indexer to ingest the change,
//! and reads it back via `ClientExt::get_dynamic_field::<NoteKey, Note>`.

use std::str::FromStr;

use counter_rs::counter::{self, Note, NoteKey};
use iota_sdk_crypto::{ed25519::Ed25519PrivateKey, ToFromBech32};
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const COUNTER_ID: &str = "0xf2850c3a3b6a4abccb9602ee454a578ad4f01bafeb492833621f05f72f97fd36";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example dynamic_field -- <iotaprivkey1...>")?;

    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let slot = 7u64;
    let text = b"hello from move-bindgen".to_vec();

    let signer = Ed25519PrivateKey::from_bech32(&private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas();

    counter::set_note(&mut ptb, counter_id, slot, text.clone()).await;
    let (effects, _cache) = ptb.execute().await?;
    println!("tx digest: {}", effects.as_v1().transaction_digest);

    client
        .wait_for_effects(&effects, WaitOptions::default())
        .await?;

    let note: Note = client
        .get_dynamic_field::<NoteKey, Note>(counter_id, NoteKey { slot })
        .await?;

    println!(
        "note at slot {slot}: {:?}",
        std::str::from_utf8(&note.text).unwrap_or("<non-utf8>")
    );

    Ok(())
}
