# PtbBuilder

`PtbBuilder` is the central runtime type. Every generated call
takes one. Every backend handle (`Fetcher`, `Submitter`,
`GasOracle`, …) plugs into one. Every PTB you ever build with
`move-bindgen` flows through one.

This chapter explains what it actually does. None of the examples
below are required reading to *use* `move-bindgen` — the
quickstart shows you can get away knowing only the four `with_*`
builder calls — but the moment you want something slightly off
the happy path (a precomputed gas coin, a dry-run, an
already-resolved `ObjectReference`), knowing what's under the
hood saves you a lot of guesswork.

## What it wraps

A `PtbBuilder` is a thin layer over the SDK's `TransactionBuilder`
plus four pieces of mutable state:

```text
PtbBuilder ┌─ inner: TransactionBuilder  (the actual PTB being built)
           ├─ object cache               (ObjectId → owner+version mapping)
           ├─ package map                (TypeId → on-chain address)
           └─ backend slots              (Fetcher, Submitter, GasOracle, …)
```

The SDK's `TransactionBuilder` is the thing that ultimately
serialises bytes for the chain. We don't replace it — we make it
much easier to drive from generated code that knows about
typed objects, runtime package addresses, and async network IO.

`inner` is `pub` because there are SDK methods we deliberately
don't surface (`transfer_objects`, raw `Command::*` building,
etc.). If you need one, reach through:

```rust,ignore
ptb.inner.transfer_objects(vec![arg], recipient);
ptb.inner.gas_budget(2_000_000);
ptb.inner.input(Input::ImmutableOrOwned(some_ref));
```

You'd typically only do this for things `move-bindgen` doesn't
generate (gas tuning, raw transfers, scripts).

## Backend slots — `with_*`

Generated calls are async because they may need to ask a backend
about something (resolve an `ObjectId`, fetch a gas coin, ask the
gas oracle for the reference price). The builder doesn't hardwire
a specific backend — it has four typed slots that you fill via
`with_*` methods:

| Slot              | Trait                  | What it does                                         |
|-------------------|------------------------|------------------------------------------------------|
| Fetcher           | `Fetcher`              | Resolve an `ObjectId` to its owner + version         |
| Submitter         | `Submitter`            | Sign + submit a finished `Transaction`               |
| GasOracle         | `GasOracle`            | List gas coins, query RGP, suggest a budget          |
| DryRunner         | `DryRunner`            | Dev-inspect a PTB without submitting                 |
| Signer            | `DynSigner`            | Produce the user's signature on demand               |
| ObjectTypeFinder  | `ObjectTypeFinder`     | Filter object refs by Move type (used by EffectsExt) |

Most of the time you fill all of them via one call:

```rust,ignore
let mut ptb = PtbBuilder::new(sender)
    .with_client(Client::new_testnet())   // slots Fetcher + Submitter + GasOracle + ObjectTypeFinder
    .with_signer(signer)                  // slots DynSigner
    .with_auto_gas();                     // opts in to oracle-driven gas selection
```

`with_client(c)` is a convenience that clones the same `Client`
into every slot it implements. Use the individual setters
(`with_fetcher`, `with_submitter`, `with_gas_oracle`, …) if you
want to mix backends — e.g. testnet GraphQL for reads but a
local indexer for the submitter.

`with_signer<S: IotaSigner>(signer)` blanket-wraps any
synchronous `iota_sdk_crypto::IotaSigner` into a dyn-compatible
`DynSigner`. Hardware-wallet-style async signers go in via
`with_dyn_signer` (more boilerplate, less common).

`with_auto_gas()` is per-slot opt-in:

- gas coin (auto-fill from `list_gas_coins`)
- gas price (auto-fill from `reference_gas_price`)
- gas budget (dry-run-estimate via the `DryRunner`)

Each is independently lockable — calling `ptb.inner.gas_budget(...)`
later only locks the budget; coin and price stay automatic.

## The object cache

