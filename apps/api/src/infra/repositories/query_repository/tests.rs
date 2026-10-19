use super::*;

#[test]
fn retention_lock_is_transaction_scoped_to_library_and_surface() {
    assert!(CONVERSATION_RETENTION_LOCK_SQL.contains("pg_advisory_xact_lock"));
    assert!(CONVERSATION_RETENTION_LOCK_SQL.contains("$1::text"));
    assert!(CONVERSATION_RETENTION_LOCK_SQL.contains("$2::text"));
}

#[test]
fn fresh_completed_mcp_rows_are_evictable_but_active_and_replay_rows_are_not() {
    let active_guard = MCP_CONVERSATION_OVERFLOW_DELETE_SQL
        .find("runtime.lifecycle_state in ('accepted', 'running')")
        .expect("MCP retention must protect active runtime executions");
    let empty_row_grace = MCP_CONVERSATION_OVERFLOW_DELETE_SQL
        .find("conversation.created_at < now() - interval '10 minutes'")
        .expect("empty MCP rows need an in-flight grace period");

    assert!(active_guard < empty_row_grace);
    assert!(!MCP_CONVERSATION_OVERFLOW_DELETE_SQL.contains("execution.completed_at"));
    assert!(MCP_CONVERSATION_OVERFLOW_DELETE_SQL.contains("replay.conversation_id <>"));
    assert!(MCP_CONVERSATION_OVERFLOW_DELETE_SQL.contains("conversation.id <> $4"));
}

#[test]
fn ui_overflow_keeps_its_existing_age_guard() {
    assert!(
        AGE_GUARDED_CONVERSATION_OVERFLOW_DELETE_SQL
            .contains("conversation.created_at < now() - interval '10 minutes'")
    );
    assert!(
        AGE_GUARDED_CONVERSATION_OVERFLOW_DELETE_SQL
            .contains("runtime.lifecycle_state in ('accepted', 'running')")
    );
    assert!(!AGE_GUARDED_CONVERSATION_OVERFLOW_DELETE_SQL.contains("execution.completed_at"));
    assert!(AGE_GUARDED_CONVERSATION_OVERFLOW_DELETE_SQL.contains("replay.conversation_id <>"));
}

#[test]
fn baseline_has_one_required_runtime_and_no_query_completion_marker() {
    let baseline = include_str!("../../../../migrations/0001_init.sql");
    let query_execution = baseline
        .split_once("CREATE TABLE public.query_execution (")
        .and_then(|(_, rest)| rest.split_once(");"))
        .map(|(table, _)| table)
        .expect("baseline must define query_execution");

    assert!(query_execution.contains("runtime_execution_id uuid NOT NULL"));
    assert!(!query_execution.contains("completed_at"));
    assert!(baseline.contains(
        "CREATE UNIQUE INDEX idx_query_execution_runtime_execution_id ON public.query_execution USING btree (runtime_execution_id);"
    ));
    assert!(baseline.contains(
        "ADD CONSTRAINT query_execution_runtime_execution_id_fkey FOREIGN KEY (runtime_execution_id) REFERENCES public.runtime_execution(id) ON DELETE RESTRICT;"
    ));
    assert!(baseline.contains("CONSTRAINT chk_runtime_execution_lifecycle_completion CHECK"));
}
