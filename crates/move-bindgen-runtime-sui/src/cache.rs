//! Per-`ObjectId` ownership info `PtbBuilder` carries between calls so
//! generated code can pass bare ids and have them resolve to the
//! correct `ObjectInput` constructor.

use std::collections::HashMap;

use crate::{ObjectId, ObjectReference};

/// Per-object metadata the builder needs to materialise an input.
/// Populated by the user via the `register_*` methods, or fetched
/// lazily by a `Fetcher` (added once a Sui-side fetcher impl lands).
#[derive(Clone, Debug)]
pub enum CachedObject {
    /// Owned — the builder needs the full ref.
    Owned(ObjectReference),
    /// Immutable — same shape as Owned, separate variant so the
    /// builder picks the right `ObjectInput` constructor.
    Immutable(ObjectReference),
    /// Shared — the builder needs the initial shared version + the
    /// per-call mutability flag (filled in at the call site by the
    /// `Shared<T>` / `SharedMut<T>` wrapper).
    Shared { initial_shared_version: u64 },
}

/// Cache of object metadata indexed by `ObjectId`. Lookups feed
/// `apply_argument` so generated code can pass bare ids.
#[derive(Clone, Debug, Default)]
pub struct ObjectCache {
    pub entries: HashMap<ObjectId, CachedObject>,
}

impl ObjectCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_owned(&mut self, id: ObjectId, reference: ObjectReference) {
        self.entries.insert(id, CachedObject::Owned(reference));
    }

    pub fn register_immutable(&mut self, id: ObjectId, reference: ObjectReference) {
        self.entries.insert(id, CachedObject::Immutable(reference));
    }

    pub fn register_shared(&mut self, id: ObjectId, initial_shared_version: u64) {
        self.entries.insert(
            id,
            CachedObject::Shared {
                initial_shared_version,
            },
        );
    }

    pub fn lookup(&self, id: &ObjectId) -> Option<&CachedObject> {
        self.entries.get(id)
    }
}
