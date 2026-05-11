# Multi-package workspaces

The quickstart bound a single Move package to a single Rust crate.
Real projects rarely look that clean — a DEX has its core protocol,
its token, its margin engine, its predict markets, plus framework
crates and Pyth oracles transitively pulled in. You want bindings
for all of them, with cross-package datatype references resolving
correctly, and you want one `cargo check` to type-check the lot.

`move-bindgen`'s **workspace mode** does this: one config lists
the root packages, auto-discovery walks each one's `Move.toml`
`[dependencies]` and pulls in transitives, and the output is a
Cargo workspace with one crate per Move package.

This chapter walks through `configs/deepbookv3.toml` — the
in-tree config that builds against MystenLabs's full DeepBook v3
deployment. By the end you'll have 12 generated Rust crates, all
compiling, all cross-referencing each other through a peer map.

## The shape of a workspace config

```toml
flavour = "sui"

# Override the default ["Iota", "MoveStdlib"]. We let auto-discovery
# pull in `sui` + `move_stdlib` as peer crates instead — codegen's
# well-known map still handles the basics (UID, ID, Option, String)
# via the runtime.
framework_packages = []

[output]
format  = "workspace"
name    = "deepbookv3-rs"
runtime = { path = "../crates/move-bindgen-runtime-sui" }

[packages.deepbook]
git    = "https://github.com/MystenLabs/deepbookv3.git"
rev    = "main"
subdir = "packages/deepbook"

[packages.deepbook_margin]
git    = "https://github.com/MystenLabs/deepbookv3.git"
rev    = "main"
subdir = "packages/deepbook_margin"

# ...one [packages.X] block per root package you want bindings for

[packages.sui]
git    = "https://github.com/MystenLabs/sui.git"
rev    = "c188832a5c2f8a48b9998e8c3888874a56bb1ca8"
subdir = "crates/sui-framework/packages/sui-framework"

[packages.move_stdlib]
git    = "https://github.com/MystenLabs/sui.git"
rev    = "c188832a5c2f8a48b9998e8c3888874a56bb1ca8"
subdir = "crates/sui-framework/packages/move-stdlib"
```

Three things changed from the single-package config:

- **`format = "workspace"`** — codegen now emits a Cargo workspace
  rather than a standalone crate.
- **`framework_packages = []`** — overrides the default
  framework-skip list. By default `move-bindgen` skips emitting
  bindings for `Sui` / `MoveStdlib` (their basics like `UID`,
  `ID`, `Option`, `String` are routed through the runtime). For
  DeepBook we want full bindings for both, so we override.
- **Multiple `[packages.X]` blocks** — each entry is a root that
  `move-bindgen` should generate bindings for. Auto-discovery
  walks each root's `Move.toml`-level `[dependencies]` and pulls
  in transitives; you don't list them explicitly unless they're
  reached via implicit-deps (see below).

## Sui frameworks have to be listed explicitly

Sui packages don't write `Sui` or `MoveStdlib` in their
`[dependencies]` block — they rely on `SuiFlavor`'s
implicit-dep injection at build time. Our auto-discovery only
walks declared dependencies, so we list both framework packages
explicitly as `[packages.sui]` / `[packages.move_stdlib]`.

The git rev pins them to the same Sui release the workspace's
`Cargo.toml` uses for the build chain (`mystenlabs/sui` at
`c188832a…`). Different revs would compile fine but generate
slightly different bindings; pinning to the same one as the build
chain is the boring safe choice.

The IOTA path doesn't need this — iota packages do list `Iota` /
`MoveStdlib` in their `Move.toml`, so auto-discovery picks them
up automatically. Setting `framework_packages = ["Iota",
"MoveStdlib"]` (the default) routes them through the runtime
instead of generating peer crates.

## Build it

```sh
move-bindgen build --config configs/deepbookv3.toml
```

`build` runs install + generate in one go. You'll see something
like:

```text
   Resolving configs/deepbookv3.toml
    Fetching https://github.com/MystenLabs/deepbookv3.git @ main
     Staging deepbook
    Fetching https://github.com/MystenLabs/deepbookv3.git @ main
     Staging deepbook_margin
    ...
    Fetching https://github.com/pyth-network/pyth-crosschain.git @ sui-contract-mainnet
     Staging Pyth
    Fetching https://github.com/pyth-network/pyth-crosschain.git @ sui-testnet
     Staging pyth_lazer
    Fetching https://github.com/wormhole-foundation/wormhole.git @ sui/mainnet
     Staging Wormhole
    Finished installing 18 package(s) in 6.57s
   Verifying deepbookv3.toml
   Compiling dbtc
   Compiling deepbook
   Compiling deepbook_margin
   ...
  Generating deepbookv3-rs (12 crates)
    Finished generating deepbookv3-rs in 34s
```

Several things worth noticing:

