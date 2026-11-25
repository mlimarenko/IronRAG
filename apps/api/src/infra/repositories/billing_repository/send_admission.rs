//! The one durable admission of one physical provider send.
//!
//! A physical send is the unit the financial counter debits and the unit the
//! provider-call ledger records. This owner writes both in one transaction on
//! the locked provider row, validates that the named owning execution really
//! belongs to the binding's workspace and library, and refuses the whole send
//! otherwise. A retry therefore cannot hide behind the first attempt's row, and
//! a refused send leaves neither a debit nor a row.

use std::time::Duration;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    infra::repositories::ai_repository::{self, FinancialAdmissionError, RequiredFinancialScope},
    shared::{provider_http::DispatchCaller, provider_send::ProviderSendAttribution},
};

use super::{NewBillingProviderCall, create_provider_call, resolve_billing_execution_scope};

/// Why one physical send was not admitted.
#[derive(Debug)]
pub enum ProviderSendAdmissionError {
    /// The named owning execution is missing or outside the binding's scope.
    Attribution,
    /// The financial policy refused the attempt; nothing was written.
    Financial(FinancialAdmissionError),
    /// The admission transaction failed; its outcome is unknown only to the
    /// caller, never partially applied.
    Persistence(sqlx::Error),
}

/// One admitted physical send: the durable row and the incident scope, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmittedProviderSend {
    pub provider_call_id: Uuid,
    pub scope_id: Option<Uuid>,
}

/// Per-send commit-fault arm set used only by product tests.
///
/// The fault is selected by the exact `provider_call_id` the caller armed, is
/// removed the first time it is consulted, and cannot be reached from request
/// input, configuration or a production constructor. It performs the real
/// commit and then reports a synthetic acknowledgement loss, which exercises the
/// same reconciliation path as a lost commit reply.
#[cfg(feature = "test-support")]
mod committed_ack_fault {
    use std::{
        collections::BTreeSet,
        sync::{Mutex, OnceLock},
    };

    use uuid::Uuid;

    fn armed() -> &'static Mutex<BTreeSet<Uuid>> {
        static ARMED: OnceLock<Mutex<BTreeSet<Uuid>>> = OnceLock::new();
        ARMED.get_or_init(|| Mutex::new(BTreeSet::new()))
    }

    pub(super) fn arm(provider_call_id: Uuid) {
        armed().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(provider_call_id);
    }

    pub(super) fn take(provider_call_id: Uuid) -> bool {
        armed().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&provider_call_id)
    }
}

/// Arms one committed-but-unacknowledged admission for the exact send row id.
///
/// Test-only: production builds do not contain the hook, and an armed id is
/// consumed by the first admission that reaches the commit boundary with that
/// id, so it cannot leak into an unrelated call.
#[cfg(feature = "test-support")]
pub fn arm_lost_commit_ack(provider_call_id: Uuid) {
    committed_ack_fault::arm(provider_call_id);
}

/// Owns a send row id across the admission transaction.
///
/// The row id exists before the INSERT, so a commit whose outcome the caller
/// cannot observe is still reconcilable: on drop, or after a commit error, the
/// guard looks the row up and terminalizes it without usage. A missing row means
/// the transaction rolled back, and the lookup is then a no-op. Bounded retries
/// cover the window where the server committed but the client has not seen it;
/// the stale-row reaper stays the last resort.
struct PendingSendRow {
    postgres: PgPool,
    provider_call_id: Uuid,
    armed: bool,
}

impl PendingSendRow {
    fn new(postgres: &PgPool, provider_call_id: Uuid) -> Self {
        Self { postgres: postgres.clone(), provider_call_id, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn reconcile_settled(&self) {
        for attempt in 0..5_u8 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            match super::finish_provider_call_without_usage(
                &self.postgres,
                self.provider_call_id,
                "canceled",
            )
            .await
            {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        provider_call_id = %self.provider_call_id,
                        %error,
                        "admission row reconciliation attempt failed"
                    );
                }
            }
        }
    }
}

impl Drop for PendingSendRow {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let guard = PendingSendRow {
            postgres: self.postgres.clone(),
            provider_call_id: self.provider_call_id,
            armed: false,
        };
        handle.spawn(async move { guard.reconcile_settled().await });
    }
}

/// One physical send presented for admission.
///
/// The fields travel together because they describe one attempt: the incident
/// scope it spends under, the caller the financial policy attests, the request
/// identity the ledger records, and the attribution the row is created from.
pub struct ProviderSendRequest<'a> {
    /// The startup-resolved incident scope, if any.
    pub required_scope: Option<&'a RequiredFinancialScope>,
    pub caller: &'a DispatchCaller,
    pub model: &'a str,
    pub endpoint: &'a str,
    pub body: &'a Value,
    pub attribution: &'a ProviderSendAttribution,
    pub provider_call_id: Uuid,
}

