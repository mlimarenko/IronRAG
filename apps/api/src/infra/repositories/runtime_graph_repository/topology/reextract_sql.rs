/// Durable marker used to distinguish automatically admitted repair jobs from
/// explicit operator recovery while retaining the ordinary ingest-job schema.
///
/// Public so the integration suite plants terminal jobs at the same priority
/// the production path counts instead of duplicating the literal.
pub const AUTOMATIC_GRAPH_REEXTRACT_PRIORITY: i32 = 250;
/// Operator-triggered version recovery keeps the established maintenance
/// priority, but it shares the canonical terminal-failure ceiling and cooldown
/// below with automatic projection repair: an explicit request must not be able
/// to re-admit the same exact revision/extractor tuple forever.
///
/// Public for the same reason as [`AUTOMATIC_GRAPH_REEXTRACT_PRIORITY`].
pub const EXPLICIT_GRAPH_REEXTRACT_PRIORITY: i32 = 200;
/// A terminal ingest job has already consumed the ordinary per-job attempt
/// budget. Both repair paths may create only this many such jobs for one exact
/// document/revision/extractor tuple before the tuple is treated as dead-letter.
pub(super) const AUTOMATIC_GRAPH_REEXTRACT_TERMINAL_JOB_LIMIT: i64 = 3;
/// Cooldown between terminal repair jobs. Per-attempt retries already use
/// their own bounded exponential backoff inside a job.
pub(super) const AUTOMATIC_GRAPH_REEXTRACT_TERMINAL_BACKOFF_SECONDS: i64 = 3_600;

const GRAPH_COMPLETION_EVENTS_CTES: &str = "with ready_documents as (
         select d.workspace_id, h.document_id, h.active_revision_id
         from content_document_head h
         join content_document d on d.id = h.document_id
         join catalog_library library
           on library.id = d.library_id
          and library.lifecycle_state = 'active'::catalog_library_lifecycle_state
         where d.library_id = $1
           and d.document_state = 'active'::content_document_state
           and d.deleted_at is null
           and h.active_revision_id is not null
           and h.readable_revision_id = h.active_revision_id
     ), current_graph_completion_events as (
         select ready.document_id,
                job.completed_at,
                job.id as job_id,
                attempt.id as attempt_id,
                attempt.attempt_number,
                event.recorded_at,
                event.ordinal,
                event.id as event_id,
                event.details_json -> 'graphContributionCount'
                    as graph_contribution_count_json,
                event.details_json ->> 'graphContributionCount'
                    as graph_contribution_count_text,
                event.details_json ->> 'extractionContractVersion'
                    as extraction_contract_version_text
         from ready_documents ready
         join ingest_job job
           on job.workspace_id = ready.workspace_id
          and job.library_id = $1
          and job.knowledge_document_id = ready.document_id
          and job.knowledge_revision_id = ready.active_revision_id
          and job.queue_state = 'completed'::ingest_queue_state
         join ingest_attempt attempt
           on attempt.job_id = job.id
          and attempt.attempt_state = 'succeeded'::ingest_attempt_state
         join ingest_stage_event event
           on event.attempt_id = attempt.id
          and event.stage_name = 'extract_graph'
          and event.stage_state = 'completed'::ingest_stage_state
          and event.details_json ->> 'extractionVersion' = $2
     ), latest_current_completion as (
         select distinct on (document_id)
                document_id,
                attempt_id,
                recorded_at,
                graph_contribution_count_json,
                graph_contribution_count_text,
                extraction_contract_version_text
         from current_graph_completion_events
         order by document_id,
                  completed_at desc nulls last,
                  job_id desc,
                  attempt_number desc,
                  ordinal desc,
                  recorded_at desc,
                  event_id desc
     )";

const CURRENT_GRAPH_COMPLETION_CTE_PREFIX: &str = ", current_completion as (
         select document_id,
                attempt_id,
                recorded_at as completed_at,
                graph_contribution_count_text,
                extraction_contract_version_text
         from latest_current_completion
         where case
             when jsonb_typeof(graph_contribution_count_json) = 'number'
              and graph_contribution_count_text ~ '^(0|[1-9][0-9]*)$'
             then graph_contribution_count_text::numeric <= ";

const CURRENT_GRAPH_COMPLETION_CTE_SUFFIX: &str = "::numeric
             else false
         end
     )";

