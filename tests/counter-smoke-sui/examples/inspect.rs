//! Dev-inspect (read-only PTB simulation) of `counter::value` and `counter::target`.
//!
//! Run with: `cargo run --example inspect -- <sender-address>`.
//! The sender needs gas coins on testnet for auto-gas to fill the gas slots,
//! but nothing is signed or submitted — `inspect()` performs a dry-run and
//! decodes per-command return values out of the simulator's output.
//!
//! Unlike the iota side, Sui's `sui_transaction_builder::Argument` has
//! private fields, so [`InspectResult::decode`] is keyed by command
//! index (0-based call order on the builder) rather than the `Argument`
//! handle returned by `move_call`.

use std::str::FromStr;

use counter_sui_rs::counter;
use move_bindgen_runtime::*;
use sui_rpc::Client;

const COUNTER_ID: &str = "0x28b54df6a98b22df5820bfe250e373b4732adf8aa8e1413d4a2bd956b4f6718d";
const PACKAGE_ADDR: &str = "0x06991aed137283b8d40a3511250a78805460c7dcf1b5d1fbb7c56a534b17825a";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sender = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example inspect -- <sender-address>")?;
    let sender = Address::from_str(&sender)?;

    let client = Client::new(Client::TESTNET_FULLNODE)?;
    let counter_id = ObjectId::from_str(COUNTER_ID)?;
    let package_addr = Address::from_str(PACKAGE_ADDR)?;

    let mut ptb = PtbBuilder::new(sender)
        .with_client(client.clone())
        .with_auto_gas()
        .with_package::<counter_sui_rs::Package>(package_addr);

    let _value_arg = counter::value(&mut ptb, counter_id).await; // command 0
    let _target_arg = counter::target(&mut ptb, counter_id).await; // command 1

    let result = ptb.inspect().await?;

    let value: u64 = result.decode(0)?;
    let target: u64 = result.decode(1)?;

    println!("counter::value({COUNTER_ID})  = {value}");
    println!("counter::target({COUNTER_ID}) = {target}");
    println!(
        "dry-run gas used: {} MIST",
        result.effects.gas_summary().gas_used()
    );

    Ok(())
}
