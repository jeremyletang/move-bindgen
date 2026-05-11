//! `sui::object` framework types ([`ID`], [`UID`]) and the
//! `make_struct_tag` helper. Hardcoded to the Sui framework address
//! (`0x2`) — these are generated-code targets, not user types.

use serde::{Deserialize, Serialize};

use crate::{
    Address, Identifier, MoveArg, MoveType, NoPackage, PackageAddrs, PureBytes, StructTag, TypeTag,
};

/// Sui's framework `Sui`/`object`/etc. lives at the canonical 0x2
/// address. Generated `MoveType` impls for `ID`/`UID` reference this.
pub const SUI_FRAMEWORK_ADDRESS: Address = Address::TWO;

/// `sui::object::ID` — a 32-byte address wrapped in a struct so Move's
/// type system can distinguish "an object's identity" from a plain
/// `address`. BCS layout matches `Address`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ID {
    pub bytes: Address,
}

/// `sui::object::UID` — owns a single `ID`. `key`-only in Move
/// (no copy/drop), so it can't be passed by value at the PTB layer
/// the way `ID` can. Generated code treats this as a non-pure type.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct UID {
    pub id: ID,
}

/// Build a `TypeTag::Struct` from string module/name + concrete type
/// params. Panics if `module`/`name` aren't valid Move identifiers —
/// only generated code calls this, with identifiers from
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
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "ID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "ID", vec![])
    }
}

impl MoveType for UID {
    type Package = NoPackage;
    const MODULE: &'static str = "object";
    const NAME: &'static str = "UID";
    fn type_tag(_: &impl PackageAddrs) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
    fn type_tag_at(_: Address) -> TypeTag {
        make_struct_tag(SUI_FRAMEWORK_ADDRESS, "object", "UID", vec![])
    }
}

// `ID` is `copy + drop + store` in Move — it can be passed as a Move
// call arg by value. Implementing `MoveArg` opts it into the blanket
// `PTBArgument for T: MoveArg` impl in ext-sui so it BCS-encodes as a
// `Pure` input. `UID` is *not* MoveArg by design: it has no `drop` in
// Move and can't be constructed off-chain.
impl MoveArg for ID {
    fn pure_bytes(self) -> PureBytes {
        PureBytes(bcs::to_bytes(&self).expect("BCS serialization of ID never fails"))
    }
}

impl From<Address> for ID {
    fn from(bytes: Address) -> Self {
        Self { bytes }
    }
}