const GRAPH_REEXTRACT_CANDIDATES_QUERY_PREFIX: &str = ", last_graph_attempt as (
         select distinct on (ready.document_id)
                ready.document_id,
                job.queue_rank as attempted_queue_rank,
                job.id as attempted_job_id
         from ready_documents ready
         join ingest_job job
           on job.workspace_id = ready.workspace_id
          and job.library_id = $1
          and job.knowledge_document_id = ready.document_id
          and job.knowledge_revision_id = ready.active_revision_id
          and job.job_kind = 'graph_refresh'::ingest_job_kind
          and job.queue_state in (
              'completed'::ingest_queue_state,
              'failed'::ingest_queue_state,
              'canceled'::ingest_queue_state
          )
         order by ready.document_id,
                  job.queue_rank desc,
                  job.id desc
     ), automatic_graph_repair_history as (
         select classified.document_id,
                count(classified.job_id) filter (
                    where classified.is_automatic_failure
                )::bigint as terminal_failure_count,
                max(classified.completed_at) filter (
                    where classified.is_automatic_failure
                ) as last_terminal_failure_completed_at,
                coalesce(bool_or(classified.is_canceled), false) as has_canceled_job
         from (
             select ready.document_id,
                    job.id as job_id,
                    job.completed_at,
                    job.queue_state = 'canceled'::ingest_queue_state as is_canceled,
                    job.queue_state = 'failed'::ingest_queue_state
                      and job.priority = $5
                      and (
                          job.dedupe_key = concat(
                              'graph_reextract:', $2, ':', ready.active_revision_id::text
                          )
                          or starts_with(
                              job.dedupe_key,
                              concat(
                                  'graph_reextract:', $2, ':',
                                  ready.active_revision_id::text, ':terminal:'
                              )
                          )
                      ) as is_automatic_failure
             from ready_documents ready
             left join ingest_job job
               on job.workspace_id = ready.workspace_id
              and job.library_id = $1
              and job.knowledge_document_id = ready.document_id
              and job.knowledge_revision_id = ready.active_revision_id
              and job.job_kind = 'graph_refresh'::ingest_job_kind
              and job.queue_state in (
                  'failed'::ingest_queue_state,
                  'canceled'::ingest_queue_state
              )
              and job.completed_at is not null
         ) classified
         group by classified.document_id
     ), latest_exact_graph_stage as (
         select distinct on (ready.document_id)
                ready.document_id,
                event.stage_state
         from ready_documents ready
         join ingest_job job
           on job.workspace_id = ready.workspace_id
          and job.library_id = $1
          and job.knowledge_document_id = ready.document_id
          and job.knowledge_revision_id = ready.active_revision_id
         join ingest_attempt attempt on attempt.job_id = job.id
         join ingest_stage_event event
           on event.attempt_id = attempt.id
          and event.stage_name = 'extract_graph'
         order by ready.document_id,
                  coalesce(job.completed_at, attempt.finished_at, event.recorded_at) desc,
                  job.id desc,
                  attempt.attempt_number desc,
                  event.ordinal desc,
                  event.recorded_at desc,
                  event.id desc
     ), extraction_state as (
         select ready.workspace_id, ready.document_id, ready.active_revision_id,
                exists (
                    select 1 from runtime_graph_extraction historical
                     where historical.library_id = $1
                       and historical.document_id = ready.document_id
                ) as has_document_extraction_history,
                exists (
                    select 1 from runtime_graph_extraction current_history
                     where current_history.library_id = $1
                       and current_history.document_id = ready.document_id
                       and current_history.extraction_version = $2
                       and current_history.raw_output_json #>> '{lifecycle,revision_id}'
                           = ready.active_revision_id::text
                ) as has_current_extraction_history,
                completion.completed_at is not null as has_current_extraction,
                completion.graph_contribution_count_text,
                completion.extraction_contract_version_text,
                latest_stage.stage_state in (
                    'started'::ingest_stage_state,
                    'failed'::ingest_stage_state
                ) as has_exact_graph_interruption,
                automatic_history.terminal_failure_count
                    as automatic_terminal_failure_count,
                automatic_history.last_terminal_failure_completed_at
                    as last_automatic_terminal_failure_completed_at,
                automatic_history.has_canceled_job as has_canceled_graph_refresh,
                exists (
                    select 1
                    from runtime_graph_node node
                    join runtime_graph_evidence evidence
                      on evidence.library_id = node.library_id
                     and evidence.target_kind = 'node'
                     and evidence.target_id = node.id
                     and evidence.document_id = ready.document_id
                     and evidence.revision_id = ready.active_revision_id
                    where node.library_id = $1
                      and node.document_id = ready.document_id
                      and node.node_type = 'document'
                      and node.projection_version = coalesce(
                          (
                              select snapshot.projection_version
                              from runtime_graph_snapshot snapshot
                              where snapshot.library_id = $1
                          ),
                          1
                      )
                ) as has_active_revision_publication
         from ready_documents ready
         left join current_completion completion on completion.document_id = ready.document_id
         left join latest_exact_graph_stage latest_stage
           on latest_stage.document_id = ready.document_id
         join automatic_graph_repair_history automatic_history
           on automatic_history.document_id = ready.document_id
     )
     select state.workspace_id, state.document_id, state.active_revision_id
     from extraction_state state
     left join last_graph_attempt attempt on attempt.document_id = state.document_id
     where ";

