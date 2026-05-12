//! Backend-trait impls for `sui_rpc::Client`. Plugs the gRPC client
//! into [`PtbBuilder::with_client`] so a Sui binding can run end-to-end
//! against a fullnode without further glue.
//!
//! `sui_rpc::Client::ledger_client(&mut self)` (and friends) take
//! `&mut self`, but our trait surface is `&self`. Since `Client` is
//! `Clone` over a shared `tonic::transport::Channel`, the impls clone
//! cheaply per call.
//!
//! All proto → SDK conversions go through `try_from`/`Bcs::deserialize`
//! so we don't hand-roll the wire format.

use std::str::FromStr;

use sui_rpc::field::{FieldMask, FieldMaskUtil};
use sui_rpc::proto::sui::rpc::v2 as proto;
use sui_rpc::proto::sui::rpc::v2::simulate_transaction_request::TransactionChecks;
use sui_rpc::Client;
use sui_sdk_types::{
    Address, Digest, ObjectReference, Owner, StructTag, Transaction, TransactionEffects, TypeTag,
    UserSignature,
};

use crate::{
    DecodeError, DryRunError, DryRunEstimateFuture, DryRunFuture, DryRunner, EventReader,
    EventReaderError, EventsByTxFuture, FetchError, FetchFuture, FetchedObject, Fetcher,
    FindByTypeFuture, FindError, GasOracle, InspectResult, ListGasCoinsFuture, ObjectId,
    ObjectTypeFinder, OracleError, RefGasPriceFuture, SubmitError, SubmitFuture, Submitter,
    SuggestBudgetFuture,
};

impl InspectResult {
    /// Decode the first return value of command `cmd_idx` (0-based, in
    /// the order calls were made on the builder). For multi-return Move
    /// functions, use [`Self::decode_at`] to pick a specific slot.
    ///
    /// Sui's `sui_transaction_builder::Argument` has private fields, so
    /// unlike the iota side we can't recover the command index from the
    /// `Argument` returned by `move_call`. Callers track the command index
    /// themselves — it's the 0-based sequence number of the call.
    pub fn decode<T: serde::de::DeserializeOwned>(&self, cmd_idx: usize) -> Result<T, DecodeError> {
        self.decode_at(cmd_idx, 0)
    }

    /// Decode the return value at slot `slot` of command `cmd_idx`.
    pub fn decode_at<T: serde::de::DeserializeOwned>(
        &self,
        cmd_idx: usize,
        slot: usize,
    ) -> Result<T, DecodeError> {
        let cmd = self
            .returns
            .get(cmd_idx)
            .ok_or_else(|| DecodeError::NotFound(format!("cmd={cmd_idx}")))?;
        let bytes = cmd
            .get(slot)
            .ok_or_else(|| DecodeError::NotFound(format!("cmd={cmd_idx} slot={slot}")))?;
        bcs::from_bytes(bytes).map_err(DecodeError::Bcs)
    }
}

impl Fetcher for Client {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        let mut client = self.clone();
        Box::pin(async move {
            let req = proto::GetObjectRequest::default()
                .with_object_id(id.to_string())
                .with_read_mask(FieldMask::from_paths([
                    "object_id",
                    "version",
                    "digest",
                    "owner",
                ]));
            let response = client
                .ledger_client()
                .get_object(req)
                .await
                .map_err(|e| FetchError::Backend(e.to_string()))?
                .into_inner();
            let obj = response.object.ok_or(FetchError::NotFound(id))?;
            object_to_fetched(&obj, id)
        })
    }
}

impl Submitter for Client {
    fn submit<'a>(
        &'a self,
        tx: &'a Transaction,
        signatures: &'a [UserSignature],
    ) -> SubmitFuture<'a> {
        let mut client = self.clone();
        let tx = tx.clone();
        let signatures = signatures.to_vec();
        Box::pin(async move {
            let proto_tx: proto::Transaction = tx.into();
            let proto_sigs: Vec<proto::UserSignature> =
                signatures.into_iter().map(Into::into).collect();
            let req = proto::ExecuteTransactionRequest::default()
                .with_transaction(proto_tx)
                .with_signatures(proto_sigs)
                // Paths are rooted at `ExecutedTransaction` (the proto
                // default is `effects.status,checkpoint`), not at the
                // wrapping response. We need the BCS-encoded effects
                // so `TransactionEffects::try_from(&proto)` can decode
                // them.
                .with_read_mask(FieldMask::from_paths(["effects.bcs", "effects.status"]));
            let response = client
                .execution_client()
                .execute_transaction(req)
                .await
                .map_err(|e| SubmitError::Backend(e.to_string()))?
                .into_inner();
            let effects = response
                .transaction
                .and_then(|t| t.effects)
                .ok_or_else(|| {
                    SubmitError::Backend("execute_transaction returned no effects".into())
                })?;
            TransactionEffects::try_from(&effects)
                .map_err(|e| SubmitError::Backend(format!("decode effects: {e}")))
        })
    }
}

