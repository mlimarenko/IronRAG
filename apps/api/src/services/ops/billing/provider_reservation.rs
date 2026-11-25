use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{agent_runtime::RuntimeTaskKind, ai::AiBindingPurpose},
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        ops::billing::{EMBED_CHUNK_CALL_KIND, ReserveExecutionProviderCallCommand},
    },
};

use crate::services::query::provider_billing::QueryProviderExecutionContext;
use crate::shared::provider_send::{
    ProviderSendAttribution, ProviderSendScope, with_provider_send,
};

/// A durable provider-call reservation that must be completed with the exact
/// response usage before the caller may expose the response.
///
/// Before a response, dropping the guard schedules a conservative terminal
/// transition. After a response, the guard owns the exact usage payload and
/// dropping it schedules idempotent completion retries; a paid response is
/// never downgraded to a no-usage failure.
pub(crate) struct ProviderCallReservation {
    state: AppState,
    /// The ambient scope of this logical call: attribution for every physical
    /// send and the row of the reply the caller is about to settle.
    scope: ProviderSendScope,
    recovery: Option<ProviderCallRecovery>,
}

enum ProviderCallRecovery {
    PreResponse { terminal_state: &'static str },
    ResponseObserved { usage_json: serde_json::Value },
}

pub(crate) type QueryProviderCallCompletionTask = tokio::task::JoinHandle<Result<(), ApiError>>;

/// Owns an in-flight provider-call completion plus the exact response usage needed
/// to retry it after a deadline, cancellation, join failure, or database error.
pub(crate) struct PendingQueryProviderCallCompletion {
    state: AppState,
    provider_call_id: Uuid,
    task: Option<QueryProviderCallCompletionTask>,
    usage_json: Option<serde_json::Value>,
}

impl PendingQueryProviderCallCompletion {
    pub(crate) fn new(
        state: &AppState,
        provider_call_id: Uuid,
        task: QueryProviderCallCompletionTask,
        usage_json: serde_json::Value,
    ) -> Self {
        Self {
            state: state.clone(),
            provider_call_id,
            task: Some(task),
            usage_json: Some(usage_json),
        }
    }

    pub(crate) async fn wait(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<Result<Result<(), ApiError>, tokio::task::JoinError>> {
        let task = self.task.as_mut()?;
        let result = tokio::time::timeout(timeout, task).await.ok()?;
        let _ = self.task.take();
        if matches!(&result, Ok(Ok(()))) {
            let _ = self.usage_json.take();
        }
        Some(result)
    }
}

impl Drop for PendingQueryProviderCallCompletion {
    fn drop(&mut self) {
        let Some(usage_json) = self.usage_json.take() else {
            return;
        };
        spawn_known_usage_completion_reconciliation(
            &self.state,
            self.provider_call_id,
            self.task.take(),
            usage_json,
        );
    }
}

fn spawn_known_usage_completion_reconciliation(
    state: &AppState,
    provider_call_id: Uuid,
    task: Option<QueryProviderCallCompletionTask>,
    usage_json: serde_json::Value,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::error!(
            %provider_call_id,
            "known provider usage could not schedule completion reconciliation"
        );
        return;
    };
    let state = state.clone();
    runtime.spawn(async move {
        let needs_completion_retry = match task {
            Some(task) => match task.await {
                Ok(Ok(())) => false,
                Ok(Err(error)) => {
                    tracing::warn!(
                        %provider_call_id,
                        %error,
                        "provider usage completion failed before background reconciliation"
                    );
                    true
                }
                Err(error) => {
                    tracing::error!(
                        %provider_call_id,
                        %error,
                        "provider usage completion task failed before background reconciliation"
                    );
                    true
                }
            },
            None => true,
        };
        if needs_completion_retry
            && let Err(error) = state
                .application_services
                .billing
                .complete_reserved_provider_call_deferred_rollup_with_retry(
                    &state,
                    provider_call_id,
                    &usage_json,
                )
                .await
        {
            tracing::error!(
                %provider_call_id,
                %error,
                "known provider usage remained uncommitted after bounded reconciliation; reservation stays started for operator repair"
            );
        }
    });
}

/// Exact `call_kind` of a Vision extraction bought by an ingest attempt. The
/// document-accounting stage map reads this value, so it stays one literal.
const INGEST_VISION_CALL_KIND: &str = "vision_extract";

/// The closed ingest provider call a resolved binding purpose buys.
///
/// An ingest attempt pays for chunk embedding and visual extraction only; every
/// other purpose has no ingest owner kind and is refused before a reservation
/// row exists. Deriving the kind here keeps one ingest reservation constructor
/// instead of a parallel implementation per purpose.
fn ingest_call_kind(
    binding: &ResolvedRuntimeBinding,
) -> Result<(AiBindingPurpose, &'static str), ApiError> {
    match binding.binding_purpose {
        AiBindingPurpose::EmbedChunk => Ok((AiBindingPurpose::EmbedChunk, EMBED_CHUNK_CALL_KIND)),
        AiBindingPurpose::ExtractText => {
            Ok((AiBindingPurpose::ExtractText, INGEST_VISION_CALL_KIND))
        }
        other => Err(ApiError::Conflict(format!(
            "provider binding {} has purpose {}, which owns no ingest provider call",
            binding.binding_id,
            other.as_str(),
        ))),
    }
}

impl ProviderCallReservation {
    pub(crate) async fn reserve_knowledge_search(
        state: &AppState,
        library_id: Uuid,
        binding: &ResolvedRuntimeBinding,
    ) -> Result<Self, ApiError> {
        Self::validate_binding(
            binding,
            AiBindingPurpose::EmbedChunk,
            binding.workspace_id,
            library_id,
        )?;
        let search = crate::infra::repositories::billing_repository::BillingKnowledgeSearchRow {
            id: Uuid::now_v7(),
            workspace_id: binding.workspace_id,
            library_id: binding.library_id,
        };
        crate::infra::repositories::billing_repository::create_knowledge_search(
            &state.persistence.postgres,
            &search,
        )
        .await
        .map_err(|error| {
            ApiError::internal_with_log(error, "knowledge search billing admission failed")
        })?;
        Self::reserve_command(
            state,
            ReserveExecutionProviderCallCommand {
                workspace_id: search.workspace_id,
                library_id: search.library_id,
                owning_execution_kind: "knowledge_search".to_string(),
                owning_execution_id: search.id,
                runtime_execution_id: None,
                runtime_task_kind: None,
                binding_id: Some(binding.binding_id),
                provider_catalog_id: binding.provider_catalog_id,
                model_catalog_id: binding.model_catalog_id,
                call_kind: crate::domains::billing::KNOWLEDGE_SEARCH_EMBEDDING_CALL_KIND
                    .to_string(),
            },
        )
        .await
    }