/// Admits exactly one physical send.
///
/// `required_scope` is the startup-resolved incident scope. With one, the
/// counter debit and the ledger row commit together under the provider row
/// lock; without one, the row alone is written and no counter moves, which is
/// the ordinary runtime the startup fence already proved safe.
pub async fn admit_provider_send(
    postgres: &PgPool,
    send: ProviderSendRequest<'_>,
) -> Result<AdmittedProviderSend, ProviderSendAdmissionError> {
    let ProviderSendRequest {
        required_scope,
        caller,
        model,
        endpoint,
        body,
        attribution,
        provider_call_id,
    } = send;
    let mut pending = PendingSendRow::new(postgres, provider_call_id);
    let owning_scope = resolve_billing_execution_scope(
        postgres,
        &attribution.owning_execution_kind,
        attribution.owning_execution_id,
    )
    .await
    .map_err(ProviderSendAdmissionError::Persistence)?
    .ok_or(ProviderSendAdmissionError::Attribution)?;
    if owning_scope.workspace_id != attribution.workspace_id
        || owning_scope.library_id != attribution.library_id
    {
        return Err(ProviderSendAdmissionError::Attribution);
    }
    let runtime_matches = provider_send_runtime_attribution_matches(
        postgres,
        &attribution.owning_execution_kind,
        attribution.owning_execution_id,
        attribution.runtime_execution_id,
        attribution.runtime_task_kind,
    )
    .await
    .map_err(ProviderSendAdmissionError::Persistence)?;
    if !runtime_matches {
        return Err(ProviderSendAdmissionError::Attribution);
    }

    let mut transaction =
        postgres.begin().await.map_err(ProviderSendAdmissionError::Persistence)?;
    let scope_id = match required_scope {
        Some(required) => {
            let permit = ai_repository::evaluate_financial_attempt(
                &mut transaction,
                required,
                caller,
                model,
                endpoint,
                body,
            )
            .await
            .map_err(ProviderSendAdmissionError::Financial)?;
            Some(permit.scope_id)
        }
        None => None,
    };
    create_provider_call(
        &mut *transaction,
        &NewBillingProviderCall {
            id: provider_call_id,
            workspace_id: attribution.workspace_id,
            library_id: attribution.library_id,
            binding_id: attribution.binding_id,
            owning_execution_kind: &attribution.owning_execution_kind,
            owning_execution_id: attribution.owning_execution_id,
            runtime_execution_id: attribution.runtime_execution_id,
            runtime_task_kind: attribution.runtime_task_kind,
            provider_catalog_id: attribution.provider_catalog_id,
            model_catalog_id: attribution.model_catalog_id,
            call_kind: &attribution.call_kind,
            call_state: "started",
            completed_at: None,
        },
    )
    .await
    .map_err(ProviderSendAdmissionError::Persistence)?;
    #[cfg(feature = "test-support")]
    let lost_ack = committed_ack_fault::take(provider_call_id);
    #[cfg(not(feature = "test-support"))]
    let lost_ack = false;

    if let Err(error) = transaction.commit().await {
        // The commit outcome is ambiguous: a server-side commit with a lost
        // acknowledgement must not leave an undeclared debit and an orphan row.
        pending.reconcile_settled().await;
        pending.disarm();
        return Err(ProviderSendAdmissionError::Persistence(error));
    }
    if lost_ack {
        // The server committed the row and the debit; only the reply is lost.
        // Reconcile the committed row before reporting the failure, so no
        // unpublished send is left for the reaper and no request leaves.
        pending.reconcile_settled().await;
        pending.disarm();
        return Err(ProviderSendAdmissionError::Persistence(sqlx::Error::Protocol(
            "synthetic lost commit acknowledgement".to_string(),
        )));
    }
    // No await between the durable commit and disarming the reconciliation.
    pending.disarm();
    Ok(AdmittedProviderSend { provider_call_id, scope_id })
}

/// Verifies that a send's optional runtime execution really belongs to the
/// named owning execution and task kind.
///
/// The runtime attribution is optional, but a partial pair is a mismatch, and a
/// row from another owner or task is refused rather than recorded. This keeps
/// the exact owner/purpose lineage the settlement path already depended on.
pub async fn provider_send_runtime_attribution_matches(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
    runtime_execution_id: Option<Uuid>,
    runtime_task_kind: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let (Some(runtime_execution_id), Some(runtime_task_kind)) =
        (runtime_execution_id, runtime_task_kind)
    else {
        return Ok(runtime_execution_id.is_none() && runtime_task_kind.is_none());
    };
    let Some(execution) =
        crate::infra::repositories::runtime_repository::get_runtime_execution_by_id(
            postgres,
            runtime_execution_id,
        )
        .await?
    else {
        return Ok(false);
    };
    Ok(execution.owner_kind.as_str() == owning_execution_kind
        && execution.owner_id == owning_execution_id
        && execution.task_kind.as_str() == runtime_task_kind)
}