impl ObjectTypeFinder for Client {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a> {
        let mut client = self.clone();
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let want_type = type_tag.to_string();
            let requests: Vec<proto::GetObjectRequest> = ids
                .iter()
                .map(|id| proto::GetObjectRequest::default().with_object_id(id.to_string()))
                .collect();
            let req = proto::BatchGetObjectsRequest::default()
                .with_requests(requests)
                .with_read_mask(FieldMask::from_paths([
                    "object_id",
                    "version",
                    "digest",
                    "object_type",
                ]));
            let response = client
                .ledger_client()
                .batch_get_objects(req)
                .await
                .map_err(|e| FindError::Backend(e.to_string()))?
                .into_inner();
            let mut refs = Vec::new();
            for result in &response.objects {
                let proto::get_object_result::Result::Object(obj) =
                    result.result.as_ref().ok_or_else(|| {
                        FindError::Backend("batch_get_objects: missing result".into())
                    })?
                else {
                    // Object not found at this id — skip rather than error;
                    // callers requested a set, not all-or-nothing.
                    continue;
                };
                let on_chain_type = obj.object_type_opt().unwrap_or_default();
                if on_chain_type != want_type {
                    continue;
                }
                refs.push(object_to_reference(obj).map_err(FindError::Backend)?);
            }
            Ok(refs)
        })
    }
}

impl EventReader for Client {
    fn events_by_tx<'a>(&'a self, digest: Digest, type_tag: TypeTag) -> EventsByTxFuture<'a> {
        let mut client = self.clone();
        Box::pin(async move {
            let req = proto::GetTransactionRequest::default()
                .with_digest(digest.to_string())
                .with_read_mask(FieldMask::from_paths([
                    "events.events.event_type",
                    "events.events.contents",
                ]));
            let response = client
                .ledger_client()
                .get_transaction(req)
                .await
                .map_err(|e| EventReaderError::Backend(e.to_string()))?
                .into_inner();
            let tx = response.transaction.ok_or_else(|| {
                EventReaderError::Backend(format!("transaction {digest} not found"))
            })?;
            let want_type = type_tag.to_string();
            let mut payloads = Vec::new();
            for ev in tx.events.iter().flat_map(|e| e.events.iter()) {
                let ty = ev.event_type_opt().unwrap_or_default();
                if ty != want_type {
                    continue;
                }
                let bytes = ev
                    .contents
                    .as_ref()
                    .and_then(|b| b.value.as_ref())
                    .ok_or_else(|| {
                        EventReaderError::Backend(
                            "event matched type but carried no BCS contents".into(),
                        )
                    })?;
                payloads.push(bytes.to_vec());
            }
            Ok(payloads)
        })
    }
}

impl GasOracle for Client {
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a> {
        let mut client = self.clone();
        Box::pin(async move {
            // `Coin<SUI>` — the standard gas type.
            let coin_struct = StructTag::coin(TypeTag::Struct(Box::new(StructTag::sui())));
            let read_mask = FieldMask::from_paths(["object_id", "version", "digest"]);
            let mut refs = Vec::new();
            let mut page_token = None;
            loop {
                let mut req = proto::ListOwnedObjectsRequest::default()
                    .with_owner(owner.to_string())
                    .with_object_type(coin_struct.to_string())
                    .with_page_size(500u32)
                    .with_read_mask(read_mask.clone());
                req.page_token = page_token;
                let page = client
                    .state_client()
                    .list_owned_objects(req)
                    .await
                    .map_err(|e| OracleError::Backend(e.to_string()))?
                    .into_inner();
                for obj in &page.objects {
                    refs.push(object_to_reference(obj).map_err(OracleError::Backend)?);
                }
                match page.next_page_token {
                    Some(t) if !t.is_empty() => page_token = Some(t),
                    _ => break,
                }
            }
            Ok(refs)
        })
    }

    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
        let mut client = self.clone();
        Box::pin(async move {
            client
                .get_reference_gas_price()
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))
        })
    }

    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
        // Conservative fallback only used when `dry_run_estimate` fails.
        // Matches the iota backend's 50_000_000 nano default at MIST scale.
        Box::pin(async move { Ok(50_000_000u64) })
    }

    fn dry_run_estimate<'a>(&'a self, tx: &'a Transaction) -> DryRunEstimateFuture<'a> {
        let mut client = self.clone();
        let tx = tx.clone();
        Box::pin(async move {
            let req = proto::SimulateTransactionRequest::default()
                .with_transaction(proto::Transaction::from(tx))
                .with_read_mask(FieldMask::from_paths(["transaction.effects.bcs"]))
                .with_checks(TransactionChecks::Disabled);
            let response = client
                .execution_client()
                .simulate_transaction(req)
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?
                .into_inner();
            let effects_proto = response
                .transaction
                .and_then(|t| t.effects)
                .ok_or_else(|| OracleError::Backend("simulate returned no effects".into()))?;
            let effects = TransactionEffects::try_from(&effects_proto)
                .map_err(|e| OracleError::Backend(format!("decode effects: {e}")))?;
            Ok(effects.gas_summary().gas_used())
        })
    }
}