const GRAPH_REEXTRACT_PROJECTION_REPAIR_PREDICATE: &str = "
       not state.has_active_revision_publication
       and not state.has_canceled_graph_refresh
       and state.automatic_terminal_failure_count < $6
       and (
           state.last_automatic_terminal_failure_completed_at is null
           or state.last_automatic_terminal_failure_completed_at
                <= clock_timestamp() - make_interval(secs => $7::double precision)
       )
       and (
           not state.has_current_extraction
           or state.graph_contribution_count_text::numeric > 0
       )
       and (
           state.graph_contribution_count_text::numeric > 0
           or state.has_current_extraction_history
           or coalesce(state.has_exact_graph_interruption, false)
       )";

/// Explicit operator version recovery admits a document when its latest
/// current-version completion was produced under a different extraction
/// contract (`$8`) than the running code — including the pre-contract rows that
/// carry no `extractionContractVersion` at all — or when it has a positive
/// contribution that the active projection never published. A completion under
/// the current contract, including a valid zero-contribution one, stays
/// terminal. Only this boundary crosses a contract gap; automatic projection
/// repair keeps its conservative current-completion view.
const GRAPH_REEXTRACT_VERSION_RECOVERY_PREDICATE: &str = "
       (
           state.extraction_contract_version_text is distinct from $8
           or (
               state.graph_contribution_count_text::numeric > 0
               and not state.has_active_revision_publication
           )
       )
       and state.automatic_terminal_failure_count < $6
       and (
           state.last_automatic_terminal_failure_completed_at is null
           or state.last_automatic_terminal_failure_completed_at
                <= clock_timestamp() - make_interval(secs => $7::double precision)
       )";

const GRAPH_REEXTRACT_CANDIDATES_QUERY_SUFFIX: &str = "
       and ($3 or state.has_document_extraction_history)
       and not exists (
            select 1 from ingest_job j
             where j.knowledge_document_id = state.document_id
               and j.knowledge_revision_id = state.active_revision_id
               and j.queue_state in ('queued', 'leased', 'paused')
       )
     order by attempt.attempted_queue_rank asc nulls first,
              attempt.attempted_job_id asc nulls first,
              state.document_id
     limit $4";

