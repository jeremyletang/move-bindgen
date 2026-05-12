//! Round-trip a dynamic field on the `Counter` shared object.
//!
//! Run with: `cargo run --example dynamic_field -- <suiprivkey1...>`.
//! Calls `counter::set_note` to attach a `Note` under a `NoteKey { slot }`
//! on the existing `Counter`, waits for the indexer to ingest the change,
//! and reads it back via `ClientExt::get_dynamic_field::<NoteKey, Note>`.

use std::str::FromStr;

use counter_sui_rs::counter::{self, Note, NoteKey};
use move_bindgen_runtime::*;
use sui_crypto::ed25519::Ed25519PrivateKey;
use sui_rpc::Client;

const COUNTER_ID: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const PACKAGE_ADDR: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example dynamic_field -- <suiprivkey1...>")?;

    let client = Client::new(Client::TESTNET_FULLNODE)?;
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let package_addr = Address::from_str(PACKAGE_ADDR)?;
    let addrs = PackageRegistry::at::<counter_sui_rs::Package>(package_addr);
    let slot = 7u64;
    let text = b"hello from move-bindgen".to_vec();

    let signer = Ed25519PrivateKey::from_suiprivkey(&private_key)?;
    let sender = signer.public_key().derive_address();

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_signer(signer)
        .with_auto_gas()
        .with_package::<counter_sui_rs::Package>(package_addr);

    counter::set_note(&mut ptb, counter_id, slot, text.clone()).await;
    let (effects, _cache) = ptb.execute().await?;
    match &effects {
        TransactionEffects::V2(v2) => println!("tx digest: {}", v2.transaction_digest),
        TransactionEffects::V1(_) => println!("tx digest: (v1 effects)"),
    }

    client
        .wait_for_effects(&effects, WaitOptions::default())
        .await?;

    let note: Note = client
        .get_dynamic_field::<NoteKey, Note>(counter_id, NoteKey { slot }, &addrs)
        .await?;

    println!(
        "note at slot {slot}: {:?}",
        std::str::from_utf8(&note.text).unwrap_or("<non-utf8>")
    );

    Ok(())
}
