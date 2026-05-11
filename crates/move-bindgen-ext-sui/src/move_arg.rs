//! `MoveArg` — BCS-encode a value into pure-input bytes for PTB
//! `Input::Pure` slots. Generated codegen emits `MoveArg` for every
//! non-`key` datatype; the blanket `PTBArgument for T: MoveArg` impl
//! in [`crate::input_kind`] routes them through `Pure` inputs.

use serde::Serialize;
use sui_sdk_types::Address;

use crate::U256;

/// Pure (BCS-serialised) bytes destined for a PTB `Input::Pure` slot.
#[derive(Clone, Debug, Default)]
pub struct PureBytes(pub Vec<u8>);

/// Convert a value to its BCS-pure-bytes representation. Codegen emits
/// `MoveArg` for every non-`key` datatype; the blanket `PTBArgument`
/// impl below routes `MoveArg` values through `Pure` inputs.
pub trait MoveArg: Sized {
    fn pure_bytes(self) -> PureBytes;
}

impl MoveArg for PureBytes {
    fn pure_bytes(self) -> PureBytes {
        self
    }
}

/// Convenience helper for tests/call-site code that wants to BCS-encode
/// an arbitrary `Serialize` value into `PureBytes`.
pub fn pure_bytes_of<T: Serialize>(v: &T) -> PureBytes {
    PureBytes(bcs::to_bytes(v).expect("BCS serialization should not fail"))
}

// `MoveArg` impls for the Move primitive types. Each delegates to
// `bcs::to_bytes` — Move's wire format is BCS, and serde+BCS agree
// for these types. Generated codegen-emitted `MoveArg` impls for user
// datatypes follow the same pattern.
//
// `U256` is a special case: `primitive_types::U256`'s default
// `impl-serde` encoding is a hex string, which doesn't match Move's
// 32-LE-bytes wire format. Push the bytes directly.
macro_rules! impl_move_arg_via_bcs {
    ($($ty:ty),* $(,)?) => {
        $(
            impl MoveArg for $ty {
                fn pure_bytes(self) -> PureBytes {
                    PureBytes(bcs::to_bytes(&self).expect("BCS serialization should not fail"))
                }
            }
        )*
    };
}

impl_move_arg_via_bcs!(bool, u8, u16, u32, u64, u128, Address, String);

impl MoveArg for U256 {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(self.to_little_endian().to_vec())
    }
}

impl<T: MoveArg + Serialize> MoveArg for Vec<T> {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("BCS serialization should not fail"))
    }
}

impl<T: MoveArg + Serialize> MoveArg for Option<T> {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("BCS serialization should not fail"))
    }
}
