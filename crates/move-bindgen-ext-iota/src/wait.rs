//! Indexer-lag handling — [`ClientExt::wait_for_effects`] /
//! [`ClientExt::wait_for_object`] poll the GraphQL indexer until it
//! reflects the post-execution state recorded in a tx's
//! [`TransactionEffects`].
//!
//! Validators acknowledge a tx before the indexer has ingested the
//! checkpoint that contains it, so a `get_object` immediately after
//! `execute` can serve the *pre-tx* version. These helpers paper over
//! that gap.

use std::time::{Duration, Instant};

use iota_sdk_graphql_client::Client;
use iota_sdk_types::{ExecutionStatus, Object, ObjectId, ObjectOut, TransactionEffects, Version};

use crate::GetError;

#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    #[error("transaction did not succeed: {0:?}")]
    TxFailed(ExecutionStatus),
    #[error("object {0} is not in transaction effects")]
    NotInEffects(ObjectId),
    #[error("timed out waiting for {id} at version {expected}")]
    Timeout { id: ObjectId, expected: Version },
    #[error("client backend: {0}")]
    Backend(String),
    #[error(transparent)]
    Decode(#[from] GetError),
}

#[allow(clippy::result_large_err)]
pub(crate) fn require_success(effects: &TransactionEffects) -> Result<(), WaitError> {
    match effects.status() {
        ExecutionStatus::Success => Ok(()),
        other => Err(WaitError::TxFailed(other.clone())),
    }
}

/// `(id, expected indexer version)` for every non-deletion in `effects`.
/// `ObjectWrite` entries inherit `lamport_version`; `PackageWrite` carries
/// its own version. `Missing` (deletion/wrap) is skipped.
pub(crate) fn target_versions(effects: &TransactionEffects) -> Vec<(ObjectId, Version)> {
    let v1 = effects.as_v1();
    let lamport = v1.lamport_version;
    v1.changed_objects
        .iter()
        .filter_map(|ch| match &ch.output_state {
            ObjectOut::ObjectWrite { .. } => Some((ch.object_id, lamport)),
            ObjectOut::PackageWrite { version, .. } => Some((ch.object_id, *version)),
            ObjectOut::Missing => None,
            _ => None,
        })
        .collect()
}

pub(crate) async fn poll_for_version(
    client: &Client,
    id: ObjectId,
    version: Version,
    deadline: Instant,
    interval: Duration,
) -> Result<Object, WaitError> {
    loop {
        match client.object(id, Some(version)).await {
            Ok(Some(obj)) => return Ok(obj),
            Ok(None) => {}
            Err(e) => return Err(WaitError::Backend(e.to_string())),
        }
        if Instant::now() >= deadline {
            return Err(WaitError::Timeout {
                id,
                expected: version,
            });
        }
        tokio::time::sleep(interval).await;
    }
}
