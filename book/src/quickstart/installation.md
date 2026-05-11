# Installation

`move-bindgen` is currently distributed as source. You build the CLI
binary from a checkout of the repo and use it from there. A future
release will ship to crates.io and as prebuilt binaries; for now,
expect to keep the repo around.

## Prerequisites

- **Rust 1.95** — the workspace pins this in `rust-toolchain.toml`,
  so [rustup](https://rustup.rs) will install it the first time you
  `cargo build` inside the repo. No manual install needed.
- **Git** — used by `move-bindgen install` to fetch Move source
  packages from git URLs (`mystenlabs/sui`, your own Move
  monorepo, etc.).
- **A few GB of free disk** — first build pulls the IOTA and Sui
  build chains as git deps; the `~/.move/` cache also fills up with
  cloned framework repos.

## Building the CLI

```sh
git clone https://github.com/jeremyletang/move-bindgen
cd move-bindgen
cargo build -p move-bindgen-cli
```

The binary lands at `target/debug/move-bindgen`. The repo's smoke
commands assume that path; an alias or symlink onto your `$PATH`
is convenient but optional.

The first build is slow — it has to compile the Move toolchain
crates for both flavours. Expect 5–10 minutes on a fresh checkout
on a modern laptop. Subsequent builds are fast (seconds).

## Quick sanity check

```sh
./target/debug/move-bindgen --help
```

You should see the top-level subcommand list (`init`, `install`,
`generate`, `build`, `check`, `clean`, `dump`). Run any subcommand
with `--help` to see its options; `--help` shows the full doc,
`-h` the one-line summary.

## What's next

You have a working CLI. The next chapter walks through generating
bindings for the example counter package — no PTBs or signers yet,
just `init → install → generate` and a look at what came out.

→ **[Your first bindings](your-first-bindings.md)**
