/// Enum with the three variant shapes Move supports: unit, positional,
/// named-fields. Used to test enum codegen.
module counter::inbox;

public enum Message has copy, drop, store {
    Empty,
    Text(vector<u8>),
    Tagged { count: u64, label: vector<u8> },
}

public fun new_text(s: vector<u8>): Message {
    Message::Text(s)
}

public fun new_tagged(count: u64, label: vector<u8>): Message {
    Message::Tagged { count, label }
}

public fun is_empty(m: &Message): bool {
    match (m) {
        Message::Empty => true,
        _ => false,
    }
}
