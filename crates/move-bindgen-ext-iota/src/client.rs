//! Backend-trait impls for `iota_sdk_graphql_client::Client`, plus the
//! `InspectResult::decode` helper that turns dry-run output into typed
//! values keyed by `Argument`.

use iota_sdk_graphql_client::{
    query_types::{EventFilter, ObjectFilter},
    Client, PaginationFilter,
};
use iota_sdk_transaction_builder::unresolved::Argument;
use iota_sdk_types::{
    Address, Digest, ObjectId, Owner, Transaction, TypeTag, UserSignature,
};

use crate::{
    DecodeError, DryRunError, DryRunEstimateFuture, DryRunFuture, DryRunner, EventReader,
    EventReaderError, EventsByTxFuture, FetchError, FetchFuture, FetchedObject, Fetcher,
    FindByTypeFuture, FindError, GasOracle, InspectResult, ListGasCoinsFuture, ObjectTypeFinder,
    OracleError, RefGasPriceFuture, SubmitError, SubmitFuture, Submitter, SuggestBudgetFuture,
};

impl InspectResult {
    /// Decode the return value referenced by `arg`. `Argument::Result(idx)`
    /// decodes the first slot of command `idx` (correct for single-return
    /// calls); `Argument::NestedResult(idx, sub)` indexes a specific slot of
    /// a multi-return call.
    pub fn decode<T: serde::de::DeserializeOwned>(&self, arg: Argument) -> Result<T, DecodeError> {
        let bytes: &[u8] = match arg {
            Argument::Result(cmd) => self
                .returns
                .get(cmd as usize)
                .and_then(|cmd_returns| cmd_returns.first())
                .map(Vec::as_slice)
                .ok_or(DecodeError::NotFound(arg))?,
            Argument::NestedResult(cmd, sub) => self
                .returns
                .get(cmd as usize)
                .and_then(|cmd_returns| cmd_returns.get(sub as usize))
                .map(Vec::as_slice)
                .ok_or(DecodeError::NotFound(arg))?,
            // `Gas`, `Input(_)`, or any future variant: not a return value.
            _ => return Err(DecodeError::NotAReturn(arg)),
        };
        bcs::from_bytes(bytes).map_err(DecodeError::Bcs)
    }
}

impl Fetcher for Client {
    fn fetch<'a>(&'a self, id: ObjectId) -> FetchFuture<'a> {
        Box::pin(async move {
            let obj = self
                .object(id, None)
                .await
                .map_err(|e| FetchError::Backend(e.to_string()))?
                .ok_or(FetchError::NotFound(id))?;
            match obj.owner() {
                Owner::Shared(initial_shared_version) => Ok(FetchedObject::Shared {
                    initial_shared_version: initial_shared_version.as_u64(),
                    // Default to mutable — the safer choice for entry points
                    // that take `&mut`. Pre-`register_shared` if you need it
                    // immutable.
                    mutable: true,
                }),
                Owner::Address(_) | Owner::Object(_) | Owner::Immutable => {
                    Ok(FetchedObject::Owned(obj.object_ref()))
                }
                other => Err(FetchError::Backend(format!("unknown owner: {other:?}"))),
            }
        })
    }
}

impl Submitter for Client {
    fn submit<'a>(
        &'a self,
        tx: &'a Transaction,
        signatures: &'a [UserSignature],
    ) -> SubmitFuture<'a> {
        Box::pin(async move {
            self.execute_tx(signatures, tx, None)
                .await
                .map_err(|e| SubmitError::Backend(e.to_string()))
        })
    }
}

impl ObjectTypeFinder for Client {
    fn find_by_type<'a>(&'a self, type_tag: TypeTag, ids: Vec<ObjectId>) -> FindByTypeFuture<'a> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let filter = ObjectFilter {
                type_: Some(type_tag.to_string()),
                owner: None,
                object_ids: Some(ids),
            };
            let page = self
                .objects(filter, PaginationFilter::default())
                .await
                .map_err(|e| FindError::Backend(e.to_string()))?;
            Ok(page.data.iter().map(|o| o.object_ref()).collect())
        })
    }
}

