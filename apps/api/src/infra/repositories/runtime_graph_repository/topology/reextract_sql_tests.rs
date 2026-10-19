use super::{
    GRAPH_REEXTRACT_PROJECTION_REPAIR_PREDICATE, GRAPH_REEXTRACT_VERSION_RECOVERY_PREDICATE,
    GraphReextractSelectionMode, graph_rebuild_document_completions_query,
    graph_reextract_candidates_query, graph_reextract_progress_query,
};

/// Fragments the operator version-recovery query must carry: completed
/// extraction stages stamped by the current extractor and contract, current
/// readable/active document heads, and the projection-evidence boundary.
const VERSION_RECOVERY_REQUIRED_FRAGMENTS: &[&str] = &[
    // only a completed stage stamped by the current extractor may satisfy recovery
    "event.details_json ->> 'extractionVersion' = $2",
    "job.queue_state = 'completed'::ingest_queue_state",
    "attempt.attempt_state = 'succeeded'::ingest_attempt_state",
    "jsonb_typeof(graph_contribution_count_json) = 'number'",
    "from latest_current_completion",
    "d.document_state = 'active'::content_document_state",
    "d.deleted_at is null",
    "h.readable_revision_id = h.active_revision_id",
    "from runtime_graph_node node",
    "from runtime_graph_snapshot snapshot",
    "node.library_id = $1",
    "node.document_id = ready.document_id",
    "node.node_type = 'document'",
    "join runtime_graph_evidence evidence",
    "evidence.target_id = node.id",
    "evidence.document_id = ready.document_id",
    "evidence.revision_id = ready.active_revision_id",
    "coalesce(",
    "snapshot.projection_version",
    "state.graph_contribution_count_text::numeric > 0",
    "not state.has_active_revision_publication",
    GRAPH_REEXTRACT_VERSION_RECOVERY_PREDICATE,
    // explicit recovery must treat a completion stamped by another extraction
    // contract as a coverage gap, read from the stage event's contract stamp
    "state.extraction_contract_version_text is distinct from $8",
    "event.details_json ->> 'extractionContractVersion'",
    "order by attempt.attempted_queue_rank asc nulls first,\n              attempt.attempted_job_id asc nulls first,\n              state.document_id",
    "state.automatic_terminal_failure_count < $6",
    "clock_timestamp()",
    "make_interval(secs => $7::double precision)",
    "state.last_automatic_terminal_failure_completed_at",
    "job.priority = $5",
    "j.knowledge_revision_id = state.active_revision_id",
    "limit $4",
];

/// Version recovery must not borrow the automatic-repair predicate or the
/// cancellation gate.
const VERSION_RECOVERY_FORBIDDEN_FRAGMENTS: &[&str] =
    &["not state.has_canceled_graph_refresh", GRAPH_REEXTRACT_PROJECTION_REPAIR_PREDICATE];

fn assert_version_recovery_query_contract(query: &str) {
    for fragment in VERSION_RECOVERY_REQUIRED_FRAGMENTS {
        assert!(query.contains(*fragment), "version-recovery query is missing {fragment:?}");
    }
    assert!(
        query.contains(&format!(
            "graph_contribution_count_text::numeric <= {}::numeric",
            usize::MAX
        ))
    );
    assert_eq!(
        query.matches("select distinct on (document_id)").count(),
        1,
        "version recovery must collapse to one completion per document",
    );
    for fragment in VERSION_RECOVERY_FORBIDDEN_FRAGMENTS {
        assert!(!query.contains(*fragment), "version-recovery query must not contain {fragment:?}");
    }
    assert!(
        !GRAPH_REEXTRACT_PROJECTION_REPAIR_PREDICATE.contains("$8"),
        "automatic projection repair must not bind or reference the contract boundary",
    );
}

#[test]
fn operator_version_recovery_targets_current_extraction_and_active_projection_boundaries() {
    let query = graph_reextract_candidates_query(GraphReextractSelectionMode::VersionRecovery {
        include_never_extracted: true,
    });
    assert_version_recovery_query_contract(&query);
}

/// Fragments the automatic projection-repair query must carry to require an
/// active projection gap per completion candidate.
const PROJECTION_REPAIR_REQUIRED_FRAGMENTS: &[&str] = &[
    GRAPH_REEXTRACT_PROJECTION_REPAIR_PREDICATE,
    "not state.has_canceled_graph_refresh",
    "latest_exact_graph_stage as",
    "select distinct on (ready.document_id)",
    "job.knowledge_revision_id = ready.active_revision_id",
    "event.stage_name = 'extract_graph'",
    "event.recorded_at desc",
    "'started'::ingest_stage_state",
    "'failed'::ingest_stage_state",
    "current_history.library_id = $1",
    "current_history.extraction_version = $2",
    "current_history.raw_output_json #>> '{lifecycle,revision_id}'\n                           = ready.active_revision_id::text",
    "or state.has_current_extraction_history",
    "historical.document_id = ready.document_id",
    "$3 or state.has_document_extraction_history",
    "or coalesce(state.has_exact_graph_interruption, false)",
    "state.automatic_terminal_failure_count < $6",
    "clock_timestamp()",
    "make_interval(secs => $7::double precision)",
    "state.last_automatic_terminal_failure_completed_at",
    "max(classified.completed_at)",
    "job.priority = $5",
    "':terminal:'",
    "not state.has_current_extraction\n           or state.graph_contribution_count_text::numeric > 0",
];

