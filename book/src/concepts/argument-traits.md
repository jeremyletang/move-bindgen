# Argument traits

The shapes you can pass into a generated PTB call.

Every parameter of every generated call builder takes an
`impl SomeTrait`. There are two families:

- **`PureX`** — for value parameters. One trait per Move
  primitive (`PureBool`, `PureU64`, `PureU256`, `PureAddress`,
  `PureID`, `PureString`, `PureVec<T>`, `PureOption<T>`).
- **`ArgumentX`** — for `key`-ability datatype parameters. One
  trait per generated datatype (`ArgumentCounter`,
  `ArgumentAdminCap`, `ArgumentMarket<T>`, …).

Both extend the SDK's `PTBArgument` so the SDK's existing
infrastructure (`Shared<T>` / `SharedMut<T>` / `Receiving<T>`
wrappers, the `Argument` → `Argument` identity) just works.
Codegen only adds the closed-impl set per Move type so you get
compile-time type safety instead of "any `Argument` goes".

## `PureX` — value parameters

`PureX` traits cover Move's primitive types. Each is implemented
by a small, closed set of Rust shapes: the natural Rust type plus
`Argument` (so you can chain — the return of a previous call
slots in wherever a value is expected).

| Move param                 | Generated bound       | Accepts                                     |
| -------------------------- | --------------------- | ------------------------------------------- |
| `bool`                     | `impl PureBool`       | `bool`, `Argument`                          |
| `u8` / `u16` / `u32` / `u64` / `u128` | `impl PureU8` / `PureU16` / `PureU32` / `PureU64` / `PureU128` | matching Rust int, `Argument` |
| `u256`                     | `impl PureU256`       | `U256`, `Argument`                          |
| `address`                  | `impl PureAddress`    | `Address`, `Argument`                       |
| `0x2::object::ID`          | `impl PureID`         | `ID`, `Argument`. `From<Address>` / `From<ObjectId>` provided for ergonomics. |
| `String`                   | `impl PureString`     | `String`, `&str`, `Argument`                |
| `vector<T>`                | `impl PureVec<T>`     | `Vec<T>` where `T: MoveArg`, `Argument`     |
| `Option<T>`                | `impl PureOption<T>`  | `Option<T>` where `T: MoveArg`, `Argument`  |
| `&T` / `&mut T` (T primitive) | same as `T`        | references erased; same accept-set          |

Two things to notice:

