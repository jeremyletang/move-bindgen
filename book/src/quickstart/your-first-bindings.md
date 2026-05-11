# Your first bindings

Goal of this chapter: generate Rust bindings for a small Move
package, look at what came out, and understand the moving pieces
before we wire them into actual code.

We'll use the `counter-iota` package that ships with the repo. It's
a deliberately tiny multi-module Move package — one shared
`Counter` object, an `AdminCap`, a couple of `entry` functions —
and it's what the IOTA smoke tests use, so you can mirror their
setup verbatim.

If you'd rather follow along against Sui's flavour, the steps below
all have a Sui mirror (`packages/counter-sui`,
`configs/counter-sui.toml`); the only difference is which flavour
the config declares.

## Scaffolding a new project

If you're starting from scratch in your own repo, the CLI has an
`init` subcommand that lays the project out for you:

```sh
move-bindgen init my-project --flavour iota
```

That writes `my-project/move-bindgen.toml` (a single-crate
config), `my-project/packages/<name>/Move.toml` + an empty
`sources/`, and a `.gitignore` covering the staging dir and the
generated crate. You can drop your Move sources into `sources/`
and run `move-bindgen build` from `my-project/`. Pass
`--flavour sui` to scaffold a Sui project instead.

For the rest of this chapter we'll work against the in-tree
`packages/counter-iota` fixture rather than the scaffold, so you
can see what `move-bindgen` does against a realistic Move
package.

## The Move package

`packages/counter-iota/Move.toml`:

```toml
[package]
name = "counter_iota"
edition = "2024"

[addresses]
counter_iota = "0x0"
```

Four source modules under `packages/counter-iota/sources/`:
`counter.move`, `errors.move`, `inbox.move`, `registry.move`.
They're chosen to exercise codegen's main paths — `key + store`
objects, capability handles, primitive arg types (`u64`, `u256`,
`ID`), an `entry` function, an enum with three variant shapes, and
a phantom-generic container.

You don't need to read the Move source to follow along, but it's
in the repo if you're curious.

## The config

Every `move-bindgen` invocation is driven by a config. For a
single-package project that's `configs/counter-iota.toml`:

```toml
flavour = "iota"   # default, omitted in the actual file

[output]
format  = "single-crate"
name    = "counter-iota-rs"
runtime = { path = "../crates/move-bindgen-runtime-iota" }

[package]
path = "../packages/counter-iota"
```

Three things to notice:

- `flavour` picks the build chain (defaults to `iota`; set to
  `"sui"` for the other one).
- `runtime` controls how the generated `Cargo.toml` references
  `move-bindgen-runtime`. Inside this repo it's a `path =` to the
  in-tree runtime; downstream users would use `git =` or
  `version =` (the default).
- `[package]` points at the Move source. Relative paths resolve
  against the config's own directory.

## Install + generate

The pipeline has two steps that can run independently, or one
combined shortcut:

```sh
# Either run the two steps explicitly...
./target/debug/move-bindgen install  --config configs/counter-iota.toml
./target/debug/move-bindgen generate --config configs/counter-iota.toml

# ...or run both at once.
./target/debug/move-bindgen build --config configs/counter-iota.toml
```

`install` is the side-effecting half: it stages a copy of the
Move source under `configs/.move-bindgen/counter-iota/`, rewrites
the staged `Move.toml` to use synthetic-address overrides, and
records a `packages.json` manifest. `generate` is the
deterministic half: it reads the manifest, drives the Move
compiler, walks the IR, and writes a Rust crate.

`build` is just `install` then `generate` in one invocation —
ergonomic for the common case. The two-step form is useful when
you want to inspect or edit the staged Move package between
fetch and codegen, or when CI caches the staged tree separately
from the build artefacts.

You'll see cargo-style output:

