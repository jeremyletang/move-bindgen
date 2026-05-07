//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Re-exports the small set of types generated code needs (so the user's
//! crate doesn't have to depend on `move-core-types` directly), defines the
//! `MoveType` trait, and provides impls for primitives and the well-known
//! framework types `UID` / `ID`.

use serde::{Deserialize, Serialize};

pub use move_core_types::{
    account_address::AccountAddress,
    language_storage::{StructTag, TypeTag},
};

/// Implemented by every Move type that can be referenced in a generated
/// signature. Used at call-time to fill in the type-argument slots of a
/// `MoveCall`.
pub trait MoveType {
    fn type_tag() -> TypeTag;
}

/// `iota::object::ID` — a 32-byte address wrapped in a struct.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ID {
    pub bytes: AccountAddress,
}

/// `iota::object::UID` — owns a single `ID`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UID {
    pub id: ID,
}

const IOTA_FRAMEWORK_ADDRESS: AccountAddress = {
    let mut bytes = [0u8; AccountAddress::LENGTH];
    bytes[AccountAddress::LENGTH - 1] = 0x02;
    AccountAddress::new(bytes)
};

const STD_FRAMEWORK_ADDRESS: AccountAddress = {
    let mut bytes = [0u8; AccountAddress::LENGTH];
    bytes[AccountAddress::LENGTH - 1] = 0x01;
    AccountAddress::new(bytes)
};

fn struct_tag(addr: AccountAddress, module: &str, name: &str, params: Vec<TypeTag>) -> TypeTag {
    use move_core_types::identifier::Identifier;
    TypeTag::Struct(Box::new(StructTag {
        address: addr,
        module: Identifier::new(module).expect("static module name is a valid Move identifier"),
        name: Identifier::new(name).expect("static type name is a valid Move identifier"),
        type_params: params,
    }))
}

macro_rules! impl_move_type_primitive {
    ($($t:ty => $tag:expr),* $(,)?) => {
        $(
            impl MoveType for $t {
                fn type_tag() -> TypeTag { $tag }
            }
        )*
    };
}

impl_move_type_primitive! {
    bool            => TypeTag::Bool,
    u8              => TypeTag::U8,
    u16             => TypeTag::U16,
    u32             => TypeTag::U32,
    u64             => TypeTag::U64,
    u128            => TypeTag::U128,
    AccountAddress  => TypeTag::Address,
}

impl<T: MoveType> MoveType for Vec<T> {
    fn type_tag() -> TypeTag {
        TypeTag::Vector(Box::new(T::type_tag()))
    }
}

impl<T: MoveType> MoveType for Option<T> {
    fn type_tag() -> TypeTag {
        struct_tag(
            STD_FRAMEWORK_ADDRESS,
            "option",
            "Option",
            vec![T::type_tag()],
        )
    }
}

impl MoveType for String {
    fn type_tag() -> TypeTag {
        struct_tag(STD_FRAMEWORK_ADDRESS, "string", "String", vec![])
    }
}

impl MoveType for ID {
    fn type_tag() -> TypeTag {
        struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    fn type_tag() -> TypeTag {
        struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}