The most opinionated part of `PtbBuilder` is its object cache.
The SDK's `TransactionBuilder` wants you to pass already-resolved
inputs:

- `Input::ImmutableOrOwned(ObjectReference)` for owned/immutable
- `Input::Shared { object_id, initial_shared_version, mutable }`
- `Input::Receiving(ObjectReference)`

Picking the right variant requires you to know each object's
ownership and current version *before* you write a single call.
On a real chain that means an RPC round-trip per object, which
splits "build PTB" into "build PTB, but only after a sequence of
async lookups have completed".

`move-bindgen`'s generated calls accept a bare `ObjectId`:

```rust,ignore
counter::increment(&mut ptb, counter_id, 5_u64).await;
```

The builder's cache is what makes this work. Three population
strategies, used together:

**1. Pre-registration.** If you already know the metadata
(e.g. you just minted the object in the same program, or it's
configured at startup), tell the builder upfront:

```rust,ignore
ptb.register_shared(counter_id, /*initial_v=*/ 1, /*mutable=*/ true);
ptb.register_owned(admin_id, admin_object_ref);
ptb.register_immutable(config_id, config_object_ref);
```

No network hit; the next call that references one of these ids
materialises the right `Input` from the cached entry.

**2. Auto-fetch on miss.** If a `Fetcher` is plugged in
(`with_client` or `with_fetcher`) and a generated call
references an `ObjectId` the cache doesn't know, the builder
hits the fetcher and stores the result before continuing. From
your code's perspective: bare `ObjectId` in, correctly-shaped
`Input` out — even for objects you've never registered.

**3. Carry-over via `with_cache`.** `ptb.execute()` returns
`(TransactionEffects, ObjectCache)`. The cache contains every
object the previous PTB touched, at their post-execution
versions. Feed it into the next builder to skip re-resolution:

```rust,ignore
let (effects_1, cache) = ptb1.execute().await?;

let mut ptb2 = PtbBuilder::new(sender)
    .with_client(client.clone())
    .with_signer(signer.clone())
    .with_auto_gas()
    .with_cache(cache)                            // pre-populated
    .with_package::<my_pkg::Package>(addr);

// Re-using the cached versions. Zero fetcher calls.
counter::increment(&mut ptb2, counter_id, 1_u64).await;
```

The cache doesn't auto-update from `effects` — that would be too
magical. If your PTB created a new shared object, register the
new id manually before the next call uses it.

## The package map

The other piece of mutable state is the package address map —
covered in detail in the [introduction's pipeline
section](../introduction.md), summary here.

Every generated crate emits a unit `pub struct Package;` marker.
Every `move_call` and `Counter::type_tag` emitted by codegen
looks up that marker's runtime address via the builder. You bind
the address per-PTB:

```rust,ignore
let mut ptb = PtbBuilder::new(sender)
    .with_package::<counter_iota_rs::Package>(counter_addr)
    .with_package::<deepbook_rs::Package>(deepbook_addr);
```

Forget to bind a package you call into → `PtbBuilder::package_id::<P>()`
panics with a clear message naming the unbound type. Bind the
same package twice → second binding wins (useful for testing
against testnet and then mainnet without changing call sites).

## Two entry points: `execute` and `inspect`

After you've appended all the calls, two ways to finish:

**`execute()`** — the write path:

```rust,ignore
let (effects, cache) = ptb.execute().await?;
```

1. Asks the `GasOracle` to fill any auto-* gas slots.
2. Calls `inner.finish()` to produce a `Transaction`.
3. Asks the `DynSigner` for the user's signature.
4. Hands the signed transaction to the `Submitter`.
5. Returns `TransactionEffects` + the (updated) object cache.

You need a `Submitter` and a `DynSigner` (and a `GasOracle` if
you used auto-gas).

**`inspect()`** — the dev-inspect read path:

```rust,ignore
let result = ptb.inspect().await?;

let value: u64       = result.decode(value_arg)?;
let owner: Address   = result.decode(owner_arg)?;
```

