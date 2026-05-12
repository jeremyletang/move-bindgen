//! `EffectsExt` — extension trait on [`TransactionEffects`] that
//! decodes typed objects + events out of a transaction's effects.
//!
//! Pairs with `ObjectTypeFinder` / `EventReader` / `ClientExt` from
//! ext-iota: `created_in::<T>` / `mutated_in::<T>` /  `changed_in::<T>`
//! return object refs; `*_decoded::<T>` follow up with a typed batch
//! fetch; `events_of_type::<E>` pulls BCS-decoded events.

use crate::{
    ClientExt, EventReader, EventReaderError, FindError, GetError, MoveType, ObjectId,
    ObjectReference, ObjectTypeFinder, PackageAddrs, TransactionEffects,
};

#[allow(async_fn_in_trait)]
pub trait EffectsExt {
    /// Object refs of all `T`-typed objects newly created in this tx.
    async fn created_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// Object refs of all `T`-typed objects mutated (but not created) in this tx.
    async fn mutated_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// Object refs of all `T`-typed objects either created or mutated in this tx.
    async fn changed_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError>;

    /// `created_in` followed by a typed batch fetch — returns fully decoded
    /// `T`s for every object of type `T` newly created in this tx.
    async fn created_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// `mutated_in` followed by a typed batch fetch.
    async fn mutated_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// `changed_in` followed by a typed batch fetch.
    async fn changed_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned;

    /// BCS-decode every event of type `E` emitted by this tx.
    async fn events_of_type<E>(
        &self,
        reader: &(impl EventReader + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<E>, EventsError>
    where
        E: MoveType + serde::de::DeserializeOwned;
}

impl EffectsExt for TransactionEffects {
    async fn created_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(addrs), changed_ids(self, ChangeKind::Created))
            .await
    }

    async fn mutated_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(addrs), changed_ids(self, ChangeKind::Mutated))
            .await
    }

    async fn changed_in<T: MoveType>(
        &self,
        finder: &(impl ObjectTypeFinder + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<ObjectReference>, FindError> {
        finder
            .find_by_type(T::type_tag(addrs), changed_ids(self, ChangeKind::Any))
            .await
    }

    async fn created_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        decoded_for(self, client, ChangeKind::Created, addrs).await
    }

    async fn mutated_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        decoded_for(self, client, ChangeKind::Mutated, addrs).await
    }

    async fn changed_decoded<T>(
        &self,
        client: &(impl ObjectTypeFinder + ClientExt),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<T>, EffectsDecodeError>
    where
        T: MoveType + serde::de::DeserializeOwned,
    {
        decoded_for(self, client, ChangeKind::Any, addrs).await
    }

    async fn events_of_type<E>(
        &self,
        reader: &(impl EventReader + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<E>, EventsError>
    where
        E: MoveType + serde::de::DeserializeOwned,
    {
        let digest = self.as_v1().transaction_digest;
        let payloads = reader
            .events_by_tx(digest, E::type_tag(addrs))
            .await
            .map_err(EventsError::Reader)?;
        payloads
            .iter()
            .map(|b| bcs::from_bytes::<E>(b).map_err(EventsError::Bcs))
            .collect()
    }
}

/// Errors from the `*_decoded` family on [`EffectsExt`].
#[derive(Debug, thiserror::Error)]
pub enum EffectsDecodeError {
    #[error(transparent)]
    Find(#[from] FindError),
    #[error(transparent)]
    Get(#[from] GetError),
}

/// Errors from [`EffectsExt::events_of_type`].
#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error(transparent)]
    Reader(#[from] EventReaderError),
    #[error("bcs decode of event payload: {0}")]
    Bcs(bcs::Error),
}

async fn decoded_for<T>(
    effects: &TransactionEffects,
    client: &(impl ObjectTypeFinder + ClientExt),
    kind: ChangeKind,
    addrs: &impl PackageAddrs,
) -> Result<Vec<T>, EffectsDecodeError>
where
    T: MoveType + serde::de::DeserializeOwned,
{
    let refs = client
        .find_by_type(T::type_tag(addrs), changed_ids(effects, kind))
        .await?;
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<ObjectId> = refs.iter().map(|r| r.object_id).collect();
    Ok(client.get_objects::<T>(&ids, addrs).await?)
}

#[derive(Copy, Clone)]
enum ChangeKind {
    Created,
    Mutated,
    Any,
}

fn changed_ids(effects: &TransactionEffects, kind: ChangeKind) -> Vec<ObjectId> {
    use iota_sdk_types::IdOperation;
    let v1 = effects.as_v1();
    v1.changed_objects
        .iter()
        .filter(|c| {
            // Only count objects that were actually written (created/mutated).
            // `Missing` outputs are deletes; we ignore those here.
            if c.output_state.is_missing() {
                return false;
            }
            match kind {
                ChangeKind::Created => matches!(c.id_operation, IdOperation::Created),
                ChangeKind::Mutated => !matches!(c.id_operation, IdOperation::Created),
                ChangeKind::Any => true,
            }
        })
        .map(|c| c.object_id)
        .collect()
}
