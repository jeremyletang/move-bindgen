# move-bindgen

Generate Rust bindings for a Move package — types, PTB call builders, and
dev-inspect "view" calls — from sources, the way `abigen` does for Solidity.

> Status: early. The build pipeline and IR walk are in place; codegen
> is wired for both IOTA and Sui Move flavours.

## Quickstart

```sh
# Build the CLI
cargo build -p move-bindgen-cli

# Inspect the IR for the example package
./target/debug/move-bindgen dump packages/counter-iota
```

The example under `packages/counter-iota/` is a small multi-module
package covering structs, enums, generics with phantom and ability
bounds, constants, references, and the four function visibilities —
used to exercise codegen. `packages/counter-sui/` is the Sui-flavoured
sibling.

## Workspace

| Crate                       | What it is                                              |
| --------------------------- | ------------------------------------------------------- |
| `move-bindgen`              | Library: build a package, walk the IR, codegen          |
| `move-bindgen-cli`          | `move-bindgen` binary (`dump`, `generate`, `check`)     |
| `move-bindgen-runtime-iota` | IOTA-flavour runtime support consumed by generated code |
| `move-bindgen-runtime-sui`  | Sui-flavour runtime support consumed by generated code  |
| `move-bindgen-ext-iota`     | IOTA backend traits + GraphQL-client integration        |
| `move-bindgen-ext-sui`      | Sui backend traits + GraphQL-client integration         |

## Repository layout

| Path          | What's there                                              |
| ------------- | --------------------------------------------------------- |
| `crates/`     | The tooling itself                                                     |
| `packages/`   | Committed Move source packages used as test inputs                     |
| `configs/`    | Committed `move-bindgen.toml` files driving end-to-end runs            |
| `generated/`  | **Gitignored.** `move-bindgen generate` writes output here              |
| `tests/`      | Committed Rust crates that exercise the generated bindings              |

**What's committed vs not:** everything except `generated/`. Configs are
hand-written and version-controlled; generated bindings are derived from
configs + Move sources and rebuilt on demand.

## Dependencies

The build chain is wired against the IOTA monorepo via pinned git deps in
the workspace `Cargo.toml`. To iterate on the IOTA crates locally,
uncomment the `[patch."https://github.com/iotaledger/iota.git"]` block at
the bottom of the workspace manifest and point it at your checkout.

## License

Apache-2.0
