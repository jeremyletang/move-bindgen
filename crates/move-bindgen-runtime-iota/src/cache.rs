//! Per-`ObjectId` ownership info `PtbBuilder` carries between calls so
//! generated code can pass bare ids and have them resolve to the
//! correct `Input` variant.

use std::collections::HashMap;

use crate::{ObjectId, ObjectReference};

/// Cached info for a known shared object. Mutability is *not* stored —
/// codegen routes `&T` / `&mut T` parameters through `Shared(_)` /
/// `SharedMut(_)` wrappers at the call site, so the on-chain
/// shared-input lock comes from the Move signature instead of a
/// per-cache default. Manual `resolve_object_shared(id, mutable)`
/// callers still get to pick.
#[derive(Copy, Clone, Debug)]
pub struct SharedObjectInfo {
    pub initial_shared_version: u64,
}

/// Per-`ObjectId` ownership info that [`crate::PtbBuilder`] consults
/// to pick the right `Input` variant. Returned by
/// [`crate::PtbBuilder::execute`] so callers can carry it into the
/// next builder via [`crate::PtbBuilder::with_cache`].
#[derive(Clone, Debug, Default)]
pub struct ObjectCache {
    pub owned: HashMap<ObjectId, ObjectReference>,
    pub shared: HashMap<ObjectId, SharedObjectInfo>,
}

impl ObjectCache {
    pub fn new() -> Self {
        Self::default()
    }
}
