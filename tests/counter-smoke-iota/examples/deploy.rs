//! Deploy the counter-iota package to testnet using the generated
//! `Package::deployer(...)` entry point.
//!
//! Run with: `cargo run --example deploy -- <iotaprivkey1...>`.
//!
//! The bytecode + dep set was captured at codegen time from the
//! `[publish] networks = ["testnet", "mainnet"]` block in
//! `configs/counter-iota.toml`. No `iota client publish` involved —
//! everything is one Rust call.
//!
//! On success, prints the new package id. Use it as `PACKAGE_ADDR` in
//! the `ptb.rs` example (or any other generated-binding caller).

use counter_iota_rs::{Network, Package};
use iota_sdk_crypto::{ed25519::Ed25519PrivateKey, ToFromBech32};
use iota_sdk_graphql_client::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example deploy -- <iotaprivkey1...>")?;

    let signer = Ed25519PrivateKey::from_bech32(&private_key)?;
    let sender = signer.public_key().derive_address();
    let client = Client::new_testnet();

    println!("publishing counter-iota as {sender} on testnet…");

    let result = Package::deployer(Network::Testnet)
        .sender(sender)
        .with_client(client)
        .with_signer(signer)
        .with_auto_gas()
        .execute()
        .await?;

    println!(
        "✓ published\n\
         package id : {}\n\
         tx digest  : {}",
        result.package_id,
        result.effects.as_v1().transaction_digest,
    );
    Ok(())
}
