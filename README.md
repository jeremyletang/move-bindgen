# move-bindgen

Generate Rust bindings for a Move package — types, PTB call builders, and
dev-inspect "view" calls — from sources, the way `abigen` does for Solidity.

> Status: early. The build pipeline and IR walk are in place; codegen is
> being added next. Targets the IOTA Move flavour.

## Quickstart

```sh
# Build the CLI
cargo build -p move-bindgen-cli

# Inspect the IR for the example package
./target/debug/move-bindgen dump examples/counter
```

The example under `examples/counter/` is a small multi-module package
covering structs, enums, generics with phantom and ability bounds,
constants, references, and the four function visibilities — used to
exercise codegen.

## Workspace

| Crate                  | What it is                                         |
| ---------------------- | -------------------------------------------------- |
| `move-bindgen`         | Library: build a package, walk the IR, codegen     |
| `move-bindgen-cli`     | `move-bindgen` binary                              |
| `move-bindgen-runtime` | Runtime support consumed by generated code        |

## Dependencies

The build chain is wired against the IOTA monorepo via pinned git deps in
the workspace `Cargo.toml`. To iterate on the IOTA crates locally,
uncomment the `[patch."https://github.com/iotaledger/iota.git"]` block at
the bottom of the workspace manifest and point it at your checkout.

## License

Apache-2.0
