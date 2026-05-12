//! `EffectsExt` — extension trait on [`TransactionEffects`] that
//! decodes typed objects + events out of a transaction's effects.
//!
//! Pairs with `ObjectTypeFinder` / `EventReader` from `ext-sui`:
//! `created_in::<T>` / `mutated_in::<T>` / `changed_in::<T>` return
//! object refs; `*_decoded::<T>` follow up with a typed batch fetch;
//! `events_of_type::<E>` pulls BCS-decoded events.
//!
//! Sui's `TransactionEffects` is a v1/v2 enum. Only v2 carries the
//! per-object `id_operation` + `output_state` shape we need; v1 effects
//! aren't supported here and surface a clear error if seen.

use sui_sdk_types::{IdOperation, ObjectOut};

use crate::{
    EventReader, EventReaderError, FindError, MoveType, ObjectId, ObjectReference,
    ObjectTypeFinder, PackageAddrs, TransactionEffects,
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

    async fn events_of_type<E>(
        &self,
        reader: &(impl EventReader + ?Sized),
        addrs: &impl PackageAddrs,
    ) -> Result<Vec<E>, EventsError>
    where
        E: MoveType + serde::de::DeserializeOwned,
    {
        let digest = match self {
            TransactionEffects::V2(v2) => v2.transaction_digest,
            TransactionEffects::V1(_) => return Err(EventsError::UnsupportedEffectsV1),
        };
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

/// Errors from [`EffectsExt::events_of_type`].
#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    #[error(transparent)]
    Reader(#[from] EventReaderError),
    #[error("bcs decode of event payload: {0}")]
    Bcs(bcs::Error),
    #[error("effects v1 isn't supported — only v2 carries the per-event metadata we need")]
    UnsupportedEffectsV1,
}

#[derive(Copy, Clone)]
enum ChangeKind {
    Created,
    Mutated,
    Any,
}

fn changed_ids(effects: &TransactionEffects, kind: ChangeKind) -> Vec<ObjectId> {
    let v2 = match effects {
        TransactionEffects::V2(v2) => v2,
        // v1 doesn't carry `id_operation` / `ObjectOut` in the same shape
        // — pre-v2 effects aren't a target for typed object lookup here.
        TransactionEffects::V1(_) => return Vec::new(),
    };
    v2.changed_objects
        .iter()
        .filter(|c| {
            // Only count real object writes; package writes and
            // accumulator writes aren't user-typed objects we'd want
            // to decode through `ObjectTypeFinder`, and `NotExist` is
            // a delete.
            if !matches!(c.output_state, ObjectOut::ObjectWrite { .. }) {
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
