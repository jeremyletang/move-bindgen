# Introduction

`move-bindgen` generates Rust bindings for a Move package — analogous
to what `abigen` does for Solidity. You point it at a Move source
package, it produces a Rust crate you can depend on, and your
off-chain code calls Move functions through type-safe builders.

## What you get

For every Move package, `move-bindgen` emits:

- Rust `struct` / `enum` for every Move datatype, with serde derives
  that match Move's BCS wire format.
- A `MoveType` impl per datatype, so generic Move calls can supply
  the right `TypeTag` at the call site.
- Per-function async builders that take an `&mut PtbBuilder` and
  return an `Argument` handle — call them like ordinary Rust
  functions, the SDK plumbing happens behind the scenes.
- Read-only helpers: dev-inspect "view" calls that decode the
  result, `Package::at(addr)` for non-PTB callers that just need a
  `TypeTag`.

A small example. Given this Move:

```move
module counter_iota::counter;

public struct Counter has key, store {
    id: UID,
    value: u64,
}

public fun increment(c: &mut Counter, by: u64) {
    c.value = c.value + by;
}
```

`move-bindgen` lets you write:

```rust,ignore
use counter_iota_rs::counter;
use move_bindgen_runtime::*;

let mut ptb = PtbBuilder::new(sender)
    .with_client(client)
    .with_signer(signer)
    .with_auto_gas()
    .with_package::<counter_iota_rs::Package>(package_addr);

counter::increment(&mut ptb, counter_id, 5_u64).await;

let (effects, _cache) = ptb.execute().await?;
```

No hand-written PTB serialization, no manual gas plumbing, no
copy-pasted object refs. The generated `increment` call accepts
either a bare `ObjectId` (resolved against the builder's object
cache), an `Argument` (e.g. the result of a previous call), or
fully-resolved object handles — whichever shape your code already
has.

## Why generate?

The Move source is the source of truth for a smart contract's
shape. Off-chain code that talks to it duplicates that shape
somewhere — either by hand-encoding BCS, by curating a separate
TypeScript type system, or by living with `serde_json::Value`
everywhere. Each approach decays the moment the contract changes.

Generating bindings from the same source the Move compiler reads
keeps the two in sync mechanically. Rename a Move struct field and
your Rust code stops compiling — not at runtime, against testnet,
hours into a debugging session.

## Supported chains

`move-bindgen` targets two Move flavours:

- **IOTA** — picked via `flavour = "iota"` in your config, or
  `move-bindgen init --flavour iota` (the default).
- **Sui** — picked via `flavour = "sui"`.

One project = one chain. The same generated code shape works on
both; only the runtime crate (`move-bindgen-runtime-iota` /
`-sui`) differs, and the CLI picks the right one for you.

## Where to next

- **[Installation](quickstart/installation.md)** — build the CLI
  from source.
- **[Your first bindings](quickstart/your-first-bindings.md)** —
  scaffold a project, generate bindings for the example counter
  package, and look at what came out.
- **[Calling a function](quickstart/calling-a-function.md)** —
  wire the generated bindings into a small program that builds a
  PTB and inspects its shape.

The book is in early development. Topics planned for later
chapters — multi-package workspaces, custom address registries,
dev-inspect read flows, the full CLI / config reference — aren't
written yet. Open an issue if you'd like one prioritised.
