use std::collections::{BTreeSet, HashMap};

use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::audit::{
        AuditAssistantCallSummary, AuditAssistantModel, AuditEvent, AuditEventInternalView,
        AuditEventRedactedView, AuditEventSubject,
    },
    infra::repositories::{audit_repository, billing_repository, query_repository},
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct AppendAuditEventCommand {
    pub actor_principal_id: Option<Uuid>,
    pub surface_kind: String,
    pub action_kind: String,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub result_kind: String,
    pub redacted_message: Option<String>,
    pub internal_message: Option<String>,
    pub subjects: Vec<AppendAuditEventSubjectCommand>,
}

#[derive(Debug, Clone)]
pub struct AppendAuditEventSubjectCommand {
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct ListAuditEventSubjectFilter {
    pub knowledge_document_id: Option<Uuid>,
    pub knowledge_revision_id: Option<Uuid>,
    pub context_bundle_id: Option<Uuid>,
    pub query_session_id: Option<Uuid>,
    pub query_execution_id: Option<Uuid>,
    pub runtime_execution_id: Option<Uuid>,
    pub async_operation_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct ListAuditEventsQuery {
    pub actor_principal_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub subject_filter: ListAuditEventSubjectFilter,
    pub surface_kind: Option<String>,
    pub result_kind: Option<String>,
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct AuditEventPage<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct AppendQueryExecutionAuditCommand {
    pub actor_principal_id: Uuid,
    pub surface_kind: String,
    pub request_id: Option<String>,
    pub query_session_id: Uuid,
    pub query_execution_id: Uuid,
    pub runtime_execution_id: Option<Uuid>,
    pub context_bundle_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    /// Truncated user question rendered into the `internal_message`
    /// so an operator scanning the audit log sees what was asked
    /// without cross-referencing the `query_turn_content` table.
    /// Callers should already trim to a sensible length (roughly 200
    /// characters) — longer values are cut again inside the renderer.
    pub question_preview: Option<String>,
}

#[derive(Clone, Default)]
pub struct AuditService;

impl AuditService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Persists a new audit event together with its attached subjects.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository write fails.
    pub async fn append_event(
        &self,
        state: &AppState,
        command: AppendAuditEventCommand,
    ) -> Result<AuditEventInternalView, ApiError> {
        let event = audit_repository::append_audit_event(
            &state.persistence.postgres,
            audit_repository::NewAuditEvent {
                actor_principal_id: command.actor_principal_id,
                surface_kind: command.surface_kind,
                action_kind: command.action_kind,
                request_id: command.request_id,
                trace_id: command.trace_id,
                result_kind: command.result_kind,
                redacted_message: command.redacted_message,
                internal_message: command.internal_message,
            },
            &command
                .subjects
                .into_iter()
                .map(|subject| audit_repository::NewAuditEventSubject {
                    subject_kind: subject.subject_kind,
                    subject_id: subject.subject_id,
                    workspace_id: subject.workspace_id,
                    library_id: subject.library_id,
                    document_id: subject.document_id,
                })
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_internal_event(event))
    }

    /// Persists the canonical assistant/query execution audit event.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository write fails.
    pub async fn append_query_execution_event(
        &self,
        state: &AppState,
        command: AppendQueryExecutionAuditCommand,
    ) -> Result<(), ApiError> {
        let async_operation = state
            .canonical_services
            .ops
            .get_latest_async_operation_by_subject(
                state,
                "query_execution",
                command.query_execution_id,
            )
            .await?;

        let mut subjects = vec![
            self.query_session_subject(
                command.query_session_id,
                command.workspace_id,
                command.library_id,
            ),
            self.query_execution_subject(
                command.query_execution_id,
                command.workspace_id,
                command.library_id,
            ),
            self.knowledge_bundle_subject(
                command.context_bundle_id,
                command.workspace_id,
                command.library_id,
            ),
        ];
        if let Some(runtime_execution_id) = command.runtime_execution_id {
            subjects.push(self.runtime_execution_subject(
                runtime_execution_id,
                Some(command.workspace_id),
                Some(command.library_id),
            ));
        }
        if let Some(operation) = async_operation {
            subjects.push(self.async_operation_subject(
                operation.id,
                command.workspace_id,
                command.library_id,
            ));
        }

        // Billing rows for this execution (query_compile + question
        // embedding + answer generation + any HyDE / CRAG retries)
        // are already persisted by the answer pipeline by the time we
        // reach this point. Pulling the summary here keeps the audit
        // log self-describing: the `internal_message` says how much
        // the turn cost and which models ran, without forcing every
        // consumer to join against `billing_provider_call`.
        let assistant_summary = self
            .list_assistant_call_summaries(state, &[command.query_execution_id])
            .await
            .ok()
            .and_then(|map| map.get(&command.query_execution_id).cloned());

        let internal_message =
            render_query_execution_audit_message(&command, assistant_summary.as_ref());
        let redacted_message = render_query_execution_audit_redacted(assistant_summary.as_ref());

        self.append_event(
            state,
            AppendAuditEventCommand {
                actor_principal_id: Some(command.actor_principal_id),
                surface_kind: command.surface_kind,
                action_kind: "query.execution.run".to_string(),
                request_id: command.request_id,
                trace_id: None,
                result_kind: "succeeded".to_string(),
                redacted_message: Some(redacted_message),
                internal_message: Some(internal_message),
                subjects,
            },
        )
        .await?;

        Ok(())
    }

    /// Lists redacted audit events visible to the caller and optional workspace scope.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_redacted_events(
        &self,
        state: &AppState,
        query: &ListAuditEventsQuery,
    ) -> Result<AuditEventPage<AuditEventRedactedView>, ApiError> {
        let page = audit_repository::list_audit_events(
            &state.persistence.postgres,
            &map_list_query(query),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(AuditEventPage {
            items: page.items.into_iter().map(map_redacted_event).collect(),
            total: page.total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Lists internal audit events visible to the caller and optional workspace scope.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_internal_events(
        &self,
        state: &AppState,
        query: &ListAuditEventsQuery,
    ) -> Result<AuditEventPage<AuditEventInternalView>, ApiError> {
        let page = audit_repository::list_audit_events(
            &state.persistence.postgres,
            &map_list_query(query),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(AuditEventPage {
            items: page.items.into_iter().map(map_internal_event).collect(),
            total: page.total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Lists all subjects attached to a previously recorded audit event.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_event_subjects(
        &self,
        state: &AppState,
        audit_event_id: Uuid,
    ) -> Result<Vec<AuditEventSubject>, ApiError> {
        let rows = audit_repository::list_audit_event_subjects(
            &state.persistence.postgres,
            audit_event_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_event_subject).collect())
    }

    /// Lists audit events with the public event shape.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the audit repository query fails.
    pub async fn list_events(
        &self,
        state: &AppState,
        query: &ListAuditEventsQuery,
    ) -> Result<AuditEventPage<AuditEvent>, ApiError> {
        let page = audit_repository::list_audit_events(
            &state.persistence.postgres,
            &map_list_query(query),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(AuditEventPage {
            items: page.items.into_iter().map(map_event).collect(),
            total: page.total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Loads assistant-call summaries keyed by `query_execution_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when any backing repository query fails.
    pub async fn list_assistant_call_summaries(
        &self,
        state: &AppState,
        query_execution_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, AuditAssistantCallSummary>, ApiError> {
        if query_execution_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut unique_query_execution_ids = query_execution_ids.to_vec();
        unique_query_execution_ids.sort_unstable();
        unique_query_execution_ids.dedup();

        let executions = query_repository::list_executions_by_ids(
            &state.persistence.postgres,
            &unique_query_execution_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let provider_calls = billing_repository::list_provider_call_descriptors_by_execution_ids(
            &state.persistence.postgres,
            "query_execution",
            &unique_query_execution_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let costs = billing_repository::list_execution_costs_by_execution_ids(
            &state.persistence.postgres,
            "query_execution",
            &unique_query_execution_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let mut summaries = HashMap::<Uuid, AuditAssistantCallSummary>::new();
        let mut models_by_execution = HashMap::<Uuid, BTreeSet<(String, String)>>::with_capacity(
            unique_query_execution_ids.len(),
        );

        for execution in executions {
            summaries.insert(
                execution.id,
                AuditAssistantCallSummary {
                    query_execution_id: execution.id,
                    conversation_id: Some(execution.conversation_id),
                    runtime_execution_id: Some(execution.runtime_execution_id),
                    models: Vec::new(),
                    total_cost: None,
                    currency_code: None,
                    provider_call_count: 0,
                },
            );
        }

        for row in provider_calls {
            let summary = summaries.entry(row.owning_execution_id).or_insert_with(|| {
                AuditAssistantCallSummary {
                    query_execution_id: row.owning_execution_id,
                    conversation_id: None,
                    runtime_execution_id: row.runtime_execution_id,
                    models: Vec::new(),
                    total_cost: None,
                    currency_code: None,
                    provider_call_count: 0,
                }
            });
            if summary.runtime_execution_id.is_none() {
                summary.runtime_execution_id = row.runtime_execution_id;
            }
            summary.provider_call_count += 1;
            models_by_execution
                .entry(row.owning_execution_id)
                .or_default()
                .insert((row.provider_kind, row.model_name));
        }

        for row in costs {
            let summary = summaries.entry(row.owning_execution_id).or_insert_with(|| {
                AuditAssistantCallSummary {
                    query_execution_id: row.owning_execution_id,
                    conversation_id: None,
                    runtime_execution_id: None,
                    models: Vec::new(),
                    total_cost: None,
                    currency_code: None,
                    provider_call_count: 0,
                }
            });
            summary.total_cost = Some(row.total_cost);
            summary.currency_code = Some(row.currency_code);
            summary.provider_call_count = i64::from(row.provider_call_count);
        }

        for (execution_id, models) in models_by_execution {
            if let Some(summary) = summaries.get_mut(&execution_id) {
                summary.models = models
                    .into_iter()
                    .map(|(provider_kind, model_name)| AuditAssistantModel {
                        provider_kind,
                        model_name,
                    })
                    .collect();
            }
        }

        for summary in summaries.values_mut() {
            if summary.total_cost.is_none() && summary.provider_call_count == 0 {
                summary.total_cost = Some(Decimal::ZERO);
                summary.currency_code = Some("USD".to_string());
            }
        }

        Ok(summaries)
    }

    #[must_use]
    pub fn query_session_subject(
        &self,
        session_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "query_session".to_string(),
            subject_id: session_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn query_execution_subject(
        &self,
        execution_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "query_execution".to_string(),
            subject_id: execution_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn knowledge_document_subject(
        &self,
        document_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "knowledge_document".to_string(),
            subject_id: document_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: Some(document_id),
        }
    }

    #[must_use]
    pub fn knowledge_bundle_subject(
        &self,
        bundle_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "knowledge_bundle".to_string(),
            subject_id: bundle_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn async_operation_subject(
        &self,
        operation_id: Uuid,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "async_operation".to_string(),
            subject_id: operation_id,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }
    }

    #[must_use]
    pub fn runtime_execution_subject(
        &self,
        runtime_execution_id: Uuid,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> AppendAuditEventSubjectCommand {
        AppendAuditEventSubjectCommand {
            subject_kind: "runtime_execution".to_string(),
            subject_id: runtime_execution_id,
            workspace_id,
            library_id,
            document_id: None,
        }
    }
}

const fn map_subject_filter(
    filter: &ListAuditEventSubjectFilter,
) -> audit_repository::AuditEventSubjectFilter {
    audit_repository::AuditEventSubjectFilter {
        knowledge_document_id: filter.knowledge_document_id,
        knowledge_revision_id: filter.knowledge_revision_id,
        context_bundle_id: filter.context_bundle_id,
        query_session_id: filter.query_session_id,
        query_execution_id: filter.query_execution_id,
        runtime_execution_id: filter.runtime_execution_id,
        async_operation_id: filter.async_operation_id,
    }
}

fn map_list_query(query: &ListAuditEventsQuery) -> audit_repository::ListAuditEventsQuery {
    audit_repository::ListAuditEventsQuery {
        actor_principal_id: query.actor_principal_id,
        workspace_id: query.workspace_id,
        library_id: query.library_id,
        subject_filter: map_subject_filter(&query.subject_filter),
        surface_kind: query.surface_kind.clone(),
        result_kind: query.result_kind.clone(),
        search: query.search.clone(),
        limit: query.limit,
        offset: query.offset,
    }
}

fn map_event(row: audit_repository::AuditEventRow) -> AuditEvent {
    AuditEvent {
        id: row.id,
        actor_principal_id: row.actor_principal_id,
        surface_kind: row.surface_kind,
        action_kind: row.action_kind,
        result_kind: row.result_kind,
        request_id: row.request_id,
        trace_id: row.trace_id,
        created_at: row.created_at,
        redacted_message: row.redacted_message,
    }
}

fn map_redacted_event(row: audit_repository::AuditEventRow) -> AuditEventRedactedView {
    AuditEventRedactedView {
        id: row.id,
        actor_principal_id: row.actor_principal_id,
        surface_kind: row.surface_kind,
        action_kind: row.action_kind,
        result_kind: row.result_kind,
        request_id: row.request_id,
        trace_id: row.trace_id,
        created_at: row.created_at,
        redacted_message: row.redacted_message,
    }
}

fn map_internal_event(row: audit_repository::AuditEventRow) -> AuditEventInternalView {
    AuditEventInternalView {
        id: row.id,
        actor_principal_id: row.actor_principal_id,
        surface_kind: row.surface_kind,
        action_kind: row.action_kind,
        result_kind: row.result_kind,
        request_id: row.request_id,
        trace_id: row.trace_id,
        created_at: row.created_at,
        redacted_message: row.redacted_message,
        internal_message: row.internal_message,
    }
}

/// Maximum characters of the user's question the audit log keeps in
/// the `internal_message`. Large enough to tell what was asked at a
/// glance, small enough that the audit stream stays scannable and
/// does not balloon storage.
const AUDIT_QUESTION_PREVIEW_CHARS: usize = 160;

fn render_query_execution_audit_message(
    command: &AppendQueryExecutionAuditCommand,
    assistant_summary: Option<&AuditAssistantCallSummary>,
) -> String {
    let question_fragment = command
        .question_preview
        .as_deref()
        .map(|raw| truncate_on_char_boundary(raw.trim(), AUDIT_QUESTION_PREVIEW_CHARS))
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, |preview| format!(r#" question: "{preview}""#));

    let ai_fragment = assistant_summary
        .map(|summary| format!(" | {}", format_assistant_call_summary_for_audit(summary)))
        .unwrap_or_default();

    format!(
        "principal {} executed assistant session {}, execution {}, runtime {}, bundle {}{}{}",
        command.actor_principal_id,
        command.query_session_id,
        command.query_execution_id,
        command.runtime_execution_id.map_or_else(
            || "none".to_string(),
            |runtime_execution_id| runtime_execution_id.to_string(),
        ),
        command.context_bundle_id,
        question_fragment,
        ai_fragment,
    )
}

fn render_query_execution_audit_redacted(
    assistant_summary: Option<&AuditAssistantCallSummary>,
) -> String {
    // The redacted view intentionally omits the question text (may
    // contain PII the caller has not opted into exposing) but keeps
    // the AI cost summary, which is already aggregate and safe.
    match assistant_summary {
        Some(summary) => format!(
            "assistant call completed | {}",
            format_assistant_call_summary_for_audit(summary)
        ),
        None => "assistant call completed".to_string(),
    }
}

fn format_assistant_call_summary_for_audit(summary: &AuditAssistantCallSummary) -> String {
    let models_fragment = if summary.models.is_empty() {
        "models: none".to_string()
    } else {
        let rendered = summary
            .models
            .iter()
            .map(|model| format!("{}/{}", model.provider_kind, model.model_name))
            .collect::<Vec<_>>()
            .join(", ");
        format!("models: {rendered}")
    };

    let cost_fragment = match (summary.total_cost, summary.currency_code.as_deref()) {
        (Some(cost), Some(currency)) => format!(", cost: {cost} {currency}"),
        (Some(cost), None) => format!(", cost: {cost}"),
        _ => String::new(),
    };

    format!("calls: {} | {}{}", summary.provider_call_count, models_fragment, cost_fragment,)
}

fn truncate_on_char_boundary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = String::with_capacity(max_chars + 1);
    for (index, ch) in text.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn map_event_subject(row: audit_repository::AuditEventSubjectRow) -> AuditEventSubject {
    let query_session_id = (row.subject_kind == "query_session").then_some(row.subject_id);
    let query_execution_id = (row.subject_kind == "query_execution").then_some(row.subject_id);
    let runtime_execution_id = (row.subject_kind == "runtime_execution").then_some(row.subject_id);
    let context_bundle_id = (row.subject_kind == "knowledge_bundle").then_some(row.subject_id);
    let async_operation_id = (row.subject_kind == "async_operation").then_some(row.subject_id);
    AuditEventSubject {
        audit_event_id: row.audit_event_id,
        subject_kind: row.subject_kind,
        subject_id: row.subject_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        document_id: row.document_id,
        query_session_id,
        query_execution_id,
        runtime_execution_id,
        context_bundle_id,
        async_operation_id,
    }
}
