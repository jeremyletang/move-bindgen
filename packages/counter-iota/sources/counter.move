/// A small counter contract that exists purely as a codegen fixture.
///
/// It deliberately covers the features the bindings generator needs to
/// handle: a `key + store` object, a capability pattern, primitive args
/// (`u64`, `u256`), an `ID` value param, references (`&` and `&mut`),
/// `entry` and `public(package)` visibility, and a private helper.
///
/// The `Counter` shared object is created up front via [`create`] (which
/// also mints an [`AdminCap`] for privileged operations) and then mutated
/// by the various `increment*` / `set_target` / `reset` entry points.
module counter_iota::counter;

use iota::dynamic_field;
use iota::event;

/// Maximum amount any single [`increment`] call can add. Calls beyond this
/// are rejected with abort code `0`.
const MAX_INCREMENT: u64 = 1_000_000;

/// Event emitted by [`increment`] / [`increment_entry`] each time the
/// counter's `value` is bumped. Carries the new value plus the address
/// of whoever made the call. Used as a fixture for typed-event readers.
public struct Bumped has copy, drop {
    /// Object id of the counter that was bumped.
    counter: ID,
    /// New value of the counter after the increment.
    value: u64,
    /// Sender that made the call.
    by: address,
}

/// Dynamic-field key fixture: a `u64` slot id under which a [`Note`] is
/// stored. Has `copy + drop + store` so it can be used as a `Name`.
public struct NoteKey has copy, drop, store {
    /// User-chosen slot id.
    slot: u64,
}

/// Dynamic-field value fixture: an arbitrary string note attached under
/// a [`NoteKey`]. Has `store` so it can live in a dynamic field.
public struct Note has copy, drop, store {
    /// Free-form content set by [`set_note`].
    text: vector<u8>,
}

/// A shared counter — incrementable by anyone, resettable only by the
/// holder of the matching [`AdminCap`].
public struct Counter has key, store {
    /// Unique identifier; assigned by `object::new` in [`create`].
    id: UID,
    /// Address that originally created this counter.
    owner: address,
    /// Current count. Bumped by [`increment`], cleared by [`reset`].
    value: u64,
    /// Wide-int counter, exercised separately so the codegen has a `u256`
    /// field to wire up.
    big_value: u256,
    /// Optional reference to another on-chain object (e.g. the next
    /// counter in a chain). Set via [`set_target`].
    target: Option<ID>,
}

/// Capability granting privileged operations on a [`Counter`]. Minted once
/// per [`Counter`] in [`create`] and transferred to the caller.
public struct AdminCap has key, store {
    /// Unique identifier of this capability.
    id: UID,
}

/// Create a shared [`Counter`] (initial state: all zeroes, no `target`)
/// and return the matching [`AdminCap`] to the caller.
///
/// The returned cap must be transferred or stored by the calling
/// transaction — it has `key + store` and no `drop`.
public fun create(ctx: &mut TxContext): AdminCap {
    transfer::share_object(Counter {
        id: object::new(ctx),
        owner: ctx.sender(),
        value: 0,
        big_value: 0,
        target: option::none(),
    });
    AdminCap { id: object::new(ctx) }
}

/// Add `by` to `c.value`. Aborts with code `0` if `by > MAX_INCREMENT`.
/// Emits a [`Bumped`] event with the new value and the caller's address.
public fun increment(c: &mut Counter, by: u64, ctx: &TxContext) {
    assert!(by <= MAX_INCREMENT, 0);
    c.value = c.value + by;
    event::emit(Bumped {
        counter: c.id.to_inner(),
        value: c.value,
        by: ctx.sender(),
    });
}

/// Attach (or overwrite) a [`Note`] under `slot` on `c`. Stored as a
/// `iota::dynamic_field` keyed by [`NoteKey`].
public fun set_note(c: &mut Counter, slot: u64, text: vector<u8>) {
    let key = NoteKey { slot };
    if (dynamic_field::exists_<NoteKey>(&c.id, key)) {
        let n = dynamic_field::borrow_mut<NoteKey, Note>(&mut c.id, key);
        n.text = text;
    } else {
        dynamic_field::add<NoteKey, Note>(&mut c.id, key, Note { text });
    }
}

/// Add `by` to `c.big_value`. No upper bound — `u256` overflow aborts as
/// usual.
public fun increment_big(c: &mut Counter, by: u256) {
    c.big_value = c.big_value + by;
}

/// Replace `c.target` with `Some(target)`. There is no clear-target
/// helper; pass a sentinel `ID` if you need one.
public fun set_target(c: &mut Counter, target: ID) {
    c.target = option::some(target);
}

/// `entry`-only wrapper around [`increment`] for callers that can't make
/// `MoveCall`s through a PTB.
public entry fun increment_entry(c: &mut Counter, by: u64, ctx: &TxContext) {
    increment(c, by, ctx);
}

/// Read the current `value`. Provided as a non-entry `public` fun so
/// dev-inspect can surface it without a transaction.
public fun value(c: &Counter): u64 {
    c.value
}

/// Read the address that created this counter.
public fun owner(c: &Counter): address {
    c.owner
}

/// Reset `c.value` to zero. Requires the matching [`AdminCap`] — passing
/// any other capability fails type-checking on chain.
public fun reset(_cap: &AdminCap, c: &mut Counter) {
    c.value = 0;
}

/// `public(package)` visibility — callable from sibling modules in this
/// package only. The bindings generator skips these because external PTB
/// callers can't invoke them.
public(package) fun bump_one(c: &mut Counter, ctx: &TxContext) {
    c.value = c.value + 1;
    event::emit(Bumped {
        counter: c.id.to_inner(),
        value: c.value,
        by: ctx.sender(),
    });
}

/// Private helper kept around so codegen can confirm it correctly skips
/// non-public, non-entry functions.
#[allow(unused_function)]
fun internal_helper(): u64 {
    42
}