    pub(crate) async fn reserve_document_read(
        state: &AppState,
        read: crate::infra::repositories::billing_repository::BillingDocumentReadRow,
        binding: &ResolvedRuntimeBinding,
    ) -> Result<Self, ApiError> {
        Self::validate_binding(
            binding,
            AiBindingPurpose::ExtractText,
            read.workspace_id,
            read.library_id,
        )?;
        crate::infra::repositories::billing_repository::create_document_read(
            &state.persistence.postgres,
            &read,
        )
        .await
        .map_err(|error| {
            ApiError::internal_with_log(error, "document read billing admission failed")
        })?;
        Self::reserve_command(
            state,
            ReserveExecutionProviderCallCommand {
                workspace_id: read.workspace_id,
                library_id: read.library_id,
                owning_execution_kind: "mcp_document_read".to_string(),
                owning_execution_id: read.id,
                runtime_execution_id: None,
                runtime_task_kind: None,
                binding_id: Some(binding.binding_id),
                provider_catalog_id: binding.provider_catalog_id,
                model_catalog_id: binding.model_catalog_id,
                call_kind: crate::domains::billing::MCP_SOURCE_IMAGE_DESCRIPTION_CALL_KIND
                    .to_string(),
            },
        )
        .await
    }

    fn validate_binding(
        binding: &ResolvedRuntimeBinding,
        expected_binding_purpose: AiBindingPurpose,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<(), ApiError> {
        if binding.binding_purpose != expected_binding_purpose {
            return Err(ApiError::Conflict(format!(
                "provider binding {} has purpose {}, expected {}",
                binding.binding_id,
                binding.binding_purpose.as_str(),
                expected_binding_purpose.as_str(),
            )));
        }
        if binding.library_id != library_id || binding.workspace_id != workspace_id {
            return Err(ApiError::Conflict(format!(
                "provider binding {} does not match execution scope",
                binding.binding_id
            )));
        }
        Ok(())
    }

    pub(crate) async fn reserve(
        state: &AppState,
        context: QueryProviderExecutionContext,
        binding: &ResolvedRuntimeBinding,
        expected_binding_purpose: AiBindingPurpose,
        call_kind: &str,
    ) -> Result<Self, ApiError> {
        Self::validate_binding(
            binding,
            expected_binding_purpose,
            context.workspace_id,
            context.library_id,
        )?;
        Self::reserve_command(
            state,
            ReserveExecutionProviderCallCommand {
                workspace_id: context.workspace_id,
                library_id: context.library_id,
                owning_execution_kind: "query_execution".to_string(),
                owning_execution_id: context.query_execution_id,
                runtime_execution_id: Some(context.runtime_execution_id),
                runtime_task_kind: Some(RuntimeTaskKind::QueryAnswer),
                binding_id: Some(binding.binding_id),
                provider_catalog_id: binding.provider_catalog_id,
                model_catalog_id: binding.model_catalog_id,
                call_kind: call_kind.to_string(),
            },
        )
        .await
    }

    /// Reserves one paid provider call of an ingest attempt before the request
    /// leaves: chunk embedding for an `EmbedChunk` binding and visual extraction
    /// for an `ExtractText` binding.
    ///
    /// The binding must be the active binding for the exact workspace/library,
    /// and the attempt must be a real ingest attempt of that library: the ledger
    /// validates both through the existing execution-scope resolution, so a
    /// synthetic attempt id fails closed. A purpose that owns no ingest provider
    /// call is refused before any row exists. The returned guard must be
    /// completed with the exact response usage, or failed when no response was
    /// observed.
    pub(crate) async fn reserve_ingest_attempt(
        state: &AppState,
        attempt_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
        binding: &ResolvedRuntimeBinding,
    ) -> Result<Self, ApiError> {
        let (expected_binding_purpose, call_kind) = ingest_call_kind(binding)?;
        Self::validate_binding(binding, expected_binding_purpose, workspace_id, library_id)?;
        // An unknown attempt must be refused before any dispatch, as the
        // reservation insert used to prove. The admission owner would otherwise
        // report an attribution refusal instead of the native not-found.
        let attempt = crate::infra::repositories::ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        if attempt.is_none() {
            return Err(ApiError::resource_not_found("ingest_attempt", attempt_id));
        }
        Self::reserve_command(
            state,
            ReserveExecutionProviderCallCommand {
                workspace_id,
                library_id,
                owning_execution_kind: "ingest_attempt".to_string(),
                owning_execution_id: attempt_id,
                runtime_execution_id: None,
                runtime_task_kind: None,
                binding_id: Some(binding.binding_id),
                provider_catalog_id: binding.provider_catalog_id,
                model_catalog_id: binding.model_catalog_id,
                call_kind: call_kind.to_string(),
            },
        )
        .await
    }

    /// Binds one logical call to its attribution and logical id.
    ///
    /// No row and no counter move here: both belong to the physical send and
    /// commit together in admission. A cancellation before the first send
    /// therefore writes nothing and leaves nothing to reconcile.
    pub(crate) async fn reserve_command(
        state: &AppState,
        command: ReserveExecutionProviderCallCommand,
    ) -> Result<Self, ApiError> {
        // No row is written here: the durable row belongs to the physical send
        // and is created by the admission owner together with its counter debit.
        // The guard carries only the attribution and the id the first send
        // reuses, so a cancellation at this point writes nothing.
        let attribution = ProviderSendAttribution {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            owning_execution_kind: command.owning_execution_kind,
            owning_execution_id: command.owning_execution_id,
            call_kind: command.call_kind,
            binding_id: command.binding_id,
            provider_catalog_id: command.provider_catalog_id,
            model_catalog_id: command.model_catalog_id,
            runtime_execution_id: command.runtime_execution_id,
            runtime_task_kind: command.runtime_task_kind.map(RuntimeTaskKind::as_str),
        };

        Ok(Self {
            state: state.clone(),
            scope: ProviderSendScope::new(attribution, Uuid::now_v7()),
            recovery: Some(ProviderCallRecovery::PreResponse { terminal_state: "canceled" }),
        })
    }

    /// The id that labels this logical call. The first physical send reuses it
    /// as its own row id; a retry claims a new row.
    #[must_use]
    pub(crate) fn provider_call_id(&self) -> Uuid {
        self.scope.logical_id()
    }

    /// The ambient scope to install around the gateway call.
    #[must_use]
    pub(crate) fn send_scope(&self) -> ProviderSendScope {
        self.scope.clone()
    }

    /// Runs one gateway call with this logical call's scope installed.
    pub(crate) async fn scoped<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        with_provider_send(self.scope.clone(), future).await
    }

