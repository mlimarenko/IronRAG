-- Reconcile non-terminal owner rows left behind by terminated ingest attempts.
--
-- One transaction closes the historical backlog and re-runs as a safe backstop.
-- Three owner families are covered:
--
--   * `runtime_graph_extraction` stuck at `status = 'processing'` whose
--     activating attempt (`raw_output_json->'lifecycle'->>'activated_by_attempt_id'`)
--     has already terminated or never existed;
--   * `runtime_execution` stuck at `lifecycle_state IN ('accepted','running')`
--     for `owner_kind = 'graph_extraction_attempt'` whose extraction or
--     activating attempt has already terminated;
--   * `content_mutation` stuck at `mutation_state IN ('accepted','running')`
--     with no live `ingest_job`, classified by observable outcome, together
--     with its pending `content_mutation_item` rows and its
--     `ops_async_operation` row (`subject_kind = 'content_mutation'`).
--
-- The content-mutation failure codes are exactly `document_deleted`,
-- `superseded_by_newer_revision` and `reconciled_orphaned_attempt`. A fourth
-- pass repairs `ops_async_operation` rows whose subject mutation is already
-- terminal or absent. A fifth pass applies the same reconciliation to
-- `ops_async_operation` rows with `subject_kind = 'query_execution'` whose
-- linked runtime execution is terminal or whose `query_execution` /
-- `runtime_execution` row is absent; a completed/recovered runtime maps to
-- `ready` with a null failure code and every other terminal state records
-- `reconciled_orphaned_attempt`. These rows are never touched by the mutation
-- passes.
--
-- Safe operator usage (psql only):
--
--   # Preview. This is the default when `apply` is omitted, false or 0.
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
--     -f scripts/ops/reconcile-nonterminal-owners.sql
--
--   # Apply after reviewing the preview counts.
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -v apply=1 \
--     -f scripts/ops/reconcile-nonterminal-owners.sql
--
-- Every pass selects rows by the expected current state and writes a terminal
-- state, so a second apply matches nothing and reports zero changes. The script
-- never deletes rows and never rewrites an already-terminal row.
--
-- A malformed `activated_by_attempt_id` is classified as "attempt absent": the
-- value is checked against a UUID text pattern before any `::uuid` cast, so a
-- non-UUID string cannot abort the run with a cast error. Rows whose activating
-- attempt is still `leased` / `running` (or whose extraction is still
-- `processing`) are never touched.
--
-- The per-pass count summary is printed before the commit/rollback decision.

\set ON_ERROR_STOP on

\if :{?apply}
\else
    \set apply false
\endif

begin;

set local lock_timeout = '30s';

\echo 'reconcile-nonterminal-owners: pass 1 - runtime_graph_extraction'

