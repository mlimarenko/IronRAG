use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeExecutionOwnerKind {
    QueryExecution,
    GraphExtractionAttempt,
    StructuredPreparation,
    TechnicalFactExtraction,
}

impl RuntimeExecutionOwnerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryExecution => "query_execution",
            Self::GraphExtractionAttempt => "graph_extraction_attempt",
            Self::StructuredPreparation => "structured_preparation",
            Self::TechnicalFactExtraction => "technical_fact_extraction",
        }
    }
}

impl std::str::FromStr for RuntimeExecutionOwnerKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "query_execution" => Ok(Self::QueryExecution),
            "graph_extraction_attempt" => Ok(Self::GraphExtractionAttempt),
            "structured_preparation" => Ok(Self::StructuredPreparation),
            "technical_fact_extraction" => Ok(Self::TechnicalFactExtraction),
            other => Err(format!("unsupported runtime execution owner kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeExecutionOwner {
    pub owner_kind: RuntimeExecutionOwnerKind,
    pub owner_id: Uuid,
}

impl RuntimeExecutionOwner {
    #[must_use]
    pub const fn new(owner_kind: RuntimeExecutionOwnerKind, owner_id: Uuid) -> Self {
        Self { owner_kind, owner_id }
    }

    #[must_use]
    pub const fn query_execution(owner_id: Uuid) -> Self {
        Self::new(RuntimeExecutionOwnerKind::QueryExecution, owner_id)
    }

    #[must_use]
    pub const fn graph_extraction_attempt(owner_id: Uuid) -> Self {
        Self::new(RuntimeExecutionOwnerKind::GraphExtractionAttempt, owner_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTaskKind {
    QueryCompile,
    QueryPlan,
    QueryRerank,
    QueryAnswer,
    QueryVerify,
    GraphExtract,
    StructuredPrepare,
    TechnicalFactExtract,
}

impl RuntimeTaskKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryCompile => "query_compile",
            Self::QueryPlan => "query_plan",
            Self::QueryRerank => "query_rerank",
            Self::QueryAnswer => "query_answer",
            Self::QueryVerify => "query_verify",
            Self::GraphExtract => "graph_extract",
            Self::StructuredPrepare => "structured_prepare",
            Self::TechnicalFactExtract => "technical_fact_extract",
        }
    }
}

impl std::str::FromStr for RuntimeTaskKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "query_compile" => Ok(Self::QueryCompile),
            "query_plan" => Ok(Self::QueryPlan),
            "query_rerank" => Ok(Self::QueryRerank),
            "query_answer" => Ok(Self::QueryAnswer),
            "query_verify" => Ok(Self::QueryVerify),
            "graph_extract" => Ok(Self::GraphExtract),
            "structured_prepare" => Ok(Self::StructuredPrepare),
            "technical_fact_extract" => Ok(Self::TechnicalFactExtract),
            other => Err(format!("unsupported runtime task kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSurfaceKind {
    Rest,
    Stream,
    Mcp,
    Worker,
    Internal,
}

impl RuntimeSurfaceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Stream => "stream",
            Self::Mcp => "mcp",
            Self::Worker => "worker",
            Self::Internal => "internal",
        }
    }
}

impl std::str::FromStr for RuntimeSurfaceKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rest" => Ok(Self::Rest),
            "stream" => Ok(Self::Stream),
            "mcp" => Ok(Self::Mcp),
            "worker" => Ok(Self::Worker),
            "internal" => Ok(Self::Internal),
            other => Err(format!("unsupported runtime surface kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOutputMode {
    Text,
    Structured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLifecycleState {
    Accepted,
    Running,
    Completed,
    Recovered,
    Failed,
    Canceled,
}

impl RuntimeLifecycleState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Recovered | Self::Failed | Self::Canceled)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Recovered => "recovered",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

impl std::str::FromStr for RuntimeLifecycleState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "accepted" => Ok(Self::Accepted),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "recovered" => Ok(Self::Recovered),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(format!("unsupported runtime lifecycle state: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStageKind {
    Compile,
    Plan,
    Retrieve,
    Rerank,
    AssembleContext,
    Answer,
    Verify,
    ExtractGraph,
    StructuredPrepare,
    TechnicalFactExtract,
    Recovery,
    Persist,
}

impl RuntimeStageKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::Plan => "plan",
            Self::Retrieve => "retrieve",
            Self::Rerank => "rerank",
            Self::AssembleContext => "assemble_context",
            Self::Answer => "answer",
            Self::Verify => "verify",
            Self::ExtractGraph => "extract_graph",
            Self::StructuredPrepare => "structured_prepare",
            Self::TechnicalFactExtract => "technical_fact_extract",
            Self::Recovery => "recovery",
            Self::Persist => "persist",
        }
    }
}

impl std::str::FromStr for RuntimeStageKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compile" => Ok(Self::Compile),
            "plan" => Ok(Self::Plan),
            "retrieve" => Ok(Self::Retrieve),
            "rerank" => Ok(Self::Rerank),
            "assemble_context" => Ok(Self::AssembleContext),
            "answer" => Ok(Self::Answer),
            "verify" => Ok(Self::Verify),
            "extract_graph" => Ok(Self::ExtractGraph),
            "structured_prepare" => Ok(Self::StructuredPrepare),
            "technical_fact_extract" => Ok(Self::TechnicalFactExtract),
            "recovery" => Ok(Self::Recovery),
            "persist" => Ok(Self::Persist),
            other => Err(format!("unsupported runtime stage kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStageState {
    Pending,
    Running,
    Completed,
    Recovered,
    Failed,
    Canceled,
}

impl RuntimeStageState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Recovered => "recovered",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

impl std::str::FromStr for RuntimeStageState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "recovered" => Ok(Self::Recovered),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(format!("unsupported runtime stage state: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeActionKind {
    DeterministicStep,
    ModelRequest,
    ToolRequest,
    ToolResult,
    RecoveryAttempt,
    PersistenceWrite,
}

impl RuntimeActionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeterministicStep => "deterministic_step",
            Self::ModelRequest => "model_request",
            Self::ToolRequest => "tool_request",
            Self::ToolResult => "tool_result",
            Self::RecoveryAttempt => "recovery_attempt",
            Self::PersistenceWrite => "persistence_write",
        }
    }
}