    /// Persist exact usage and the completed state before any caller may
    /// persist a terminal query outcome. Derived cost repair is intentionally
    /// deferred; the provider-call completion transaction is awaited here.
    pub(crate) async fn complete(
        &mut self,
        usage_json: &serde_json::Value,
    ) -> Result<(), ApiError> {
        let Some(provider_call_id) = self.scope.current_send_id() else {
            // No physical send was admitted, so there is no row to settle.
            self.recovery = None;
            return Ok(());
        };
        let capture_usage = match self.recovery.as_ref() {
            None => return Ok(()),
            Some(ProviderCallRecovery::ResponseObserved { usage_json: observed_usage })
                if observed_usage != usage_json =>
            {
                return Err(ApiError::Conflict(format!(
                    "provider call {provider_call_id} cannot be completed with two different usage payloads"
                )));
            }
            Some(ProviderCallRecovery::PreResponse { .. }) => true,
            Some(ProviderCallRecovery::ResponseObserved { .. }) => false,
        };
        if capture_usage {
            // Own the exact response usage before the first fallible
            // completion attempt. From this point onward, cancellation or a
            // transient database failure must retry completion and may never
            // downgrade the paid response to a no-usage failure.
            self.recovery =
                Some(ProviderCallRecovery::ResponseObserved { usage_json: usage_json.clone() });
        }
        self.state
            .application_services
            .billing
            .complete_reserved_provider_call_deferred_rollup_with_retry(
                &self.state,
                provider_call_id,
                usage_json,
            )
            .await?;
        self.recovery = None;
        Ok(())
    }

