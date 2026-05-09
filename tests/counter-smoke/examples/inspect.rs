//! Dev-inspect (read-only PTB simulation) of `counter::value` and `counter::owner`.
//!
//! Run with: `cargo run --example inspect -- <sender-address>`.
//! The sender needs gas coins on testnet for auto-gas to fill the gas slots,
//! but nothing is signed or submitted — `inspect()` performs a dry-run and
//! decodes per-command return values out of the dry-run results.

use std::str::FromStr;

use counter_rs::counter;
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const COUNTER_ID: &str = "0x17b5fb620158dc2d08f9456415314b6f80b62432b28bebc7c7ac906e2509f4ea";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sender = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example inspect -- <sender-address>")?;
    let sender = Address::from_str(&sender)?;

    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_auto_gas();

    let value_arg = counter::value(&mut ptb, counter_id).await;
    let owner_arg = counter::owner(&mut ptb, counter_id).await;

    let result = ptb.inspect().await?;

    let value: u64 = result.decode(value_arg)?;
    let owner: Address = result.decode(owner_arg)?;

    println!("counter::value({COUNTER_ID}) = {value}");
    println!("counter::owner({COUNTER_ID}) = {owner}");
    println!(
        "dry-run gas used: {} nanos",
        result.effects.gas_summary().gas_used()
    );

    Ok(())
}
