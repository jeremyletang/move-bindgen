//! [`WorkItem`] — one entry on the install worklist: a package source
//! plus enough provenance to render diagnostics.

use crate::config::PackageSource;

/// One unit of staging work. Listed entries from `move-bindgen.toml`
/// carry their explicit id + crate-name override; transitively-discovered
/// entries set `id = None` and rely on the staging basename.
pub(super) struct WorkItem {
    pub(super) id: Option<String>,
    pub(super) source: PackageSource,
    pub(super) crate_name_override: Option<String>,
    /// Human-readable provenance string, surfaced in error messages so
    /// the user can trace e.g. a basename collision back to the dep
    /// edge that introduced it.
    pub(super) origin: String,
}

impl WorkItem {
    pub(super) fn label(&self) -> String {
        self.id.clone().unwrap_or_else(|| self.origin.clone())
    }
}
