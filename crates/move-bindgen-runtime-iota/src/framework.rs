//! `iota::object` framework types ([`ID`], [`UID`]) and the
//! `make_struct_tag` helper. Hardcoded to the IOTA framework address
//! (`0x2`) — these are generated-code targets, not user types.

use iota_sdk_transaction_builder::types::MoveArg;
use iota_sdk_transaction_builder::PureBytes;
use serde::{Deserialize, Serialize};

use crate::{Address, Identifier, MoveType, NoPackage, ObjectId, PackageAddrs, StructTag, TypeTag};

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
/// Panics if `module`/`name` aren't valid Move identifiers — only generated
/// code calls this, with identifiers from already-compiled bytecode.
pub fn make_struct_tag(addr: Address, module: &str, name: &str, params: Vec<TypeTag>) -> TypeTag {
    TypeTag::Struct(Box::new(StructTag::new(
        addr,
        Identifier::new(module).expect("static module name is a valid Move identifier"),
        Identifier::new(name).expect("static type name is a valid Move identifier"),
        params,
    )))
}

impl MoveType for ID {
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "ID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "UID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(IOTA_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}

// `ID` is `copy + drop + store` in Move — pass-by-value as a Move-call arg
// works. Implementing `MoveArg` makes `PTBArgument for ID` available via the
// SDK's blanket, and `PureID` can route through `apply_argument`.
impl MoveArg for ID {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("bcs serialization of ID never fails"))
    }
}

impl From<Address> for ID {
    fn from(bytes: Address) -> Self {
        Self { bytes }
    }
}

impl From<ObjectId> for ID {
    fn from(id: ObjectId) -> Self {
        Self {
            bytes: Address::from(id),
        }
    }
}
