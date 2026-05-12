//! `ClientExt` — typed read helpers on `sui_rpc::Client`. Mirrors the
//! iota-side surface (`get_object`, `get_objects`, `wait_for_*`) so
//! generated code and examples can target the same shape regardless
//! of flavour.

use std::time::{Duration, Instant};

use sui_rpc::field::{FieldMask, FieldMaskUtil};
use sui_rpc::proto::sui::rpc::v2 as proto;
use sui_rpc::Client;
use sui_sdk_types::{
    Address, ExecutionStatus, Object, ObjectOut, StructTag, TransactionEffects, TypeTag, Version,
};

use crate::{MoveType, ObjectId, PackageAddrs};

/// Read-mask paths that cover every field
/// `sui_sdk_types::Object::try_from(&proto::Object)` consults. Used by
/// the typed-read helpers below.
const OBJECT_READ_PATHS: [&str; 9] = [
    "object_id",
    "version",
    "digest",
    "owner",
    "object_type",
    "has_public_transfer",
    "contents",
    "previous_transaction",
    "storage_rebate",
];

fn object_read_mask() -> FieldMask {
    FieldMask::from_paths(OBJECT_READ_PATHS)
}

/// Indexer-poll defaults — duplicated from the iota crate's
/// [`crate::WaitOptions`] (which is re-exported but lives in
/// `ext-core`). Keeps the surface identical so codegen + examples can
/// be flavour-agnostic.
pub use crate::WaitOptions;

#[derive(Debug, thiserror::Error)]
pub enum GetError {
    #[error("client backend: {0}")]
    Backend(String),
    #[error("object {0} not found")]
    NotFound(ObjectId),
    #[error("object {id} is not a Move struct (looks like a package)")]
    NotAStruct { id: ObjectId },
    #[error("type mismatch for {id}: expected {expected}, got {actual}")]
    TypeMismatch {
        id: ObjectId,
        expected: Box<StructTag>,
        actual: Box<StructTag>,
    },
    #[error("bcs decode for {id}: {source}")]
    Bcs {
        id: ObjectId,
        #[source]
        source: bcs::Error,
    },
}

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

