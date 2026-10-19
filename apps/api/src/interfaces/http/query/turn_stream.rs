use super::*;
use crate::shared::cancellation::AbortOnDrop;

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum AssistantTurnStreamEvent {
    Activity {
        event: AssistantActivityEvent,
    },
    Completed {
        detail: Box<ironrag_contracts::assistant::AssistantExecutionDetail>,
    },
    /// `code` is the same stable slug every other error surface carries, so a
    /// client can say what happened in its own words. Sending prose alone meant
    /// the assistant screen had nothing to translate and fell back to a generic
    /// apology that told the reader to try again — advice that is wrong for
    /// every reason a turn actually fails.
    Failed {
        code: &'static str,
        message: String,
    },
}

#[derive(Debug, Serialize)]
pub(super) struct AssistantActivityEvent {
    #[serde(rename = "type")]
    pub(super) event_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) iteration: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) provider_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) has_final_answer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) child_execution_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result_preview: Option<String>,
}

const ASSISTANT_TURN_ACTIVITY_INTERVAL: Duration = Duration::from_secs(5);
const ASSISTANT_TURN_STREAM_BUFFER: usize = 512;
pub(super) const ASSISTANT_TURN_TERMINAL_EVENT_RESERVE: usize = 8;
const ASSISTANT_ACTIVITY_DRAIN_GRACE: Duration = Duration::from_millis(250);
const ASSISTANT_PANIC_FAILURE_SEND_GRACE: Duration = Duration::from_secs(1);

#[tracing::instrument(
    level = "info",
    name = "http.create_session_turn_event_stream",
    skip_all,
    fields(session_id = %session_id)
)]
pub(super) async fn create_session_turn_event_stream(
    auth: AuthContext,
    state: AppState,
    session_id: Uuid,
    payload: CreateSessionTurnRequest,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<AssistantTurnStreamEvent>(ASSISTANT_TURN_STREAM_BUFFER);
    let state_for_task = state.clone();
    let auth_for_task = auth.clone();

    tokio::spawn(async move {
        let panic_sender = sender.clone();
        let producer = async move {
            send_assistant_activity(
                &sender,
                AssistantActivityEvent {
                    event_type: "started",
                    deadline_ms: Some(ASSISTANT_AGENT_LOOP_DEADLINE_MS),
                    iteration: None,
                    provider_kind: None,
                    model_name: None,
                    tool_call_count: None,
                    has_final_answer: None,
                    tool_name: None,
                    elapsed_ms: None,
                    is_error: None,
                    child_execution_id: None,
                    result_preview: None,
                },
            );

            let (agent_activity_tx, mut agent_activity_rx) =
                tokio::sync::mpsc::channel::<AgentLoopActivityEvent>(256);
            let agent_activity_sender = sender.clone();
            let mut agent_activity_task = tokio::spawn(async move {
                while let Some(event) = agent_activity_rx.recv().await {
                    send_assistant_activity(&agent_activity_sender, map_agent_loop_activity(event));
                }
            });

            let progress_started_at = Instant::now();
            let mut progress_task = spawn_progress_reporter(sender.clone(), progress_started_at);

            let result = execute_ui_session_turn(
                state_for_task.clone(),
                auth_for_task.clone(),
                session_id,
                payload,
                Some(agent_activity_tx),
            )
            .await;
            if let Some(handle) = progress_task.disarm() {
                handle.abort();
            }
            if tokio::time::timeout(ASSISTANT_ACTIVITY_DRAIN_GRACE, &mut agent_activity_task)
                .await
                .is_err()
            {
                agent_activity_task.abort();
            }

            match result {
                Ok(outcome) => {
                    send_assistant_activity(
                        &sender,
                        AssistantActivityEvent {
                            event_type: "model_response",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: Some(true),
                            tool_name: None,
                            elapsed_ms: Some(progress_started_at.elapsed().as_millis() as u64),
                            is_error: Some(false),
                            child_execution_id: Some(outcome.execution.id),
                            result_preview: Some(format!(
                                "verification={}",
                                verification_state_stream_label(&outcome.verification_state)
                            )),
                        },
                    );
                    append_query_execution_audit(
                        state_for_task.clone(),
                        auth_for_task.principal_id,
                        "ui",
                        &outcome,
                    )
                    .await;
                    send_assistant_activity(
                        &sender,
                        AssistantActivityEvent {
                            event_type: "persisting",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: None,
                            tool_name: None,
                            elapsed_ms: None,
                            is_error: None,
                            child_execution_id: None,
                            result_preview: None,
                        },
                    );
                    send_required_turn_stream_event(
                        &sender,
                        AssistantTurnStreamEvent::Completed {
                            detail: Box::new(map_ui_turn_execution_response(outcome)),
                        },
                    )
                    .await;
                }
                Err(error) => {
                    send_assistant_activity(
                        &sender,
                        AssistantActivityEvent {
                            event_type: "model_response",
                            deadline_ms: None,
                            iteration: None,
                            provider_kind: None,
                            model_name: None,
                            tool_call_count: None,
                            has_final_answer: Some(false),
                            tool_name: None,
                            elapsed_ms: Some(progress_started_at.elapsed().as_millis() as u64),
                            is_error: Some(true),
                            child_execution_id: None,
                            result_preview: Some(error.to_string()),
                        },
                    );
                    send_required_turn_stream_event(
                        &sender,
                        AssistantTurnStreamEvent::Failed {
                            code: error.kind(),
                            message: error.to_string(),
                        },
                    )
                    .await;
                }
            }
        };
        if let Err(panic) = Box::pin(AssertUnwindSafe(producer).catch_unwind()).await {
            tracing::error!(
                panic = %panic_payload_message(panic.as_ref()),
                "assistant turn stream producer panicked"
            );
            let _ = tokio::time::timeout(
                ASSISTANT_PANIC_FAILURE_SEND_GRACE,
                send_required_turn_stream_event(
                    &panic_sender,
                    AssistantTurnStreamEvent::Failed {
                        code: "internal",
                        message: "assistant turn stream failed unexpectedly".to_string(),
                    },
                ),
            )
            .await;
        }
    });

    let stream = stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|payload| {
            let event = Event::default()
                .event("assistant_turn")
                .json_data(payload)
                .unwrap_or_else(|error| {
                    Event::default()
                        .event("assistant_turn")
                        .data(format!(
                            r#"{{"type":"failed","code":"internal","message":"failed to serialize stream event: {error}"}}"#
                        ))
                });
            (Ok(event), receiver)
        })
    });

    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)).text("keep-alive")))
}