**`PureU256` has a custom impl.** The SDK's default `MoveArg
for U256` uses hex-string serde, which doesn't match Move's
32-LE-bytes BCS wire format. The runtime's `PureU256` impl
pushes a manually-encoded 32-LE-bytes `Input::Pure` instead —
serialises correctly without overriding the global serde
behaviour.

**`Argument` is always accepted.** Pass the result of a previous
call as a parameter to a later one. Codegen's tuple return for
multi-value Move functions makes this idiomatic:

```rust,ignore
// pair returns (u64, u64)
let (val, target) = counter::snapshot(&mut ptb, counter_id).await;
// `val` is an Argument; PureU64 accepts it
counter::increment(&mut ptb, counter_id, val).await;
```

## `ArgumentX` — datatype parameters

For every Move datatype with the `key` ability, codegen emits an
`ArgumentX` trait listing every Rust shape that can serve as
that datatype in argument position. The trait's closed-impl set
is the same for every `key` datatype:

| Rust shape              | What it means                                          |
| ----------------------- | ------------------------------------------------------ |
| `Argument`              | Output of a previous call.                             |
| `ObjectId`              | Bare id; the builder's cache (or attached `Fetcher`) resolves owner + version. |
| `ObjectReference`       | Fully-resolved owned object reference (id + version + digest). |
| `Shared<ObjectId>`      | Shared object referenced read-only.                    |
| `SharedMut<ObjectId>`   | Shared object referenced mutably.                      |
| `Receiving<ObjectId>`   | Object being received in a `transfer::receive` call.   |

So `counter::increment(&mut ptb, x, 5)` accepts any of:

```rust,ignore
counter::increment(&mut ptb, counter_id,                   5).await; // bare id
counter::increment(&mut ptb, SharedMut(counter_id),        5).await; // explicit mutability
counter::increment(&mut ptb, counter_object_ref,           5).await; // pre-resolved
counter::increment(&mut ptb, prev_call_result_argument,    5).await; // chain
```

The `ObjectId` impl is the only one with a custom `into_argument`
body: it consults `PtbBuilder`'s cache to pick the right `Input`
variant (Owned / Shared / Immutable / Receiving), with a fetcher
fallback on cache miss. Every other impl uses the default
delegation through the SDK.

## Non-`key` datatype parameters

For datatypes *without* `key` — your typical
`copy + drop + store` values like a small struct passed by
value — codegen emits a different `ArgumentX` impl set:

| Rust shape    | What it means                                  |
| ------------- | ---------------------------------------------- |
| `Argument`    | Output of a previous call.                     |
| `X` itself    | BCS-encode the value and push it as `Pure`.    |

So if `Bumped` is a non-`key` event payload:

```rust,ignore
some_call(&mut ptb, Bumped { value: 5, /* … */ });   // BCS-encoded into Pure
some_call(&mut ptb, prev_argument);                  // chain
```

Codegen also emits `MoveArg for Bumped` (BCS via `bcs::to_bytes`)
so the SDK's blanket `PTBArgument for T: MoveArg` impl supplies
the `Pure` packing automatically.

## References don't matter

Move's `&T` / `&mut T` parameters share the same `ArgumentX`
trait as `T` itself. Generated signatures look like:

```rust,ignore
// Move: public fun increment(c: &mut Counter, by: u64)
pub async fn increment(
    b:    &mut PtbBuilder,
    arg0: impl ArgumentCounter,
    by:   u64,
) -> Argument
```

There's no `&mut` on `arg0` — the reference is purely a Move-side
borrow-checker concern, erased before codegen sees the parameter.
You pass the same shapes regardless of whether Move took `Counter`,
`&Counter`, or `&mut Counter`.

## TxContext is stripped

The Move VM injects `&mut TxContext` into every entry / public
function that needs it. It isn't a real off-chain input — codegen
drops the parameter:

```move
// Move
public fun create(target: u64, ctx: &mut TxContext): AdminCap { /* … */ }
```

```rust,ignore
// Rust — no `ctx` param
pub async fn create(b: &mut PtbBuilder, target: u64) -> Argument
```

Same for `&TxContext`.

## Generic value parameters

Generic value parameters (`T` not bound to `key`) fall back to
the SDK's permissive `PTBArgument` bound — no per-type closed
set, because codegen doesn't know what shapes the user will
instantiate `T` with:

```rust,ignore
// Move: public fun put_in_bag<T>(b: &mut Bag, value: T)
pub async fn put_in_bag<T0: MoveType>(
    b:     &mut PtbBuilder,
    arg0:  impl ArgumentBag,
    value: impl PTBArgument,
) -> Argument
```

This is the only place generated calls drop down to the SDK's
permissive bound. If you're hitting a generic-value-parameter
call frequently and want stronger guarantees, you can manually
construct an `Argument` with the right `Input::Pure` and pass
that — but in practice generic value parameters are rare.

## Where to look next

- **[PtbBuilder](ptb-builder.md)** — the object cache + fetcher
  fallback that the `ObjectId` impls of `ArgumentX` traits feed
  into.
- **[Type mapping](type-mapping.md)** — the companion table for
  *type position* (field types, return types, generics).
- `crates/move-bindgen/src/codegen/datatype.rs::argument_trait` —
  the codegen emitter for `ArgumentX`. The closed-impl set is
  hardcoded there; if you ever want to extend what shapes a
  generated `ArgumentX` accepts, this is the function.
- `crates/move-bindgen-runtime-iota/src/lib.rs` — the `PureX`
  trait definitions and their impls. Sui mirror at
  `crates/move-bindgen-runtime-sui/src/lib.rs`.