1. Asks the `GasOracle` to fill gas slots.
2. Calls `inner.finish()`.
3. Sends the finished PTB to the `DryRunner` for a dry-run.
4. Returns an `InspectResult` whose `decode<T>(arg)` method
   pulls the BCS-encoded return value of any `Argument::Result`
   out of the dry-run output.

No signer needed; nothing is submitted; no on-chain state is
modified. Read-only "view" functions that return non-trivial Move
types (a `vector<address>`, a custom struct, a tuple) are exactly
what this is for — Move 2024 still emits them as regular
functions rather than special view annotations, and dev-inspect
is the canonical way to call them off-chain.

The full working example lives at
`tests/counter-smoke-iota/examples/inspect.rs`.

## What happens when generated code calls in

For the curious: the body of a generated function looks roughly
like this (simplified):

```rust,ignore
pub async fn increment(
    b:    &mut PtbBuilder,
    arg0: impl ArgumentCounter,
    by:   u64,
) -> Argument {
    let a0 = arg0.into_argument(b).await;   // ObjectId → cache lookup → Input
    let a1 = by.into_argument(b).await;     // u64 → BCS-encoded Pure input
    b.move_call(
        b.package_id::<super::Package>(),   // runtime address lookup
        "counter",
        "increment",
        Vec::new(),                         // no type args
        vec![a0, a1],
    )
}
```

Three things to take away from this:

1. **Every argument funnels through `into_argument(b)`**, which
   is `ArgumentX`'s only required method. For object args
   (`ObjectId`, `Shared<ObjectId>`, etc.) the impl consults the
   cache or the fetcher and produces an `Input` variant. For
   value args (`u64`, custom datatypes, …) it BCS-encodes and
   pushes a `Pure` input. Generated calls don't know which —
   the trait dispatch handles it.
2. **`move_call` is the runtime's wrapper** around the SDK's
   underlying `MoveCall` command. It also exists as
   `move_call_n(count)` for multi-return functions (returns
   `Vec<Argument>` of `NestedResult` handles). You'd only call
   either of these directly if you're hand-rolling a Move call
   the codegen didn't emit.
3. **Generated code reads addresses via `b.package_id::<P>()`**
   — no `super::PACKAGE_ID` const, no constant baked in at
   codegen time. Same generated bindings run against any
   deployment.

## When you need the SDK directly

`inner: pub TransactionBuilder` is the escape hatch. Use it for:

- **Transfers.** `move-bindgen` doesn't emit `TransferObjects`
  commands. Build them on `inner` directly:
  `ptb.inner.transfer_objects(vec![arg], recipient_addr);`
- **Splitting / merging coins.** Same story —
  `ptb.inner.split_coins(coin_arg, vec![amount_arg])`.
- **Raw `Input::*` injection.** If you've already got an
  `ObjectReference` and don't want the cache to be involved:
  `let arg = ptb.inner.input(Input::ImmutableOrOwned(ref));`
- **Reading the assembled PTB.** Call `ptb.inner.finish()` to
  serialise without going through `execute`/`inspect` — useful
  for offline assertions, deeplinks, or handing the bytes off to
  a different signer.

Anything else generated code does (`move_call`, `move_call_n`,
`pure_bytes`) is also available on the runtime's `PtbBuilder`
surface, not just on generated functions — fine to call them
directly when you're synthesising Move calls outside of what
codegen emits.

## Where to look next

- `tests/counter-smoke-iota/examples/ptb.rs` — full
  with_client + with_signer + with_auto_gas + execute flow.
- `tests/counter-smoke-iota/examples/inspect.rs` — dev-inspect
  via `inspect()`.
- `crates/move-bindgen-runtime-iota/src/lib.rs` — the IOTA
  `PtbBuilder` itself; the Sui variant at
  `crates/move-bindgen-runtime-sui/src/lib.rs`. Worth a read if
  you want to plug in a custom backend.
