# Type mapping

How Move types translate to Rust in the generated bindings.

`move-bindgen` has two distinct mapping tables — one for
**type positions** (field types, return types, generic instantiations)
and one for **argument positions** (PTB call builder parameters).
This chapter covers the first; the next chapter covers the second.

## Type position

The Rust type that gets emitted for a struct field, an enum
variant, a function return, or anywhere else a Move type appears
in value position.

| Move                                | Rust                                                          |
| ----------------------------------- | ------------------------------------------------------------- |
| `bool`                              | `bool`                                                        |
| `u8` / `u16` / `u32` / `u64` / `u128` | matching Rust integer                                       |
| `u256`                              | `U256` (re-exported `primitive_types::U256`)                  |
| `address`                           | `Address`                                                     |
| `vector<T>`                         | `Vec<T>`                                                      |
| `Option<T>` *(well-known `0x1::option::Option`)*       | `Option<T>`                                |
| `String` / `String::Ascii` *(well-known `0x1::string` / `0x1::ascii`)* | `String`                                |
| `0x2::object::UID` *(well-known)*  | `UID` (runtime-provided)                                       |
| `0x2::object::ID` *(well-known)*   | `ID` (runtime-provided)                                        |
| Same-package, same-module datatype  | bare `Type`                                                   |
| Same-package, sibling-module        | `super::module::Type`                                         |
| Peer-package datatype (workspace mode) | `::peer_crate::module::Type`                                |
| Generic param `T`                   | `T` (with `T: MoveType` bound on the impl)                    |
| Phantom param `T`                   | `PhantomData<T>` field with `#[serde(skip)]`                  |
| `signer`                            | error — `signer` isn't supported on IOTA/Sui PTB inputs       |
| External-package datatype with no peer entry | error at codegen — "add it to `[packages.*]`"        |

Three notes worth filing away:

**u256 has custom serde.** The default `primitive_types::U256`
encoding via `impl-serde` is a hex string — doesn't match Move's
32-LE-bytes BCS wire format. Codegen attaches
`#[serde(with = "move_bindgen_runtime::u256_le")]` to every
U256 field so BCS round-trips correctly.

**Phantom generics get a `PhantomData<T>` field.** Move's
`phantom T` means the parameter doesn't appear in any field but
still tracks identity. To keep the Rust type carrying the same
`T`, codegen synthesises a `#[serde(skip)] _phantom_t0:
PhantomData<T0>` field. Same trick is used for unused
non-phantom generics if needed — Rust requires every type
parameter to appear somewhere.

**References (`&T` / `&mut T`) don't exist in value position.**
Move's references are a borrow-checker artefact, erased before
codegen looks at the type. You'll see them in *argument*
position (a function takes `&mut Counter`), but never as field
types or return types.

## Move primitives and their TypeTag

For generic Move calls that take type arguments, codegen emits
`<T as MoveType>::type_tag(b)` to construct the `TypeTag` at
the call site. The runtime ships `MoveType` impls for every
primitive:

| Rust                | TypeTag         | Notes                                                |
| ------------------- | --------------- | ---------------------------------------------------- |
| `bool`              | `TypeTag::Bool` |                                                      |
| `u8` … `u128`       | matching variant|                                                      |
| `U256`              | `TypeTag::U256` |                                                      |
| `Address`           | `TypeTag::Address`|                                                    |
| `String`            | `TypeTag::Vector(TypeTag::U8)` | Move's `String` is `vector<u8>` on the wire |
| `Vec<T>`            | `TypeTag::Vector(T::type_tag())` | recurses into `T`                          |
| `Option<T>`         | `TypeTag::Struct(0x1::option::Option<T>)` | the well-known struct shape       |
| `ID`                | `TypeTag::Struct(0x2::object::ID)` | hardcoded framework address              |
| `UID`               | `TypeTag::Struct(0x2::object::UID)` | hardcoded framework address             |
| Generated datatype  | `make_struct_tag(addr, module, name, params)` | `addr` from `PackageAddrs`     |

The generated datatype case is the only one that consults the
runtime address map — everything above it is constant. That's
why primitives ignore the `&impl PackageAddrs` argument: their
tag doesn't depend on a runtime address.

## What's *not* emitted

A few Move shapes don't get bindings:

- **`init` functions.** Called by the framework on package
  publish; not externally callable, so codegen skips them.
- **`Friend` / `public(package)` functions.** Not callable from
  off-chain code; skipped.
- **Test-only items (`#[test]`, `#[test_only]`).** Compiled out
  by the build config (`dev_mode = false` by default).
- **Native functions without a corresponding Move-side signature.**
  Anything the build chain rejects upstream doesn't make it to
  codegen.

## Where to look next

- **[Argument traits](argument-traits.md)** — the *other* mapping,
  what shapes you can pass into a generated call builder.
- `packages/counter-iota/sources/registry.move` — exercises
  phantom generics, ability bounds, well-known `Option<T>`, and
  multi-value returns. The IR dump
  (`move-bindgen dump packages/counter-iota`) shows exactly what
  codegen consumes.
- `crates/move-bindgen/src/codegen/ty.rs` — the actual mapping
  function, including the well-known-type short-circuits for
  `Option`/`String`/`UID`/`ID`.