    /// Mark a request that failed before returning provider usage. This is
    /// awaited on ordinary error paths; the drop fallback retries after an
    /// interrupted or ambiguous database operation.
    pub(crate) async fn fail(&mut self) -> Result<(), ApiError> {
        let Some(provider_call_id) = self.scope.current_send_id() else {
            self.recovery = None;
            return Ok(());
        };
        match self.recovery.as_mut() {
            None => return Ok(()),
            Some(ProviderCallRecovery::ResponseObserved { .. }) => {
                return Err(ApiError::Conflict(format!(
                    "provider call {provider_call_id} already observed a response and must retain its usage"
                )));
            }
            Some(ProviderCallRecovery::PreResponse { terminal_state }) => {
                *terminal_state = "failed";
            }
        }
        self.state
            .application_services
            .billing
            .finish_reserved_provider_call_without_usage(&self.state, provider_call_id, "failed")
            .await?;
        self.recovery = None;
        Ok(())
    }
}

impl Drop for ProviderCallReservation {
    fn drop(&mut self) {
        let Some(recovery) = self.recovery.take() else {
            return;
        };
        // A logical call that never admitted a send owns no row: nothing was
        // debited and nothing must be terminalized.
        let Some(provider_call_id) = self.scope.current_send_id() else {
            return;
        };
        let terminal_state = match recovery {
            ProviderCallRecovery::ResponseObserved { usage_json } => {
                spawn_known_usage_completion_reconciliation(
                    &self.state,
                    provider_call_id,
                    None,
                    usage_json,
                );
                return;
            }
            ProviderCallRecovery::PreResponse { terminal_state } => terminal_state,
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                %provider_call_id,
                "pre-response provider-call reservation dropped without a reconciliation runtime"
            );
            return;
        };
        let state = self.state.clone();
        runtime.spawn(async move {
            if let Err(error) = state
                .application_services
                .billing
                .finish_reserved_provider_call_without_usage(
                    &state,
                    provider_call_id,
                    terminal_state,
                )
                .await
            {
                tracing::error!(
                    %provider_call_id,
                    terminal_state,
                    %error,
                    "failed to reconcile interrupted pre-response provider-call reservation"
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(binding_purpose: AiBindingPurpose) -> ResolvedRuntimeBinding {
        ResolvedRuntimeBinding {
            admission: std::sync::Arc::new(
                crate::integrations::provider_admission::UnqualifiedTestAdmission,
            ),
            binding_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            binding_purpose,
            provider_catalog_id: Uuid::now_v7(),
            provider_kind: "openai".to_string(),
            provider_base_url: None,
            provider_api_style: "openai_compatible".to_string(),
            account_id: Uuid::now_v7(),
            api_key: None,
            model_catalog_id: Uuid::now_v7(),
            model_name: "gpt-vision".to_string(),
            effective_embedding_dimensions: None,
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        }
    }

    #[test]
    fn ingest_provider_call_kind_is_closed_to_embedding_and_vision() {
        let (purpose, call_kind) = ingest_call_kind(&binding(AiBindingPurpose::EmbedChunk))
            .expect("embedding ingest call");
        assert_eq!(purpose, AiBindingPurpose::EmbedChunk);
        assert_eq!(call_kind, EMBED_CHUNK_CALL_KIND);

        let (purpose, call_kind) =
            ingest_call_kind(&binding(AiBindingPurpose::ExtractText)).expect("vision ingest call");
        assert_eq!(purpose, AiBindingPurpose::ExtractText);
        assert_eq!(call_kind, "vision_extract");

        for unsupported in AiBindingPurpose::ALL {
            if matches!(unsupported, AiBindingPurpose::EmbedChunk | AiBindingPurpose::ExtractText) {
                continue;
            }
            assert!(
                matches!(ingest_call_kind(&binding(unsupported)), Err(ApiError::Conflict(_))),
                "{} must own no ingest provider call",
                unsupported.as_str(),
            );
        }
    }
}