impl EventReader for Client {
    fn events_by_tx<'a>(&'a self, digest: Digest, type_tag: TypeTag) -> EventsByTxFuture<'a> {
        Box::pin(async move {
            let filter = EventFilter {
                emitting_module: None,
                event_type: Some(type_tag.to_string()),
                sender: None,
                transaction_digest: Some(digest.to_string()),
            };
            let mut bcs_payloads: Vec<Vec<u8>> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = self
                    .events(
                        filter.clone(),
                        PaginationFilter {
                            cursor: cursor.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|e| EventReaderError::Backend(e.to_string()))?;
                for ev in &page.data {
                    let bytes = base64ct_decode(&ev.bcs.0)
                        .map_err(|e| EventReaderError::Backend(format!("base64: {e}")))?;
                    bcs_payloads.push(bytes);
                }
                if !page.page_info.has_next_page {
                    break;
                }
                cursor = page.page_info.end_cursor;
            }
            Ok(bcs_payloads)
        })
    }
}

fn base64ct_decode(s: &str) -> Result<Vec<u8>, base64ct::Error> {
    use base64ct::Encoding;
    base64ct::Base64::decode_vec(s)
}

impl GasOracle for Client {
    fn list_gas_coins<'a>(&'a self, owner: Address) -> ListGasCoinsFuture<'a> {
        Box::pin(async move {
            let page = self
                .gas_coins(owner, PaginationFilter::default())
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?;
            // `Coin` carries only id+balance. Re-fetch each as an `Object` to
            // get the full reference (id + version + digest).
            let mut refs = Vec::with_capacity(page.data.len());
            for coin in &page.data {
                let obj = self
                    .object(*coin.id(), None)
                    .await
                    .map_err(|e| OracleError::Backend(e.to_string()))?
                    .ok_or_else(|| {
                        OracleError::Backend(format!("coin {} disappeared", coin.id()))
                    })?;
                refs.push(obj.object_ref());
            }
            Ok(refs)
        })
    }

    fn reference_gas_price<'a>(&'a self) -> RefGasPriceFuture<'a> {
        Box::pin(async move {
            self.reference_gas_price(None)
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?
                .ok_or_else(|| OracleError::Backend("reference gas price unavailable".into()))
        })
    }

    fn suggest_gas_budget<'a>(&'a self) -> SuggestBudgetFuture<'a> {
        // Conservative fallback only used when `dry_run_estimate` fails.
        Box::pin(async move { Ok(50_000_000u64) })
    }

    fn dry_run_estimate<'a>(&'a self, tx: &'a Transaction) -> DryRunEstimateFuture<'a> {
        Box::pin(async move {
            let result = self
                .dry_run_tx(tx, /* skip_checks */ true)
                .await
                .map_err(|e| OracleError::Backend(e.to_string()))?;
            if let Some(err) = result.error {
                return Err(OracleError::Backend(format!("dry-run aborted: {err}")));
            }
            let effects = result
                .effects
                .ok_or_else(|| OracleError::Backend("dry-run returned no effects".into()))?;
            Ok(effects.gas_summary().gas_used())
        })
    }
}

impl DryRunner for Client {
    fn dry_run<'a>(&'a self, tx: &'a Transaction) -> DryRunFuture<'a> {
        Box::pin(async move {
            let result = self
                .dry_run_tx(tx, /* skip_checks */ true)
                .await
                .map_err(|e| DryRunError::Backend(e.to_string()))?;
            if let Some(err) = result.error {
                return Err(DryRunError::Aborted(err));
            }
            let effects = result
                .effects
                .ok_or_else(|| DryRunError::Backend("dry-run returned no effects".into()))?;
            let returns = result
                .results
                .into_iter()
                .map(|cmd| cmd.return_values.into_iter().map(|r| r.bcs).collect())
                .collect();
            Ok(InspectResult { effects, returns })
        })
    }
}
