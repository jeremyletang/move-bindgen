/// Generics with phantom and ability-bounded type parameters, plus a
/// well-known `Option<T>`, a `vector<T>`, and a multi-value return.
module counter::registry;

public struct Registry<phantom T> has key, store {
    id: UID,
    capacity: u64,
}

public struct Holder<T: store> has store {
    items: vector<T>,
    last: Option<u64>,
}

public fun new_registry<T>(capacity: u64, ctx: &mut TxContext): Registry<T> {
    Registry { id: object::new(ctx), capacity }
}

public fun capacity<T>(r: &Registry<T>): u64 {
    r.capacity
}

public fun new_holder<T: store>(): Holder<T> {
    Holder { items: vector[], last: option::none() }
}

public fun stats(_addr: address): (u64, address) {
    (0, @0x0)
}
