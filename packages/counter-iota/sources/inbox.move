/// Enum codegen fixture — hits each of the three variant shapes Move
/// supports: a unit variant, a positional variant, and a named-fields
/// variant. The bindings generator emits a Rust `enum` with the same
/// three shapes plus serde derives.
module counter_iota::inbox;

/// A small message envelope used to exercise enum codegen. Has
/// `copy + drop + store` so it can be passed by value as a PTB arg or
/// stored in another struct.
public enum Message has copy, drop, store {
    /// Unit variant — no payload.
    Empty,
    /// Positional variant — a single byte-vector payload.
    Text(vector<u8>),
    /// Named-fields variant — used to test that codegen emits the same
    /// field names and order in Rust.
    Tagged {
        /// Free-form numeric tag.
        count: u64,
        /// Free-form byte-vector label.
        label: vector<u8>,
    },
}

/// Build a [`Message::Text`] from raw bytes.
public fun new_text(s: vector<u8>): Message {
    Message::Text(s)
}

/// Build a [`Message::Tagged`] with the given `count` and `label`.
public fun new_tagged(count: u64, label: vector<u8>): Message {
    Message::Tagged { count, label }
}

/// `true` iff `m` is the [`Message::Empty`] variant.
public fun is_empty(m: &Message): bool {
    match (m) {
        Message::Empty => true,
        _ => false,
    }
}
