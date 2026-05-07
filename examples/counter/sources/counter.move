/// A `key + store` object plus a capability pattern.
/// Covers: object refs (`&` and `&mut`), primitive args, value return,
/// `entry` and `public(package)` visibility, a private helper, a constant.
module counter::counter;

const MAX_INCREMENT: u64 = 1_000_000;

public struct Counter has key, store {
    id: UID,
    owner: address,
    value: u64,
}

public struct AdminCap has key, store {
    id: UID,
}

public fun create(ctx: &mut TxContext): AdminCap {
    transfer::share_object(Counter {
        id: object::new(ctx),
        owner: ctx.sender(),
        value: 0,
    });
    AdminCap { id: object::new(ctx) }
}

public fun increment(c: &mut Counter, by: u64) {
    assert!(by <= MAX_INCREMENT, 0);
    c.value = c.value + by;
}

public entry fun increment_entry(c: &mut Counter, by: u64) {
    increment(c, by);
}

public fun value(c: &Counter): u64 {
    c.value
}

public fun owner(c: &Counter): address {
    c.owner
}

public fun reset(_cap: &AdminCap, c: &mut Counter) {
    c.value = 0;
}

public(package) fun bump_one(c: &mut Counter) {
    c.value = c.value + 1;
}

#[allow(unused_function)]
fun internal_helper(): u64 {
    42
}
