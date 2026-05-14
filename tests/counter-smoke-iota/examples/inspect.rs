//! Dev-inspect (read-only PTB simulation) of `counter::value`,
//! `counter::owner`, and the multi-return `counter::snapshot`.
//!
//! Run with: `cargo run --example inspect -- <sender-address>`.
//! The sender needs gas coins on testnet for auto-gas to fill the gas slots,
//! but nothing is signed or submitted — `inspect()` performs a dry-run and
//! decodes per-command return values out of the dry-run results.
//!
//! `snapshot` returns `(u64, u256)`. The generated Rust binding hands
//! back a tuple `(Argument, Argument)` of `Argument::NestedResult`
//! handles; `InspectResult::decode(arg)` pulls each slot out
//! individually with its own Rust type.

use std::str::FromStr;

use counter_iota_rs::counter;
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

const COUNTER_ID: &str = "0xf2850c3a3b6a4abccb9602ee454a578ad4f01bafeb492833621f05f72f97fd36";
const PACKAGE_ADDR: &str = "0xc27f4d59eb52aee53c5398037de235adecc9642407a9a7c67d99178de65ad368";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sender = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example inspect -- <sender-address>")?;
    let sender = Address::from_str(&sender)?;

    let client = Client::new_testnet();
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let package_addr = Address::from_str(PACKAGE_ADDR)?;

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_auto_gas()
        .with_package::<counter_iota_rs::Package>(package_addr);

    // Single-return calls produce one `Argument::Result(_)` each.
    let value_arg = counter::value(&mut ptb, counter_id).await;
    let owner_arg = counter::owner(&mut ptb, counter_id).await;
    // Multi-return: `snapshot(): (u64, u256)` produces a pair of
    // `Argument::NestedResult(_, 0)` / `NestedResult(_, 1)` handles.
    let (snap_value, snap_big) = counter::snapshot(&mut ptb, counter_id).await;

    let result = ptb.inspect().await?;

    let value: u64 = result.decode(value_arg)?;
    let owner: Address = result.decode(owner_arg)?;
    // Each nested slot decodes with its own type.
    let snap_v: u64 = result.decode(snap_value)?;
    let snap_b: U256 = result.decode(snap_big)?;

    println!("counter::value({COUNTER_ID}) = {value}");
    println!("counter::owner({COUNTER_ID}) = {owner}");
    println!("counter::snapshot({COUNTER_ID}) = (value={snap_v}, big_value={snap_b})");
    println!(
        "dry-run gas used: {} nanos",
        result.effects.gas_summary().gas_used()
    );

    Ok(())
}
