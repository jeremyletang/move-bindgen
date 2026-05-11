//! `ClientExt` — typed read helpers on `iota_sdk_graphql_client::Client`.
//! Sibling methods follow the `get_X` / `list_X` pattern as we add them
//! (`get_dynamic_field`, `list_objects_owned_by`, …).

use std::time::Instant;

use iota_sdk_graphql_client::{query_types::ObjectFilter, Client, PaginationFilter};
use iota_sdk_types::{Address, Object, ObjectId, StructTag, TransactionEffects, TypeTag};

use crate::{
    wait::{poll_for_version, require_success, target_versions},
    MoveType, PackageAddrs, WaitError, WaitOptions,
};

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

/// Typed read helpers on `iota_sdk_graphql_client::Client`.
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
    /// as `V`. `K`'s type tag is sent as the field's name type; `V`'s
    /// is checked against the indexer's reported value type before
    /// decoding. Both type tags resolve against `addrs`.
    async fn get_dynamic_field<K, V>(
        &self,
        parent: ObjectId,
        key: K,
        addrs: &impl PackageAddrs,
    ) -> Result<V, GetError>
    where
        K: MoveType + serde::Serialize,
        V: MoveType + serde::de::DeserializeOwned;

    /// Block until the indexer reflects every changed object in `effects`.
    ///
    /// `execute()` returns once the validators have processed the tx, but the
    /// GraphQL indexer ingests checkpoints asynchronously, so a subsequent
    /// `get_object` may briefly serve the *pre-tx* state. Call this between
    /// `execute()` and any read that depends on the new state.
    ///
    /// Errors if the tx didn't succeed or any object hasn't appeared at its
    /// post-execution version before [`WaitOptions::timeout`].
    async fn wait_for_effects(
        &self,
        effects: &TransactionEffects,
        opts: WaitOptions,
    ) -> Result<(), WaitError>;

    /// Like [`ClientExt::wait_for_effects`] for a single object id, returning
    /// the BCS-decoded object once the indexer is caught up. `id` must appear
    /// in `effects.changed_objects` and must not be a deletion.
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
        let obj = self
            .object(id, None)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .ok_or(GetError::NotFound(id))?;
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
        let filter = ObjectFilter {
            type_: None,
            owner: None,
            object_ids: Some(ids.to_vec()),
        };
        let mut objs: Vec<Object> = Vec::with_capacity(ids.len());
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .objects(
                    filter.clone(),
                    PaginationFilter {
                        cursor: cursor.clone(),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| GetError::Backend(e.to_string()))?;
            objs.extend(page.data);
            if !page.page_info.has_next_page {
                break;
            }
            cursor = page.page_info.end_cursor;
        }

        for id in ids {
            if !objs.iter().any(|o| o.object_id() == *id) {
                return Err(GetError::NotFound(*id));
            }
        }

        objs.iter()
            .map(|o| decode_object_as::<T>(o.object_id(), o, addrs))
            .collect()
    }

    async fn get_dynamic_field<K, V>(
        &self,
        parent: ObjectId,
        key: K,
        addrs: &impl PackageAddrs,
    ) -> Result<V, GetError>
    where
        K: MoveType + serde::Serialize,
        V: MoveType + serde::de::DeserializeOwned,
    {
        let parent_addr: Address = *parent.as_address();
        let output = self
            .dynamic_field(parent_addr, K::type_tag(addrs), key)
            .await
            .map_err(|e| GetError::Backend(e.to_string()))?
            .ok_or(GetError::NotFound(parent))?;
        let dfv = output.value.as_ref().ok_or(GetError::NotFound(parent))?;
        let expected = V::type_tag(addrs);
        if dfv.type_ != expected {
            return Err(GetError::Backend(format!(
                "dynamic field on {parent}: expected value type {expected}, got {actual}",
                actual = dfv.type_,
            )));
        }
        bcs::from_bytes::<V>(&dfv.bcs).map_err(|source| GetError::Bcs { id: parent, source })
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
        let obj = poll_for_version(self, id, version, deadline, opts.interval).await?;
        decode_object_as::<T>(id, &obj, addrs).map_err(WaitError::Decode)
    }
}

pub(crate) fn decode_object_as<T>(
    id: ObjectId,
    obj: &Object,
    addrs: &impl PackageAddrs,
) -> Result<T, GetError>
where
    T: MoveType + serde::de::DeserializeOwned,
{
    let move_struct = obj.as_struct_opt().ok_or(GetError::NotAStruct { id })?;

    let expected = match T::type_tag(addrs) {
        TypeTag::Struct(s) => s,
        // T isn't a struct type → can't be the contents of an object.
        _ => {
            return Err(GetError::TypeMismatch {
                id,
                expected: Box::new(StructTag::new(
                    Address::ZERO,
                    iota_sdk_types::Identifier::new("∅").expect("placeholder"),
                    iota_sdk_types::Identifier::new("∅").expect("placeholder"),
                    Vec::new(),
                )),
                actual: Box::new(move_struct.struct_tag().clone()),
            });
        }
    };

    let actual = move_struct.struct_tag();
    if &*expected != actual {
        return Err(GetError::TypeMismatch {
            id,
            expected,
            actual: Box::new(actual.clone()),
        });
    }

    bcs::from_bytes::<T>(move_struct.contents()).map_err(|source| GetError::Bcs { id, source })
}