-- Pass 1: terminalize `processing` extractions whose activating attempt is
-- absent or terminal. `leased` / `running` attempts are excluded by the
-- selection, so a live attempt is never swept. A malformed attempt id joins to
-- no attempt and is therefore treated as absent.
with targets as materialized (
    select
        extraction.id as extraction_id,
        attempt.attempt_state as attempt_state
    from public.runtime_graph_extraction as extraction
    left join public.ingest_attempt as attempt
      on attempt.id = case
          when extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id'
               ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
          then (extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id')::uuid
          else null
      end
    where extraction.status = 'processing'
      and (
          attempt.id is null
          or attempt.attempt_state in ('succeeded', 'failed', 'canceled', 'abandoned')
      )
),
updated as (
    update public.runtime_graph_extraction as extraction
    set status = case
            when targets.attempt_state = 'canceled' then 'canceled'
            else 'failed'
        end,
        error_message = coalesce(extraction.error_message, 'reconciled_orphaned_attempt')
    from targets
    where extraction.id = targets.extraction_id
      and extraction.status = 'processing'
    returning extraction.id
)
select 'pass1.extraction' as pass, count(*) as changed
from updated;

\echo 'reconcile-nonterminal-owners: pass 2 - runtime_execution'

-- Pass 2: terminalize non-terminal graph-extraction executions whose extraction
-- or activating attempt is absent or terminal. An execution whose extraction is
-- still `processing` under a `leased` / `running` attempt is excluded; pass 1
-- has already terminalized any extraction whose attempt was terminal.
with targets as materialized (
    select
        execution.id as execution_id,
        attempt.attempt_state as attempt_state
    from public.runtime_execution as execution
    left join public.runtime_graph_extraction as extraction
      on extraction.id = execution.owner_id
    left join public.ingest_attempt as attempt
      on attempt.id = case
          when extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id'
               ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
          then (extraction.raw_output_json->'lifecycle'->>'activated_by_attempt_id')::uuid
          else null
      end
    where execution.lifecycle_state in ('accepted', 'running')
      and execution.owner_kind = 'graph_extraction_attempt'
      -- Both traces must be terminal or absent. An extraction can reach
      -- `ready` while its execution is still running, so the activating
      -- attempt is the authority and the extraction-terminal condition alone
      -- is not sufficient.
      and (
          attempt.id is null
          or attempt.attempt_state in ('succeeded', 'failed', 'canceled', 'abandoned')
      )
      and (
          extraction.id is null
          or extraction.status in ('ready', 'failed', 'canceled')
      )
),
updated as (
    update public.runtime_execution as execution
    set lifecycle_state = case
            when targets.attempt_state = 'canceled' then 'canceled'
            else 'failed'
        end::runtime_lifecycle_state,
        failure_code = 'reconciled_orphaned_attempt',
        active_stage = null,
        completed_at = now()
    from targets
    where execution.id = targets.execution_id
      and execution.lifecycle_state in ('accepted', 'running')
    returning execution.id
)
select 'pass2.execution' as pass, count(*) as changed
from updated;

\echo 'reconcile-nonterminal-owners: pass 3 - content_mutation classification'

-- Pass 3: classify every non-terminal mutation that has no live job and write
-- the matching terminal state to the mutation, its pending items and its async
-- operation. Classification priority: document_deleted, then
-- superseded_by_newer_revision, then reconciled_orphaned_attempt. All three
-- sub-statements read the same materialized target snapshot, so the mutation
-- state guard `mutation_state IN ('accepted','running')` is evaluated against
-- the pre-update state everywhere.
with targets as materialized (
    select
        mutation.id as mutation_id,
        case
            when exists (
                select 1
                from public.content_mutation_item as item
                join public.content_document as document
                  on document.id = item.document_id
                where item.mutation_id = mutation.id
                  and (document.document_state = 'deleted'
                       or document.deleted_at is not null)
            ) then 'document_deleted'
            when exists (
                select 1
                from public.content_mutation_item as item
                join public.content_document as document
                  on document.id = item.document_id
                where item.mutation_id = mutation.id
                  and document.document_state <> 'deleted'
                  and document.deleted_at is null
                  and exists (
                      select 1
                      from public.content_revision as revision
                      where revision.document_id = item.document_id
                        and revision.created_at > mutation.requested_at
                  )
            ) then 'superseded_by_newer_revision'
            else 'reconciled_orphaned_attempt'
        end as failure_class
    from public.content_mutation as mutation
    where mutation.mutation_state in ('accepted', 'running')
      and not exists (
          select 1
          from public.ingest_job as job
          where job.mutation_id = mutation.id
            and job.queue_state in ('queued', 'leased', 'paused')
            and job.completed_at is null
      )
      -- A mutation that spans more than one document (a web-capture run owns
      -- one item per page) cannot be classified or terminated from a single
      -- document's outcome: retiring it would mislabel live sibling pages.
      -- Leave those to an explicit operator decision rather than guess here.
      and (
          select count(distinct item.document_id)
          from public.content_mutation_item as item
          where item.mutation_id = mutation.id
      ) <= 1
),
updated_mutations as (
    update public.content_mutation as mutation
    set mutation_state = case
            when targets.failure_class = 'document_deleted' then 'canceled'
            else 'failed'
        end::content_mutation_state,
        completed_at = now(),
        failure_code = targets.failure_class
    from targets
    where mutation.id = targets.mutation_id
      and mutation.mutation_state in ('accepted', 'running')
    returning targets.failure_class
),
updated_items as (
    update public.content_mutation_item as item
    set         item_state = case
            when targets.failure_class = 'document_deleted' then 'skipped'
            else 'failed'
        end::content_mutation_item_state,
        message = coalesce(
            item.message,
            case
                when targets.failure_class = 'document_deleted'
                    then 'mutation skipped because document was deleted'
                else 'mutation failed: ' || targets.failure_class
            end
        )
    from targets, public.content_mutation as mutation
    where item.mutation_id = targets.mutation_id
      and mutation.id = item.mutation_id
      and mutation.mutation_state in ('accepted', 'running')
      and item.item_state = 'pending'
    returning targets.failure_class
),
updated_ops as (
    update public.ops_async_operation as operation
    set status = case
            when targets.failure_class = 'superseded_by_newer_revision' then 'superseded'
            else 'failed'
        end::ops_async_operation_status,
        completed_at = now(),
        failure_code = targets.failure_class
    from targets, public.content_mutation as mutation
    where operation.subject_kind = 'content_mutation'
      and operation.subject_id = targets.mutation_id
      and mutation.id = targets.mutation_id
      and mutation.mutation_state in ('accepted', 'running')
      and operation.status in ('accepted', 'processing')
    returning targets.failure_class
)
select 'pass3.mutation' as pass, failure_class, count(*) as changed
from updated_mutations
group by failure_class
union all
select 'pass3.item', failure_class, count(*)
from updated_items
group by failure_class
union all
select 'pass3.ops', failure_class, count(*)
from updated_ops
group by failure_class
order by pass, failure_class;

\echo 'reconcile-nonterminal-owners: pass 4 - ops_async_operation backstop'

-- Pass 4: repair content-mutation async operations that are still non-terminal
-- while their subject mutation is already terminal or absent. A canceled
-- mutation yields a canceled operation; anything else yields failed with the
-- mutation's own code or the reconciliation backstop code. `query_execution`
-- subjects are not selected.
with subjects as materialized (
    select
        operation.id as operation_id,
        mutation.mutation_state as mutation_state,
        mutation.failure_code as mutation_failure_code
    from public.ops_async_operation as operation
    left join public.content_mutation as mutation
      on mutation.id = operation.subject_id
    where operation.subject_kind = 'content_mutation'
      and operation.status in ('accepted', 'processing')
      and (
          mutation.id is null
          or mutation.mutation_state in ('applied', 'failed', 'conflicted', 'canceled')
      )
),
updated as (
    update public.ops_async_operation as operation
    set status = case
            when subjects.mutation_state = 'canceled' then 'canceled'
            else 'failed'
        end::ops_async_operation_status,
        completed_at = now(),
        failure_code = coalesce(subjects.mutation_failure_code, 'reconciled_orphaned_attempt')
    from subjects
    where operation.id = subjects.operation_id
      and operation.subject_kind = 'content_mutation'
      and operation.status in ('accepted', 'processing')
    returning operation.id
)
select 'pass4.ops' as pass, count(*) as changed
from updated;

\echo 'reconcile-nonterminal-owners: pass 5 - query_execution ops backstop'

-- Pass 5: repair query-execution async operations whose linked runtime
-- execution already terminated, or whose `query_execution` / `runtime_execution`
-- row is absent. The selection requires `operation_kind = 'query_execution'`,
-- matching `reap_stale_query_executions`. `reap_stale_query_executions` only
-- selects operations whose runtime is still `accepted` / `running`, so these
-- are otherwise stranded in `processing`. A completed/recovered runtime yields
-- `ready` with no failure code; canceled yields `canceled`; anything else
-- yields `failed` with `reconciled_orphaned_attempt`.
with targets as materialized (
    select
        operation.id as operation_id,
        runtime.lifecycle_state as runtime_lifecycle_state
    from public.ops_async_operation as operation
    left join public.query_execution as query
      on query.id = operation.subject_id
    left join public.runtime_execution as runtime
      on runtime.id = query.runtime_execution_id
     and runtime.owner_kind = 'query_execution'
     and runtime.owner_id = query.id
    where operation.subject_kind = 'query_execution'
      and operation.operation_kind = 'query_execution'
      and operation.status in ('accepted', 'processing')
      and (
          query.id is null
          or runtime.id is null
          or runtime.lifecycle_state in ('completed', 'recovered', 'failed', 'canceled')
      )
),
updated as (
    update public.ops_async_operation as operation
    set status = case
            when targets.runtime_lifecycle_state in ('completed', 'recovered') then 'ready'
            when targets.runtime_lifecycle_state = 'canceled' then 'canceled'
            else 'failed'
        end::ops_async_operation_status,
        completed_at = now(),
        failure_code = case
            when targets.runtime_lifecycle_state in ('completed', 'recovered') then null
            else 'reconciled_orphaned_attempt'
        end
    from targets
    where operation.id = targets.operation_id
      and operation.status in ('accepted', 'processing')
    returning operation.id
)
select 'pass5.ops' as pass, count(*) as changed
from updated;

\if :apply
    commit;
    \echo 'reconcile-nonterminal-owners: changes committed'
\else
    rollback;
    \echo 'reconcile-nonterminal-owners: dry-run only; changes rolled back'
\endif
