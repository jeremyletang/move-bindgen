/// Generic-container fixtures for the bindings generator.
///
/// Covers: phantom type parameters, ability-bounded type parameters
/// (`T: store`), well-known `Option<T>` and `vector<T>` from the standard
/// library, and a multi-value tuple return.
module counter_iota::registry;

/// Shared registry parameterised over a phantom marker type. The phantom
/// `T` carries no runtime data — codegen has to emit a `PhantomData<T>`
/// field with `#[serde(skip)]` to keep Rust happy.
public struct Registry<phantom T> has key, store {
    /// Unique identifier; assigned by `object::new` in [`new_registry`].
    id: UID,
    /// Maximum number of items this registry is willing to hold.
    capacity: u64,
}

/// Wrapper that owns a `vector<T>` plus an optional pointer to the last
/// inserted slot. `T` is required to have `store` so the holder itself
/// can have `store`.
public struct Holder<T: store> has store {
    /// Backing storage for the held items.
    items: vector<T>,
    /// Index of the most-recently-inserted item, if any.
    last: Option<u64>,
}

/// Construct a fresh, empty [`Registry<T>`] with the given capacity.
public fun new_registry<T>(capacity: u64, ctx: &mut TxContext): Registry<T> {
    Registry { id: object::new(ctx), capacity }
}

/// Read the configured capacity of `r`.
public fun capacity<T>(r: &Registry<T>): u64 {
    r.capacity
}

/// Construct an empty [`Holder<T>`] (no items, `last = None`).
public fun new_holder<T: store>(): Holder<T> {
    Holder { items: vector[], last: option::none() }
}

/// `Holder<0x1::string::String>` constructor — exercises the
/// bug-triggering shape from [BUGS/move-bindgen-string-typetag.md]:
/// a generic-T function instantiated at `String` from generated
/// Rust. The on-chain VM checks struct identity at the generic
/// position, so the call's `type_args[0]` must be
/// `TypeTag::Struct(0x1::string::String)`, not `Vector(U8)`.
public fun new_string_holder(): Holder<std::string::String> {
    new_holder<std::string::String>()
}

/// `Holder<0x1::ascii::String>` constructor — same idea as
/// [`new_string_holder`], but for `0x1::ascii::String`. Catches the
/// codegen path that conflates `string` and `ascii` (they share BCS
/// wire format but have distinct on-chain `TypeTag`s).
public fun new_ascii_holder(): Holder<std::ascii::String> {
    new_holder<std::ascii::String>()
}

/// Generic identity-like fixture whose only purpose is to exercise
/// generic-position `TypeTag`s on chain *and* return something
/// droppable so the PTB doesn't need a custom destructor. The Move VM
/// still checks the runtime `TypeTag` of `T` at the call site, so
/// passing the wrong tag (e.g. `vector<u8>` instead of
/// `0x1::string::String`) aborts with `TypeMismatch` — exactly the
/// signal the testnet smoke needs.
public fun probe_type<T>(): bool {
    true
}

/// Two-value return for codegen: returns `(0, @0x0)` — used to confirm
/// tuple-return shape in the generated Rust.
public fun stats(_addr: address): (u64, address) {
    (0, @0x0)
}
