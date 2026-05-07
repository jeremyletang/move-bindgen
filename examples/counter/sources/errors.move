/// Error constants — both the classic `u64` style used with `assert!`/`abort`
/// and the Move 2024 `#[error]`-annotated string form.
module counter::errors;

const E_NOT_OWNER: u64 = 0;
const E_VALUE_TOO_LARGE: u64 = 1;

#[error]
const ENotInitialized: vector<u8> = b"contract is not initialized";

public fun ensure_owner(actual: address, expected: address) {
    assert!(actual == expected, E_NOT_OWNER);
}

public fun ensure_in_range(v: u64, max: u64) {
    assert!(v <= max, E_VALUE_TOO_LARGE);
}

public fun assert_initialized(initialized: bool) {
    assert!(initialized, ENotInitialized);
}
