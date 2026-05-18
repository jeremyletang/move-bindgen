/// Generic-container fixtures for the bindings generator.
///
/// Covers: phantom type parameters, ability-bounded type parameters
/// (`T: store`), well-known `Option<T>` and `vector<T>` from the standard
/// library, and a multi-value tuple return.
module counter_sui::registry;

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

/// Two-value return for codegen: returns `(0, @0x0)` — used to confirm
/// tuple-return shape in the generated Rust.
public fun stats(_addr: address): (u64, address) {
    (0, @0x0)
}
