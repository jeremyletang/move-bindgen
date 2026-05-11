# ClientExt and EffectsExt

`move-bindgen` ships two small extension traits the runtime adds
on top of the SDK's `Client` and `TransactionEffects`. They sit
between "raw SDK call" and "thing your app actually wants" and
strip out the most common boilerplate: querying an object and
decoding it, polling the indexer until effects show up, pulling
typed events out of a transaction, decoding a dynamic field.

Everything below works for both flavours. The examples use the
IOTA `Client`; the Sui side is the same shape against the Sui
GraphQL client (once its impl lands).

## `ClientExt` — typed reads against the indexer

`ClientExt` adds typed helpers to `iota_sdk_graphql_client::Client`.
Every method takes a `&impl PackageAddrs` (a `PtbBuilder` or a
free-standing `PackageRegistry`) so it knows what address to
expect on-chain for the requested type:

```rust,ignore
use counter_iota_rs::counter::{Counter, Note, NoteKey};
use move_bindgen_runtime::*;
use iota_sdk_graphql_client::Client;

let client       = Client::new_testnet();
let package_addr = Address::from_str(PACKAGE_ADDR)?;
let addrs        = PackageRegistry::at::<counter_iota_rs::Package>(package_addr);

// Fetch + type-check + BCS-decode in one call.
let counter: Counter = client.get_object(counter_id, &addrs).await?;

// Same for a batch.
let counters: Vec<Counter> =
    client.get_objects(&[id_a, id_b, id_c], &addrs).await?;

// Typed dynamic-field read. Sends K's type tag as the key, checks
// V's type tag against the indexer's reported value type, and
// BCS-decodes the value.
let note: Note = client
    .get_dynamic_field::<NoteKey, Note>(counter_id, NoteKey { slot: 7 }, &addrs)
    .await?;
```

Three patterns to notice:

- **Type-checked decoding.** Each method constructs the expected
  `TypeTag` from `T`'s `MoveType` impl and compares it to what the
  indexer returns. If your code asks for a `Counter` and the
  object is actually a `Coin<IOTA>`, you get a clear typed error,
  not a "field missing" surprise three layers down.
- **Indexer-coherent waits.** `wait_for_effects` blocks until the
  indexer has ingested every changed object in
  `effects.changed_objects`. Without it, a `get_object` call
  right after `execute()` may briefly serve pre-tx state.
  `wait_for_object` combines the two: wait + fetch + decode in
  one helper.
- **`addrs` carries the runtime package address.** Same
  `PackageAddrs` your PTB calls use. Construct one
  `PackageRegistry` at the start of your program, pass it
  everywhere.

## `EffectsExt` — decode-after-execution

`EffectsExt` is the post-execution counterpart. It hangs off
`TransactionEffects` (what `ptb.execute()` returns) and pulls out
the typed objects + events you care about:

```rust,ignore
use counter_iota_rs::counter::{Counter, Bumped};

let (effects, _cache) = ptb.execute().await?;

// All `Counter`-typed objects mutated by this tx, fully decoded.
let counters: Vec<Counter> = effects.mutated_decoded(&client, &addrs).await?;

// Same for newly created objects.
let created: Vec<Counter> = effects.created_decoded(&client, &addrs).await?;

// Every typed event the tx emitted of type `Bumped`.
let events: Vec<Bumped> = effects.events_of_type::<Bumped>(&client, &addrs).await?;
for ev in events {
    println!("counter {} bumped to {} by {}", ev.counter.bytes, ev.value, ev.by);
}
```

`mutated_in` / `created_in` / `changed_in` return only
`ObjectReference`s (no decode); the `*_decoded` variants follow
up with a typed batch fetch. Pick whichever fits — if you only
need the new versions of objects for a downstream tx,
`mutated_in` is cheaper.

Combined with auto-gas + auto-resolved objects, the full
write-then-read round-trip becomes:

```rust,ignore
let mut ptb = PtbBuilder::new(sender)
    .with_client(client.clone())
    .with_signer(signer)
    .with_auto_gas()
    .with_package::<counter_iota_rs::Package>(addr);

counter::increment(&mut ptb, counter_id, 5_u64).await;
let (effects, _cache) = ptb.execute().await?;

client.wait_for_effects(&effects, WaitOptions::default()).await?;
let updated: Vec<Counter> = effects.mutated_decoded(&client, &addrs).await?;
```

No raw BCS, no manual `TypeTag` construction, no version
bookkeeping.

## `Package::at(addr)` — read tags without a builder

If all you need is a `TypeTag` (filtering an event subscription,
decoding a stored blob), there's an even smaller surface. Every
generated crate emits a `Package::at(addr) -> PackageAt` handle
with one method per Move module, each returning a `ModuleAt`
that exposes `<type>_tag()` methods for the module's non-generic
datatypes:

```rust,ignore
let pkg = counter_iota_rs::Package::at(package_addr);
let counter_tag = pkg.counter().counter_tag();
let bumped_tag  = pkg.counter().bumped_tag();
```

Equivalent to `Counter::type_tag_at(package_addr)` and
`Bumped::type_tag_at(package_addr)` — same thing, more
discoverable when you have an address in hand and want to ask
"what types does this module expose?".

## Where to look next

- `tests/counter-smoke-iota/examples/dynamic_field.rs` — full
  `ClientExt::get_dynamic_field` round-trip.
- `tests/counter-smoke-iota/examples/events.rs` — typed event
  decoding via `EffectsExt::events_of_type`.
- `crates/move-bindgen-ext-iota/src/lib.rs` — the trait
  definitions themselves; the Sui counterpart lives at
  `crates/move-bindgen-ext-sui/src/lib.rs`.