/// Spawns the periodic `working` progress reporter for a turn stream and wraps
/// its handle in an [`AbortOnDrop`] guard, so a panic or early return between
/// the spawn and the explicit stop cannot leak the loop forever (IRG-203).
/// Extracted from [`create_session_turn_event_stream`] so the abort-on-drop
/// contract is unit-testable without constructing a whole turn.
pub(super) fn spawn_progress_reporter(
    sender: Sender<AssistantTurnStreamEvent>,
    progress_started_at: Instant,
) -> AbortOnDrop<()> {
    AbortOnDrop::new(tokio::spawn(async move {
        loop {
            sleep(ASSISTANT_TURN_ACTIVITY_INTERVAL).await;
            send_assistant_activity(
                &sender,
                AssistantActivityEvent {
                    event_type: "working",
                    deadline_ms: None,
                    iteration: None,
                    provider_kind: None,
                    model_name: None,
                    tool_call_count: None,
                    has_final_answer: None,
                    tool_name: None,
                    elapsed_ms: Some(progress_started_at.elapsed().as_millis() as u64),
                    is_error: None,
                    child_execution_id: None,
                    result_preview: None,
                },
            );
        }
    }))
}

pub(super) async fn execute_ui_session_turn(
    state: AppState,
    auth: AuthContext,
    session_id: Uuid,
    payload: CreateSessionTurnRequest,
    agent_activity_tx: Option<tokio::sync::mpsc::Sender<AgentLoopActivityEvent>>,
) -> Result<crate::services::query::service::QueryTurnExecutionResult, ApiError> {
    let _ = load_query_session_and_authorize(&auth, &state, session_id, POLICY_QUERY_RUN).await?;
    let query_service = state.application_services.query.clone();
    query_service
        .execute_assistant_agent_turn(
            &state,
            &auth,
            ExecuteConversationTurnCommand {
                conversation_id: session_id,
                author_principal_id: Some(auth.principal_id),
                surface_kind: RuntimeSurfaceKind::Ui,
                content_text: payload.content_text,
                external_prior_turns: Vec::new(),
                top_k: resolve_query_turn_top_k(payload.top_k),
                include_debug: payload.include_debug.unwrap_or(false),
            },
            agent_activity_tx,
        )
        .await
}