/// Typed read helpers on `sui_rpc::Client`.
#[allow(async_fn_in_trait)] // static dispatch only — never used as `dyn`
pub trait ClientExt {
    /// Fetch the object at `id` and BCS-decode its contents as `T`.
    /// Verifies the on-chain type matches `T`'s `TypeTag` (resolved
    /// against `addrs`) before decoding.
    async fn get_object<T>(&self, id: ObjectId, addrs: &impl PackageAddrs) -> Result<T, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// Batch variant of [`ClientExt::get_object`].
    async fn get_objects<T>(
        &self,
        ids: &[ObjectId],
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// Read the dynamic field at `(parent, key)` and decode its value
    /// as `V`. Both type tags resolve against `addrs`. Sui dynamic
    /// fields are stored as `0x2::dynamic_field::Field<K, V>` objects
    /// whose ids are derived from `hash(parent || key_bytes ||
    /// key_type_tag)`; this method derives the child id, fetches it,
    /// and extracts the `value: V` from the wrapper.
    ///
    /// Unlike the iota side, `K` must be `DeserializeOwned` too — Sui
    /// doesn't expose a typed `dynamic_field` lookup that returns the
    /// value bytes directly, so we deserialize the whole `Field<K, V>`
    /// wrapper to skip the `name: K` slot.
    async fn get_dynamic_field<K, V>(
        &self,
        parent: ObjectId,
        key: K,
        addrs: &impl PackageAddrs,
    ) -> Result<V, GetError>
    where
        K: MoveType + serde::Serialize + serde::de::DeserializeOwned,
        V: MoveType + serde::de::DeserializeOwned;

    /// Block until every object changed by `effects` is observable at
    /// its post-execution version.
    async fn wait_for_effects(
        &self,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<(), WaitError>;

    /// Single-object variant of [`ClientExt::wait_for_effects`] that
    /// returns the BCS-decoded object once the indexer is caught up.
    async fn wait_for_object<T>(
        &self,
        id: ObjectId,
        effects: &TransactionEffects,
        opts: WaitOptions,
        addrs: &impl PackageAddrs,
    ) -> Result<T, WaitError>
    where
        T: MoveType + serde::de::DeserializeOwned;
}

impl ClientExt for Client {
    async fn get_object<T>(&self, id: ObjectId, addrs: &impl PackageAddrs) -> Result<T, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        let mut client = self.clone();
        let req = proto::GetObjectRequest::default()
            .with_object_id(id.to_string())
            .with_read_mask(object_read_mask());
        let response = client
            .ledger_client()
            .get_object(req)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .into_inner();
        let proto_obj = response.object.ok_or(GetError::NotFound(id))?;
        let obj = Object::try_from(&proto_obj)
            .map_err(|e| GetError::Backend(format!("decode object: {e}")))?;
        decode_object_as::<T>(id, &obj, addrs)
    }

    async fn get_objects<T>(
        &self,
        ids: &[ObjectId],
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, GetError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut client = self.clone();
        let requests: Vec<proto::GetObjectRequest> = ids
            .iter()
            .map(|id| proto::GetObjectRequest::default().with_object_id(id.to_string()))
            .collect();
        let req = proto::BatchGetObjectsRequest::default()
            .with_requests(requests)
            .with_read_mask(object_read_mask());
        let response = client
            .ledger_client()
            .batch_get_objects(req)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .into_inner();
        let mut out = Vec::with_capacity(ids.len());
        for (id, result) in ids.iter().zip(&response.objects) {
            let proto::get_object_result::Result::Object(proto_obj) = result
                .result
                .as_ref()
                .ok_or_else(|| GetError::Backend("batch_get_objects: missing result".into()))?
            else {
                return Err(GetError::NotFound(*id));
            };
            let obj = Object::try_from(proto_obj)
                .map_err(|e| GetError::Backend(format!("decode object {id}: {e}")))?;
            out.push(decode_object_as::<T>(*id, &obj, addrs)?);
        }
        Ok(out)
    }

    async fn get_dynamic_field<K, V>(
        &self,
        parent: ObjectId,
        key: K,
        addrs: &impl PackageAddrs,
    ) -> Result<V, GetError>
    where
        K: MoveType + serde::Serialize + serde::de::DeserializeOwned,
        V: MoveType + serde::de::DeserializeOwned,
    {
        let key_bytes =
            bcs::to_bytes(&key).map_err(|e| GetError::Backend(format!("bcs encode key: {e}")))?;
        let key_type = K::type_tag(addrs);
        let child_id = parent.derive_dynamic_child_id(&key_type, &key_bytes);

        let mut client = self.clone();
        let req = proto::GetObjectRequest::default()
            .with_object_id(child_id.to_string())
            .with_read_mask(object_read_mask());
        let response = client
            .ledger_client()
            .get_object(req)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .into_inner();
        let proto_obj = response.object.ok_or(GetError::NotFound(child_id))?;
        let obj = Object::try_from(&proto_obj)
            .map_err(|e| GetError::Backend(format!("decode object: {e}")))?;
        let move_struct = obj
            .as_struct()
            .ok_or(GetError::NotAStruct { id: child_id })?;
        // BCS layout of `0x2::dynamic_field::Field<K, V>`:
        // `UID(=Address) || bcs(K) || bcs(V)`. We deserialize a
        // synthetic tuple-struct that pulls all three out at once
        // and returns just the value.
        #[derive(serde::Deserialize)]
        struct Field<K, V> {
            _id: Address,
            _name: K,
            value: V,
        }
        let field: Field<K, V> =
            bcs::from_bytes(move_struct.contents()).map_err(|source| GetError::Bcs {
                id: child_id,
                source,
            })?;
        Ok(field.value)
    }

    async fn wait_for_effects(
        &self,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<(), WaitError> {
        require_success(effects)?;
        let deadline = Instant::now() + opts.timeout;
        for (id, version) in target_versions(effects) {
            poll_for_version(self, id, version, deadline, opts.interval).await?;
        }
        Ok(())
    }

    async fn wait_for_object<T>(
        &self,
        id: ObjectId,
        effects: &TransactionEffects,
        opts: WaitOptions,
        addrs: &impl PackageAddrs,
    ) -> Result<T, WaitError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        require_success(effects)?;
        let version = target_versions(effects)
            .into_iter()
            .find(|(oid, _)| *oid == id)
            .map(|(_, v)| v)
            .ok_or(WaitError::NotInEffects(id))?;
        let deadline = Instant::now() + opts.timeout;
        let proto_obj = poll_for_version(self, id, version, deadline, opts.interval).await?;
        let obj = Object::try_from(&proto_obj)
            .map_err(|e| WaitError::Backend(format!("decode object: {e}")))?;
        decode_object_as::<T>(id, &obj, addrs).map_err(WaitError::Decode)
    }
}

#[allow(clippy::result_large_err)]
fn require_success(effects: &TransactionEffects) -> Result<(), WaitError> {
    match effects.status() {
        ExecutionStatus::Success => Ok(()),
        other => Err(WaitError::TxFailed(other.clone())),
    }
}

/// `(id, expected indexer version)` for every non-deletion write in
/// `effects`. `ObjectWrite` entries inherit `lamport_version`;
/// `PackageWrite` carries its own version. Other variants
/// (`NotExist`, accumulator writes) are skipped.
fn target_versions(effects: &TransactionEffects) -> Vec<(ObjectId, Version)> {
    let v2 = match effects {
        TransactionEffects::V2(v2) => v2,
        // v1 effects don't shape-up the same way; pre-v2 transactions
        // aren't covered by this poller.
        TransactionEffects::V1(_) => return Vec::new(),
    };
    let lamport = v2.lamport_version;
    v2.changed_objects
        .iter()
        .filter_map(|ch| match &ch.output_state {
            ObjectOut::ObjectWrite { .. } => Some((ch.object_id, lamport)),
            ObjectOut::PackageWrite { version, .. } => Some((ch.object_id, *version)),
            _ => None,
        })
        .collect()
}

async fn poll_for_version(
    client: &Client,
    id: ObjectId,
    version: Version,
    deadline: Instant,
    interval: Duration,
) -> Result<proto::Object, WaitError> {
    loop {
        let mut c = client.clone();
        let req = proto::GetObjectRequest::default()
            .with_object_id(id.to_string())
            .with_version(version)
            .with_read_mask(object_read_mask());
        match c.ledger_client().get_object(req).await {
            Ok(resp) => {
                if let Some(obj) = resp.into_inner().object {
                    return Ok(obj);
                }
            }
            Err(_) => {
                // Object not yet observable at this version — try again
                // until the deadline. Sustained errors hit the timeout
                // arm below.
            }
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

fn decode_object_as<T>(id: ObjectId, obj: &Object, addrs: &impl PackageAddrs) -> Result<T, GetError>
where
    T: MoveType + serde::de::DeserializeOwned,
{
    let move_struct = obj.as_struct().ok_or(GetError::NotAStruct { id })?;

    let expected = match T::type_tag(addrs) {
        TypeTag::Struct(s) => s,
        _ => {
            return Err(GetError::TypeMismatch {
                id,
                expected: Box::new(StructTag::new(
                    Address::ZERO,
                    sui_sdk_types::Identifier::new("p").expect("placeholder"),
                    sui_sdk_types::Identifier::new("p").expect("placeholder"),
                    Vec::new(),
                )),
                actual: Box::new(move_struct.object_type().clone()),
            });
        }
    };

    let actual = move_struct.object_type();
    if &*expected != actual {
        return Err(GetError::TypeMismatch {
            id,
            expected,
            actual: Box::new(actual.clone()),
        });
    }

    bcs::from_bytes::<T>(move_struct.contents()).map_err(|source| GetError::Bcs { id, source })
}