impl DryRunner for Client {
    fn dry_run<'a>(&'a self, tx: &'a Transaction) -> DryRunFuture<'a> {
        let mut client = self.clone();
        let tx = tx.clone();
        Box::pin(async move {
            let req = proto::SimulateTransactionRequest::default()
                .with_transaction(proto::Transaction::from(tx))
                .with_read_mask(FieldMask::from_paths([
                    "transaction.effects.bcs",
                    "command_outputs",
                ]))
                .with_checks(TransactionChecks::Disabled);
            let response = client
                .execution_client()
                .simulate_transaction(req)
                .await
                .map_err(|e| DryRunError::Backend(e.to_string()))?
                .into_inner();
            let effects_proto = response
                .transaction
                .and_then(|t| t.effects)
                .ok_or_else(|| DryRunError::Backend("simulate returned no effects".into()))?;
            let effects = TransactionEffects::try_from(&effects_proto)
                .map_err(|e| DryRunError::Backend(format!("decode effects: {e}")))?;
            let returns: Vec<Vec<Vec<u8>>> = response
                .command_outputs
                .into_iter()
                .map(|cmd| {
                    cmd.return_values
                        .into_iter()
                        .filter_map(|out| out.value.and_then(|b| b.value).map(|b| b.to_vec()))
                        .collect()
                })
                .collect();
            Ok(InspectResult { effects, returns })
        })
    }
}

/// Build a [`FetchedObject`] from a `proto::Object`, mapping `Owner` to
/// our cache's owned/shared distinction.
fn object_to_fetched(obj: &proto::Object, id: ObjectId) -> Result<FetchedObject, FetchError> {
    let owner_proto = obj
        .owner
        .as_ref()
        .ok_or_else(|| FetchError::Backend(format!("object {id} has no owner")))?;
    let owner = Owner::try_from(owner_proto)
        .map_err(|e| FetchError::Backend(format!("decode owner: {e}")))?;
    match owner {
        Owner::Shared(initial) => Ok(FetchedObject::Shared {
            initial_shared_version: initial,
            // Default to mutable — Sui's `Shared<T>` / `SharedMut<T>` wrappers
            // override per call site if needed.
            mutable: true,
        }),
        Owner::Address(_) | Owner::Object(_) | Owner::Immutable => Ok(FetchedObject::Owned(
            object_to_reference(obj).map_err(FetchError::Backend)?,
        )),
        Owner::ConsensusAddress { .. } => Err(FetchError::Backend(format!(
            "object {id}: ConsensusAddress owner not supported"
        ))),
        _ => Err(FetchError::Backend(format!(
            "object {id}: unknown owner variant {owner:?}"
        ))),
    }
}

/// Extract an `ObjectReference` from a `proto::Object` whose read mask
/// covered `object_id,version,digest`.
fn object_to_reference(obj: &proto::Object) -> Result<ObjectReference, String> {
    let id_str = obj
        .object_id_opt()
        .ok_or_else(|| "object missing object_id".to_string())?;
    let id = Address::from_str(id_str).map_err(|e| format!("parse object_id: {e}"))?;
    let version = obj
        .version_opt()
        .ok_or_else(|| "object missing version".to_string())?;
    let digest_str = obj
        .digest_opt()
        .ok_or_else(|| "object missing digest".to_string())?;
    let digest = Digest::from_str(digest_str).map_err(|e| format!("parse digest: {e}"))?;
    Ok(ObjectReference::new(id, version, digest))
}