impl std::str::FromStr for RuntimeActionKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deterministic_step" => Ok(Self::DeterministicStep),
            "model_request" => Ok(Self::ModelRequest),
            "tool_request" => Ok(Self::ToolRequest),
            "tool_result" => Ok(Self::ToolResult),
            "recovery_attempt" => Ok(Self::RecoveryAttempt),
            "persistence_write" => Ok(Self::PersistenceWrite),
            other => Err(format!("unsupported runtime action kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeActionState {
    Pending,
    Running,
    Completed,
    Recovered,
    Failed,
    Canceled,
}

impl RuntimeActionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Recovered => "recovered",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

impl std::str::FromStr for RuntimeActionState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "recovered" => Ok(Self::Recovered),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(format!("unsupported runtime action state: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDecisionTargetKind {
    ModelRequest,
    ToolRequest,
    ToolResult,
    StageTransition,
    FinalOutcome,
}

impl RuntimeDecisionTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelRequest => "model_request",
            Self::ToolRequest => "tool_request",
            Self::ToolResult => "tool_result",
            Self::StageTransition => "stage_transition",
            Self::FinalOutcome => "final_outcome",
        }
    }
}

impl std::str::FromStr for RuntimeDecisionTargetKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "model_request" => Ok(Self::ModelRequest),
            "tool_request" => Ok(Self::ToolRequest),
            "tool_result" => Ok(Self::ToolResult),
            "stage_transition" => Ok(Self::StageTransition),
            "final_outcome" => Ok(Self::FinalOutcome),
            other => Err(format!("unsupported runtime decision target kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDecisionKind {
    Allow,
    Reject,
    Terminate,
}

impl RuntimeDecisionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Reject => "reject",
            Self::Terminate => "terminate",
        }
    }
}

impl std::str::FromStr for RuntimeDecisionKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(Self::Allow),
            "reject" => Ok(Self::Reject),
            "terminate" => Ok(Self::Terminate),
            other => Err(format!("unsupported runtime decision kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRecoveryPolicy {
    None,
    VisibleBounded { max_attempts: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeOverrideBudget {
    pub max_turns: Option<u8>,
    pub max_parallel_actions: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeExecution {
    pub id: Uuid,
    pub owner_kind: RuntimeExecutionOwnerKind,
    pub owner_id: Uuid,
    pub task_kind: RuntimeTaskKind,
    pub surface_kind: RuntimeSurfaceKind,
    pub contract_name: String,
    pub contract_version: String,
    pub lifecycle_state: RuntimeLifecycleState,
    pub active_stage: Option<RuntimeStageKind>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
    pub accepted_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStageRecord {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_kind: RuntimeStageKind,
    pub stage_ordinal: i32,
    pub attempt_no: i32,
    pub stage_state: RuntimeStageState,
    pub deterministic: bool,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub input_summary_json: serde_json::Value,
    pub output_summary_json: serde_json::Value,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeActionRecord {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_record_id: Uuid,
    pub action_kind: RuntimeActionKind,
    pub action_ordinal: i32,
    pub action_state: RuntimeActionState,
    pub provider_binding_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub usage_json: Option<serde_json::Value>,
    pub summary_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicyDecision {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub stage_record_id: Option<Uuid>,
    pub action_record_id: Option<Uuid>,
    pub target_kind: RuntimeDecisionTargetKind,
    pub decision_kind: RuntimeDecisionKind,
    pub reason_code: String,
    pub reason_summary_redacted: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicyDecisionSummary {
    pub target_kind: RuntimeDecisionTargetKind,
    pub decision_kind: RuntimeDecisionKind,
    pub reason_code: String,
    pub reason_summary_redacted: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicySummary {
    pub allow_count: usize,
    pub reject_count: usize,
    pub terminate_count: usize,
    pub recent_decisions: Vec<RuntimePolicyDecisionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeExecutionSummary {
    pub runtime_execution_id: Uuid,
    pub lifecycle_state: RuntimeLifecycleState,
    pub active_stage: Option<RuntimeStageKind>,
    pub turn_budget: i32,
    pub turn_count: i32,
    pub parallel_action_limit: i32,
    pub failure_code: Option<String>,
    pub failure_summary_redacted: Option<String>,
    pub policy_summary: RuntimePolicySummary,
    pub accepted_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl From<&RuntimeExecution> for RuntimeExecutionSummary {
    fn from(value: &RuntimeExecution) -> Self {
        Self {
            runtime_execution_id: value.id,
            lifecycle_state: value.lifecycle_state,
            active_stage: value.active_stage,
            turn_budget: value.turn_budget,
            turn_count: value.turn_count,
            parallel_action_limit: value.parallel_action_limit,
            failure_code: value.failure_code.clone(),
            failure_summary_redacted: value.failure_summary_redacted.clone(),
            policy_summary: RuntimePolicySummary::default(),
            accepted_at: value.accepted_at,
            completed_at: value.completed_at,
        }
    }
}
