/// Codegen fixture mirroring the IOTA `counter` package against Sui's
/// framework. Covers a shared `key + store` object, an admin
/// capability, primitive args (`u64`, `u256`), references, an `entry`
/// function, and an event the bindings generator can wire up.
module counter_sui::counter;

use sui::event;

/// Maximum amount any single `increment` call can add.
const MAX_INCREMENT: u64 = 1_000_000;

/// Event emitted on each `increment`. Carries the new value plus the
/// caller. Exists so the bindings generator has a typed event to wire
/// up.
public struct Bumped has copy, drop {
    counter: ID,
    value: u64,
    by: address,
}

/// Shared counter object. Stores the current value plus a "target"
/// callers can compare against.
public struct Counter has key, store {
    id: UID,
    value: u64,
    target: u64,
}

/// Capability minted on creation; required for `reset`.
public struct AdminCap has key, store {
    id: UID,
    counter: ID,
}

/// Create a fresh shared counter. Returns the `AdminCap` to the sender.
public fun create(target: u64, ctx: &mut TxContext): AdminCap {
    let c = Counter { id: object::new(ctx), value: 0, target };
    let cap = AdminCap { id: object::new(ctx), counter: object::id(&c) };
    transfer::share_object(c);
    cap
}

/// Bump the counter by `by`. Aborts if `by` is over the configured max.
public fun increment(c: &mut Counter, by: u64, ctx: &TxContext) {
    assert!(by <= MAX_INCREMENT, 0);
    c.value = c.value + by;
    event::emit(Bumped {
        counter: object::id(c),
        value: c.value,
        by: tx_context::sender(ctx),
    });
}

/// Read-only views — useful for dev-inspect codegen.
public fun value(c: &Counter): u64 { c.value }
public fun target(c: &Counter): u64 { c.target }

/// Multi-return fixture — exercises codegen's tuple-return path.
/// Returns `(value, target)` so callers can destructure both at once.
public fun snapshot(c: &Counter): (u64, u64) { (c.value, c.target) }

/// Wide-int parameter exists purely to exercise `u256` codegen.
public fun set_target_u256(c: &mut Counter, target: u256) {
    c.target = (target as u64);
}

/// Reset the counter to zero. Requires the `AdminCap`.
public fun reset(cap: &AdminCap, c: &mut Counter) {
    assert!(cap.counter == object::id(c), 1);
    c.value = 0;
}

/// Entry-fn variant of `increment` so we have an `entry` fixture.
entry fun increment_entry(c: &mut Counter, by: u64, ctx: &TxContext) {
    increment(c, by, ctx);
}