/// Automatic projection repair must not borrow the version-recovery boundary
/// or the older failure-queue/graph-state shapes.
const PROJECTION_REPAIR_FORBIDDEN_FRAGMENTS: &[&str] = &[
    GRAPH_REEXTRACT_VERSION_RECOVERY_PREDICATE,
    "last_terminal_failure_queue_rank",
    "graph_state",
];

fn assert_projection_repair_query_contract(query: &str) {
    for fragment in PROJECTION_REPAIR_REQUIRED_FRAGMENTS {
        assert!(query.contains(*fragment), "projection-repair query is missing {fragment:?}");
    }
    for fragment in PROJECTION_REPAIR_FORBIDDEN_FRAGMENTS {
        assert!(
            !query.contains(*fragment),
            "projection-repair query must not contain {fragment:?}"
        );
    }
    assert!(
        query.contains(&format!(
            "graph_contribution_count_text::numeric <= {}::numeric",
            usize::MAX
        ))
    );
}

#[test]
fn automatic_projection_repair_requires_an_active_projection_gap_for_every_candidate() {
    let query = graph_reextract_candidates_query(GraphReextractSelectionMode::ProjectionRepair);

    assert_projection_repair_query_contract(&query);
    assert!(GraphReextractSelectionMode::ProjectionRepair.include_never_extracted());
    assert!(
        GraphReextractSelectionMode::VersionRecovery { include_never_extracted: true }
            .include_never_extracted()
    );
    assert!(
        !GraphReextractSelectionMode::VersionRecovery { include_never_extracted: false }
            .include_never_extracted()
    );
}

#[test]
fn recovery_latest_order_prefers_terminal_and_event_causality() {
    let query = graph_reextract_candidates_query(GraphReextractSelectionMode::ProjectionRepair);

    assert!(query.contains(
        "order by document_id,\n                  completed_at desc nulls last,\n                  job_id desc,\n                  attempt_number desc,\n                  ordinal desc,\n                  recorded_at desc,\n                  event_id desc"
    ));
    assert!(query.contains(
        "order by ready.document_id,\n                  coalesce(job.completed_at, attempt.finished_at, event.recorded_at) desc,\n                  job.id desc,\n                  attempt.attempt_number desc,\n                  event.ordinal desc,\n                  event.recorded_at desc,\n                  event.id desc"
    ));
    assert!(!query.contains("attempted_at"));
}

#[test]
fn graph_reextract_progress_uses_the_same_current_version_boundary() {
    let query = graph_reextract_progress_query();
    assert_eq!(query.matches("event.details_json ->> 'extractionVersion' = $2").count(), 1);
    assert!(query.contains("job.queue_state = 'completed'::ingest_queue_state"));
    assert!(query.contains("attempt.attempt_state = 'succeeded'::ingest_attempt_state"));
    assert!(query.contains("jsonb_typeof(graph_contribution_count_json) = 'number'"));
    assert!(
        query.contains(&format!(
            "graph_contribution_count_text::numeric <= {}::numeric",
            usize::MAX
        ))
    );
    assert_eq!(query.matches("select distinct on (document_id)").count(), 1);
    assert!(query.contains("from latest_current_completion"));
    assert!(query.contains("job_kind = 'graph_refresh'"));
    assert!(query.contains("queue_state = 'queued'"));
    assert!(query.contains("queue_state = 'leased'"));
    assert!(query.contains("queue_state = 'paused'"));
}

#[test]
fn graph_rebuild_completion_query_uses_latest_successful_exact_revision_marker() {
    let query = graph_rebuild_document_completions_query();
    assert!(query.contains("h.readable_revision_id = h.active_revision_id"));
    assert!(query.contains("job.knowledge_revision_id = ready.active_revision_id"));
    assert!(query.contains("job.queue_state = 'completed'::ingest_queue_state"));
    assert!(query.contains("attempt.attempt_state = 'succeeded'::ingest_attempt_state"));
    assert!(query.contains("event.stage_state = 'completed'::ingest_stage_state"));
    assert!(query.contains("event.details_json ->> 'extractionVersion' = $2"));
    assert!(query.contains("event.details_json ->> 'graphContributionCount'"));
    assert!(query.contains("attempt.id as attempt_id"));
    assert!(query.contains("completion.attempt_id as activated_by_attempt_id"));
    assert!(
        query.contains(&format!(
            "graph_contribution_count_text::numeric <= {}::numeric",
            usize::MAX
        ))
    );
    assert!(query.contains("completed_at desc nulls last"));
    assert_eq!(query.matches("select distinct on (document_id)").count(), 1);
    assert!(query.contains("left join current_completion completion"));
}