pub(super) async fn send_required_turn_stream_event(
    sender: &Sender<AssistantTurnStreamEvent>,
    event: AssistantTurnStreamEvent,
) {
    let _ = sender.send(event).await;
}

pub(super) fn send_assistant_activity(
    sender: &Sender<AssistantTurnStreamEvent>,
    event: AssistantActivityEvent,
) {
    if sender.capacity() <= ASSISTANT_TURN_TERMINAL_EVENT_RESERVE {
        return;
    }
    let _ = sender.try_send(AssistantTurnStreamEvent::Activity { event });
}

pub(super) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

pub(super) fn map_agent_loop_activity(event: AgentLoopActivityEvent) -> AssistantActivityEvent {
    match event {
        AgentLoopActivityEvent::ModelRequest { iteration, provider_kind, model_name } => {
            AssistantActivityEvent {
                event_type: "model_request",
                deadline_ms: None,
                iteration: Some(iteration),
                provider_kind: Some(provider_kind),
                model_name: Some(model_name),
                tool_call_count: None,
                has_final_answer: None,
                tool_name: None,
                elapsed_ms: None,
                is_error: None,
                child_execution_id: None,
                result_preview: None,
            }
        }
        AgentLoopActivityEvent::ModelResponse {
            iteration,
            provider_kind,
            model_name,
            tool_call_count,
            has_final_answer,
        } => AssistantActivityEvent {
            event_type: "model_response",
            deadline_ms: None,
            iteration: Some(iteration),
            provider_kind: Some(provider_kind),
            model_name: Some(model_name),
            tool_call_count: Some(tool_call_count),
            has_final_answer: Some(has_final_answer),
            tool_name: None,
            elapsed_ms: None,
            is_error: None,
            child_execution_id: None,
            result_preview: None,
        },
        AgentLoopActivityEvent::ToolCallStarted { iteration, tool_name } => {
            AssistantActivityEvent {
                event_type: "tool_call_started",
                deadline_ms: None,
                iteration: Some(iteration),
                provider_kind: None,
                model_name: None,
                tool_call_count: None,
                has_final_answer: None,
                tool_name: Some(tool_name),
                elapsed_ms: None,
                is_error: None,
                child_execution_id: None,
                result_preview: None,
            }
        }
        AgentLoopActivityEvent::ToolCallFinished {
            iteration,
            tool_name,
            elapsed_ms,
            is_error,
            child_execution_id,
            result_preview,
        } => AssistantActivityEvent {
            event_type: "tool_call_finished",
            deadline_ms: None,
            iteration: Some(iteration),
            provider_kind: None,
            model_name: None,
            tool_call_count: None,
            has_final_answer: None,
            tool_name: Some(tool_name),
            elapsed_ms: Some(elapsed_ms),
            is_error: Some(is_error),
            child_execution_id,
            result_preview,
        },
    }
}

pub(super) const fn verification_state_stream_label(
    state: &QueryVerificationState,
) -> &'static str {
    match state {
        QueryVerificationState::NotRun => "not_run",
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
    }
}

pub(super) fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers.get(header::ACCEPT).and_then(|value| value.to_str().ok()).is_some_and(|value| {
        value.split(',').any(|segment| segment.trim().eq_ignore_ascii_case("text/event-stream"))
    })
}
