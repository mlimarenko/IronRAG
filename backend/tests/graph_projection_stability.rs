use rustrag_backend::domains::runtime_graph::RuntimeGraphWriteFailureKind;
use rustrag_backend::infra::graph_store::GraphProjectionWriteError;
use rustrag_backend::services::graph_projection_guard::{
    GraphProjectionFailureDecision, GraphProjectionGuardService,
};

#[test]
fn retryable_contention_stays_on_retry_path_before_exhaustion() {
    let service = GraphProjectionGuardService::new(3);
    let decision = service.classify_write_error(
        &GraphProjectionWriteError::ProjectionContention {
            message: "neo4j deadlock detected".to_string(),
        },
        1,
    );

    assert_eq!(decision, GraphProjectionFailureDecision::RetryContention);
}

#[test]
fn exhausted_contention_is_classified_explicitly() {
    let service = GraphProjectionGuardService::new(3);
    let decision = service.classify_write_error(
        &GraphProjectionWriteError::ProjectionContention {
            message: "neo4j deadlock detected".to_string(),
        },
        3,
    );

    assert_eq!(
        decision,
        GraphProjectionFailureDecision::FailExplicitly(
            RuntimeGraphWriteFailureKind::ProjectionContention,
        )
    );
}

#[test]
fn graph_integrity_failure_is_not_treated_as_retryable_contention() {
    let service = GraphProjectionGuardService::new(3);
    let decision = service.classify_write_error(
        &GraphProjectionWriteError::GraphPersistenceIntegrity {
            message: "foreign key violation".to_string(),
        },
        1,
    );

    assert_eq!(
        decision,
        GraphProjectionFailureDecision::FailExplicitly(
            RuntimeGraphWriteFailureKind::GraphPersistenceIntegrity,
        )
    );
}