const GRAPH_REEXTRACT_PROGRESS_QUERY_TAIL: &str = ", document_progress as (
         select ready.document_id, ready.active_revision_id,
                exists (
                    select 1 from runtime_graph_extraction historical
                     where historical.document_id = ready.document_id
                ) as has_extraction_history,
                completion.completed_at is not null as has_current_extraction
         from ready_documents ready
         left join current_completion completion on completion.document_id = ready.document_id
     )
     select count(*)::bigint as ready_document_count,
            count(*) filter (where has_extraction_history)::bigint
                as known_eligible_document_count,
            count(*) filter (where has_current_extraction)::bigint
                as current_extraction_document_count,
            count(*) filter (where not has_current_extraction)::bigint
                as missing_current_extraction_document_count,
            count(*) filter (
                where has_extraction_history and not has_current_extraction
            )::bigint as outdated_or_incomplete_document_count,
            count(*) filter (where not has_extraction_history)::bigint
                as never_extracted_document_count,
            (
                select count(*)::bigint
                from ingest_job job
                join ready_documents ready
                  on ready.document_id = job.knowledge_document_id
                 and ready.active_revision_id = job.knowledge_revision_id
                where job.job_kind = 'graph_refresh'::ingest_job_kind
                  and job.queue_state = 'queued'::ingest_queue_state
            ) as queued_job_count,
            (
                select count(*)::bigint
                from ingest_job job
                join ready_documents ready
                  on ready.document_id = job.knowledge_document_id
                 and ready.active_revision_id = job.knowledge_revision_id
                where job.job_kind = 'graph_refresh'::ingest_job_kind
                  and job.queue_state = 'leased'::ingest_queue_state
            ) as leased_job_count,
            (
                select count(*)::bigint
                from ingest_job job
                join ready_documents ready
                  on ready.document_id = job.knowledge_document_id
                 and ready.active_revision_id = job.knowledge_revision_id
                where job.job_kind = 'graph_refresh'::ingest_job_kind
                  and job.queue_state = 'paused'::ingest_queue_state
            ) as paused_job_count
     from document_progress";

const GRAPH_REBUILD_DOCUMENT_COMPLETIONS_QUERY_TAIL: &str = "
     select ready.workspace_id,
            ready.document_id,
            ready.active_revision_id as revision_id,
            completion.attempt_id as activated_by_attempt_id,
            completion.graph_contribution_count_text
     from ready_documents ready
     left join current_completion completion
       on completion.document_id = ready.document_id
     order by ready.document_id";

fn graph_completion_ctes() -> String {
    [
        GRAPH_COMPLETION_EVENTS_CTES,
        CURRENT_GRAPH_COMPLETION_CTE_PREFIX,
        &usize::MAX.to_string(),
        CURRENT_GRAPH_COMPLETION_CTE_SUFFIX,
    ]
    .concat()
}

pub(super) fn graph_reextract_candidates_query(
    selection_mode: GraphReextractSelectionMode,
) -> String {
    let mut query = graph_completion_ctes();
    query.push_str(GRAPH_REEXTRACT_CANDIDATES_QUERY_PREFIX);
    query.push_str(match selection_mode {
        GraphReextractSelectionMode::ProjectionRepair => {
            GRAPH_REEXTRACT_PROJECTION_REPAIR_PREDICATE
        }
        GraphReextractSelectionMode::VersionRecovery { .. } => {
            GRAPH_REEXTRACT_VERSION_RECOVERY_PREDICATE
        }
    });
    query.push_str(GRAPH_REEXTRACT_CANDIDATES_QUERY_SUFFIX);
    query
}

pub(super) fn graph_reextract_progress_query() -> String {
    let mut query = graph_completion_ctes();
    query.push_str(GRAPH_REEXTRACT_PROGRESS_QUERY_TAIL);
    query
}

pub(super) fn graph_rebuild_document_completions_query() -> String {
    let mut query = graph_completion_ctes();
    query.push_str(GRAPH_REBUILD_DOCUMENT_COMPLETIONS_QUERY_TAIL);
    query
}

/// Selects which recovery boundary a graph re-extract query is allowed to
/// cross.
///
/// Automatic projection repair is conservative: the active projection must
/// lack exact-revision publication evidence for its document node, and a
/// missing/malformed current completion needs current-version/current-revision
/// extraction or exact-head interruption evidence.
/// Explicit version recovery may also replace a missing or malformed
/// current-version completion behind an existing node and crosses an
/// extraction-contract gap (a completion stamped by another contract, or by
/// none); the operator controls whether documents with no extraction history
/// are included.
///
/// Both boundaries observe the same canonical durable terminal-failure ceiling
/// and cooldown for their own repair priority, so neither can re-admit a
/// dead-lettered document/revision/extractor tuple without bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphReextractSelectionMode {
    ProjectionRepair,
    VersionRecovery { include_never_extracted: bool },
}

impl GraphReextractSelectionMode {
    pub(super) const fn include_never_extracted(self) -> bool {
        match self {
            Self::ProjectionRepair => true,
            Self::VersionRecovery { include_never_extracted } => include_never_extracted,
        }
    }
}

#[cfg(test)]
#[path = "reextract_sql_tests.rs"]
mod tests;
