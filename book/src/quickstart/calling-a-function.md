# Calling a function

You have a generated crate. Time to use it.

This chapter builds a PTB *offline* — no network, no signer — and
inspects the resulting `ProgrammableTransaction` to see what
got assembled. That's what the in-tree
`tests/counter-smoke-iota/tests/build_ptb.rs` does on every CI
run, and it's the cheapest way to convince yourself the
generated bindings line up with what the chain expects before
risking a real transaction.

## A consumer crate

Make a new crate that path-depends on the generated bindings:

```toml
# my-counter-app/Cargo.toml
[package]
name    = "my-counter-app"
version = "0.1.0"
edition = "2021"

[dependencies]
counter-iota-rs      = { path = "../move-bindgen/generated/counter-iota-rs" }
move-bindgen-runtime = { package = "move-bindgen-runtime-iota",
                         path = "../move-bindgen/crates/move-bindgen-runtime-iota" }
tokio                = { version = "1", features = ["rt", "macros"] }
```

Once we ship to crates.io the runtime dep becomes a regular
`version =`/`git =`. The `package =` aliasing is so generated code
can write `use move_bindgen_runtime::*;` regardless of flavour —
the consumer crate decides which concrete crate that alias points
at.

## Building a PTB

```rust,ignore
use std::str::FromStr;

use counter_iota_rs::counter;
use iota_sdk_crypto::{ed25519::Ed25519PrivateKey, ToFromBech32};
use iota_sdk_graphql_client::Client;
use move_bindgen_runtime::*;

// Stand-in addresses. Real usage parses these from `iota client
// publish` output, a testnet snapshot, or your app config.
const PACKAGE_ADDR: &str = "0x00000000000000000000000000000000000000000000000000000000000000ab";
const COUNTER_ID:   &str = "0x00000000000000000000000000000000000000000000000000000000000000c0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let private_key  = std::env::args().nth(1).expect("usage: <iotaprivkey1...>");
    let signer       = Ed25519PrivateKey::from_bech32(&private_key)?;
    let sender       = signer.public_key().derive_address();

    let package_addr = Address::from_str(PACKAGE_ADDR)?;
    let counter_id   = ObjectId::from_str(COUNTER_ID)?;

    let mut ptb = PtbBuilder::new(sender)
        .with_client(Client::new_testnet())                    // Fetcher + Submitter + GasOracle
        .with_signer(signer)                                   // any IotaSigner
        .with_auto_gas()                                       // pick coin / price / budget for you
        .with_package::<counter_iota_rs::Package>(package_addr); // bind the runtime id

    // The generated call takes a bare `ObjectId`. The builder's
    // fetcher hits the indexer on the first reference and caches
    // the object's owner + version for the rest of the PTB. No
    // hand-rolled `Input` variant, no manual version bookkeeping.
    counter::increment(&mut ptb, counter_id, 5_u64).await;

    let (effects, _cache) = ptb.execute().await?;
    println!("tx digest: {}", effects.as_v1().transaction_digest);

    Ok(())
}
```

Run with a funded testnet key:

```sh
cargo run -- iotaprivkey1...
```

That's the whole thing. Four builder steps + one call + one
execute. No `register_shared`, no `register_owned`, no manual gas
plumbing, no PTB-level fiddling.

Worth understanding what each builder step contributes:

- `with_client(client)` — slots a `Client` into the builder as a
  `Fetcher` (auto-resolves objects), a `Submitter` (signs +
  submits), and a `GasOracle` (queries reference gas price + picks
  coins). One handle covers all three.
- `with_signer(signer)` — any `IotaSigner` (Ed25519, secp256k1,
  …). The builder produces the signature when `execute()` is
  called.
- `with_auto_gas()` — opts in to automatic gas-coin selection,
  reference gas price, and dry-run-based budget estimation. Each
  is per-slot, so calling `gas_budget(...)` later only locks the
  budget — coin and price stay automatic.
- `with_package::<P>(addr)` — binds the generated `Package`
  marker to its on-chain address. Every `move_call` emitted by the
  bindings reads this map. Multiple `with_package` calls register
  multiple packages for the same PTB.

## Where to look next

- `tests/counter-smoke-iota/examples/ptb.rs` — the working
  testnet executable this chapter is based on.
- `tests/counter-smoke-iota/examples/inspect.rs` — dev-inspect
  read flow (no signer, no gas, just a dry-run that decodes
  per-command return values).
- `tests/counter-smoke-iota/examples/events.rs` —
  `EffectsExt::events_of_type::<E>` for typed event decoding.
- `tests/counter-smoke-iota/examples/dynamic_field.rs` — typed
  dynamic-field round-trip via `ClientExt::get_dynamic_field`.
- `tests/counter-smoke-iota/tests/build_ptb.rs` — the offline
  PTB-shape assertion that runs on every CI build, for when you
  want to inspect the assembled `ProgrammableTransaction`
  yourself without going through `execute`.

The Sui mirror lives under `tests/counter-smoke-sui/` and
`configs/counter-sui.toml`.

## What's next

This was a single-package single-call walkthrough. The pieces
under the hood — `PtbBuilder`, the `Package` marker, the
`PackageAddrs` lookup, the `MoveType` / `ArgumentX` traits — get
their own chapters in later sections of the book. None of those
are written yet; the [introduction](../introduction.md) lists
what's planned.

For now, the existing in-tree examples are the best reference:

- `tests/counter-smoke-iota/tests/build_ptb.rs` — offline PTB
  assertion (what this chapter walked through).
- `tests/counter-smoke-iota/examples/ptb.rs` — live execute path.
- `tests/counter-smoke-iota/examples/inspect.rs` — dev-inspect
  read path.
- `tests/counter-smoke-iota/examples/events.rs` — `EffectsExt`
  event decoding.
- `tests/counter-smoke-iota/examples/dynamic_field.rs` — typed
  dynamic-field round-trip.

The Sui mirror lives under `tests/counter-smoke-sui/` and
`configs/counter-sui.toml`.
