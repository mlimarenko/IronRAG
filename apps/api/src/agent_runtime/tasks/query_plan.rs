use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::{
        agent_runtime::{
            RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
            RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
    },
    services::query_planner::{QueryPlanFailure, QueryPlanTaskInput, RuntimeQueryPlan},
};

const QUERY_PLAN_STAGE_CATALOG: &[RuntimeStageKind] = &[RuntimeStageKind::Plan];

pub struct QueryPlanTask;

impl RuntimeTask for QueryPlanTask {
    type Input = QueryPlanTaskInput;
    type Success = RuntimeQueryPlan;
    type Failure = QueryPlanFailure;

    const CONTRACT_NAME: &'static str = "query_plan";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::QueryPlan,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::QueryPlan),
            machine_consumed: true,
            max_turns: 1,
            max_parallel_actions: 1,
            stage_catalog: QUERY_PLAN_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Structured,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        QueryPlanFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl StructuredRuntimeTask for QueryPlanTask {}