```text
   Resolving configs/counter-iota.toml
     Staging counter_iota
    Finished installing 1 package(s) in 0.01s
   Compiling counter_iota
  Generating counter-iota-rs
    Finished generating counter-iota-rs in 1.7s
```

## What got written

The generated crate lands at `configs/counter-iota-rs/` (next to
the config — that's the default `[output]` location):

```text
configs/counter-iota-rs/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── counter.rs
    ├── errors.rs
    ├── inbox.rs
    └── registry.rs
```

One Rust module per Move source module, all listed under `lib.rs`.

### lib.rs

```rust,ignore
pub mod counter;
pub mod errors;
pub mod inbox;
pub mod registry;

/// Marker type identifying this package.
pub struct Package;

impl Package {
    pub fn at(addr: Address) -> PackageAt { PackageAt { addr } }
}

pub struct PackageAt { addr: Address }

impl PackageAt {
    pub fn counter(&self) -> counter::ModuleAt { /* … */ }
    pub fn inbox(&self) -> inbox::ModuleAt   { /* … */ }
    // …
}
```

Two things to note for now:

- **No `PACKAGE_ID` constant.** The on-chain address the package
  is published at gets bound at runtime — same generated crate
  works on mainnet, testnet, your own deployment.
- **`pub struct Package;`** is a zero-sized marker. It's what
  `PtbBuilder::with_package::<Package>(addr)` keys on (next
  chapter) and what `PackageRegistry::at::<Package>(addr)` uses
  for read-only callers without a `PtbBuilder`.

### A module: counter.rs

The interesting one. Trimmed for the basics:

```rust,ignore
pub struct Counter { pub id: UID, pub value: u64, pub target: u64 }

impl MoveType for Counter {
    type Package = super::Package;
    const MODULE: &'static str = "counter";
    const NAME:   &'static str = "Counter";
}

pub trait ArgumentCounter: PTBArgument { /* … */ }
impl ArgumentCounter for Argument             {}
impl ArgumentCounter for ObjectId             { /* cache-aware override */ }
impl ArgumentCounter for ObjectReference      {}
impl ArgumentCounter for Shared<ObjectId>     {}
impl ArgumentCounter for SharedMut<ObjectId>  {}

pub async fn increment(
    b: &mut PtbBuilder,
    arg0: impl ArgumentCounter,
    by:   u64,
) -> Argument {
    let a0 = arg0.into_argument(b).await;
    let a1 = by.into_argument(b).await;
    b.move_call(
        b.package_id::<super::Package>(),
        "counter",
        "increment",
        Vec::new(),
        vec![a0, a1],
    )
}
```

Three patterns repeat for every Move datatype:

1. **The struct/enum**, with serde derives so it BCS-encodes
   identically to Move's wire format. No hand-written
   `Serialize`/`Deserialize`.
2. **A `MoveType` impl** that pins the type to a package marker and
   declares its Move name. The default `type_tag` body looks the
   package address up in the supplied `PackageAddrs`-implementor
   (the builder or a `PackageRegistry`) and constructs a
   `TypeTag::Struct`. No address is baked at codegen time.
3. **An `ArgumentX` trait** listing every Rust shape that can be
   passed for a `&Counter` parameter — `Argument` (handle from a
   previous call), `ObjectId` (resolved against the builder's
   object cache), `ObjectReference` (fully resolved), and the
   `Shared` / `SharedMut` / `Receiving` wrappers. Generated call
   builders take `impl ArgumentX` so users pass whichever shape
   they already have.

For every public Move function, codegen emits one `async fn` with
the right argument bounds and return shape. `increment` above is
the simplest case — single `&mut Counter` + a `u64` + no return.
The other variants (multi-return tuples, generics, `entry`-only
functions) are documented further along in the book.

## What's next

You have a generated crate. You haven't called any functions yet —
that's the next chapter, where we build a PTB offline, register a
package address, and look at what the resulting transaction
contains.

→ **[Calling a function](calling-a-function.md)**