1. **Pyth and Wormhole came along for free.** Neither is listed
   in the config — they were discovered transitively by walking
   `predict`'s `Move.toml`, which dep-replaces `pyth_lazer`,
   which in turn depends on `wormhole`.
2. **18 packages staged → 12 crates generated.** The other six
   are duplicate framework references at different revs (e.g.
   different Pyth deployments pin slightly different Sui-framework
   revs). `move-bindgen` deduplicates them by address so the
   workspace has exactly one crate per unique on-chain package.
3. **No manual address bookkeeping.** Each generated crate emits
   its own `pub struct Package;` marker; call sites bind the
   on-chain address at runtime via `with_package`.

The output lands at `configs/deepbookv3-rs/`:

```text
configs/deepbookv3-rs/
├── Cargo.toml             # workspace, one [member] per crate
├── contracts-rs/          # Pyth (source subdir is `target_chains/sui/contracts`)
├── dbtc-rs/
├── deepbook-margin-rs/
├── deepbook-rs/
├── dusdc-rs/
├── margin-liquidation-rs/
├── move-stdlib-rs/
├── predict-rs/
├── sui-2-rs/              # pyth_lazer (its subdir is `lazer/contracts/sui`)
├── sui-framework-rs/
├── token-rs/
└── wormhole-rs/
```

Verify it type-checks:

```sh
(cd configs/deepbookv3-rs && cargo check)
```

That's the CI smoke-test for this config — it catches any
regression in cross-package resolution, framework address
handling, or the per-flavour runtime.

## Cross-package references

The interesting bit is what happens when one Move package
references a type from another. DeepBook's `predict` package has
a `lazer_helper` module that uses `pyth_lazer::update::Update`.
After codegen, `predict-rs/src/lazer_helper.rs` contains:

```rust,ignore
use sui_2_rs::update::Update;  // pyth_lazer resolved through the peer map
```

The "peer map" is `move-bindgen`'s internal directory of
`address → crate name`. When codegen sees a type reference whose
address isn't the current package's, it looks the address up in
this map and emits a `::peer_crate::module::Type` reference. If
the address isn't known, codegen errors with a clear "add it to
`[packages.*]`" message — that's how you discover missing roots.

You don't have to touch the peer map directly — it's built
automatically from the install manifest. It just controls how
generated cross-package references look in Rust.

## Using the workspace

The generated crates are normal Rust crates. Path-dep them in
your consumer:

```toml
# my-deepbook-app/Cargo.toml
[dependencies]
deepbook-rs          = { path = "../move-bindgen/configs/deepbookv3-rs/deepbook-rs" }
deepbook-margin-rs   = { path = "../move-bindgen/configs/deepbookv3-rs/deepbook-margin-rs" }
sui-framework-rs     = { path = "../move-bindgen/configs/deepbookv3-rs/sui-framework-rs" }
move-bindgen-runtime = { package = "move-bindgen-runtime-sui",
                         path = "../move-bindgen/crates/move-bindgen-runtime-sui" }
```

Register a runtime address for *every* package whose functions
you call:

```rust,ignore
let mut ptb = PtbBuilder::new(sender)
    .with_client(Client::new_testnet())
    .with_signer(signer)
    .with_auto_gas()
    .with_package::<deepbook_rs::Package>(DEEPBOOK_ADDR)
    .with_package::<deepbook_margin_rs::Package>(DEEPBOOK_MARGIN_ADDR);
```

If you call a function and forgot to register its package,
`PtbBuilder::package_id::<P>()` panics with a clear message
naming the unbound `Package` type — fix-and-go.

## The `framework_packages` knob

By default, `framework_packages = ["Iota", "MoveStdlib"]` (or
the Sui equivalent). Packages whose `[package].name` matches one
of those are **skipped** during codegen — no crate is emitted for
them, and references to their types route through the runtime
(`UID`, `ID`, `Option<T>`, `String` are runtime-provided).

Setting `framework_packages = []` flips this: every package gets
emitted as a peer crate, including the framework. The DeepBook
config does this because parts of DeepBook's API surface types
beyond the runtime's well-known set — `VecSet`, `Coin<T>`,
`Table<K, V>` — that the runtime can't provide directly.

Tradeoff: opting in to framework codegen means longer install +
generate times (the framework has ~hundred modules each), and the
generated framework crates have to be re-emitted every time you
bump the Sui rev. Worth it for breadth; not worth it if your code
only touches basics.

## Where to look next

- `configs/exchange.toml` — an IOTA workspace config; same shape,
  IOTA flavour, with `framework_packages = []`.
- `configs/pyth.toml` — minimal git-only workspace: one root,
  every transitive picked up automatically.
- The CI workflow at `.github/workflows/integration.yml` —
  `Smoke-test — DeepBook v3 workspace build` builds the
  deepbookv3 workspace and `cargo check`s it on every push.
