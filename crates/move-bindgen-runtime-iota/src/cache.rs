//! Per-`ObjectId` ownership info `PtbBuilder` carries between calls so
//! generated code can pass bare ids and have them resolve to the
//! correct `Input` variant.

use std::collections::HashMap;

use crate::{ObjectId, ObjectReference};

/// Cached info for a known shared object.
#[derive(Copy, Clone, Debug)]
pub struct SharedObjectInfo {
    pub initial_shared_version: u64,
    pub mutable: bool,
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
