//! Runtime support consumed by `move-bindgen`-generated code.
//!
//! Re-exports the small set of SDK types generated code needs (so the user's
//! crate doesn't have to depend on `iota-sdk-types` directly), and defines
//! the well-known framework types `UID` / `ID`.

use serde::{Deserialize, Serialize};

pub use iota_sdk_transaction_builder::{
    PureBytes,
    types::{MoveArg, MoveType},
};
pub use iota_sdk_types::{Address, Identifier, ObjectId, StructTag, TypeTag};

/// `iota::object::ID` — a 32-byte address wrapped in a struct.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ID {
    pub bytes: Address,
}

/// `iota::object::UID` — owns a single `ID`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UID {
    pub id: ID,
}

const IOTA_FRAMEWORK_ADDRESS: Address = {
    let mut bytes = [0u8; 32];
    bytes[31] = 0x02;
    Address::new(bytes)
};

/// Build a `TypeTag::Struct` from string module/name + concrete type params.
///
/// Panics if `module` or `name` aren't valid Move identifiers — fine because
/// every caller is generated code baking in identifiers extracted from
/// already-compiled bytecode.
pub fn make_struct_tag(addr: Address, module: &str, name: &str, params: Vec<TypeTag>) -> TypeTag {
    TypeTag::Struct(Box::new(StructTag::new(
        addr,
        Identifier::new(module).expect("static module name is a valid Move identifier"),
        Identifier::new(name).expect("static type name is a valid Move identifier"),
        params,
    )))
}

impl MoveType for ID {
    fn type_tag() -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    fn type_tag() -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}
