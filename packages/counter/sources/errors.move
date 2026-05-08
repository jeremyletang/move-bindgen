/// Error-constants codegen fixture.
///
/// Move has two flavours of error code: the classic `u64` form used as
/// the second argument to `assert!` / `abort`, and the Move 2024
/// `#[error]`-annotated string form which carries a human-readable
/// message. The bindings generator surfaces both.
module counter::errors;

/// Caller is not the registered owner.
const E_NOT_OWNER: u64 = 0;
/// Value exceeds the configured maximum.
const E_VALUE_TOO_LARGE: u64 = 1;

/// Move 2024 `#[error]`-annotated abort: emitted when a contract-level
/// initialisation step has not yet been performed.
#[error]
const ENotInitialized: vector<u8> = b"contract is not initialized";

/// Abort with [`E_NOT_OWNER`] unless `actual == expected`.
public fun ensure_owner(actual: address, expected: address) {
    assert!(actual == expected, E_NOT_OWNER);
}

/// Abort with [`E_VALUE_TOO_LARGE`] unless `v <= max`.
public fun ensure_in_range(v: u64, max: u64) {
    assert!(v <= max, E_VALUE_TOO_LARGE);
}

/// Abort with [`ENotInitialized`] (the Move 2024 string form) unless
/// `initialized` is `true`.
public fun assert_initialized(initialized: bool) {
    assert!(initialized, ENotInitialized);
}
