-- Heal script: documents that have `readable_revision_id` set on their
-- `content_document_head` row BUT whose latest `content_mutation` is
-- stuck in a non-terminal state (`accepted` / `running`). This was
-- produced by the pre-transactional worker finalize path
-- (`services/ingest/worker.rs:1313-1344`) where
-- `update_mutation_item` / `update_mutation_status` failures were
-- logged as `warn!` and silently skipped, while `promote_document_head`
-- ran unconditionally afterwards.
--
-- The fix in the same release (wrapping finalize in one
-- `sqlx::Transaction`) stops fresh drift from forming; this script
-- cleans up the rows that already landed in the contract-violation
-- state before the worker was tightened.
--
-- Safety:
--   * We ONLY touch rows where `readable_revision_id IS NOT NULL` —
--     that is the proof the head-promote step actually succeeded, so
--     it is safe to declare the mutation `applied` after the fact.
--   * Terminal states (`failed`, `conflicted`, `canceled`) are
--     explicitly NOT touched.
--   * Fully idempotent: re-running the script after it already
--     succeeded is a no-op (every row is either already `applied` or
--     never matches the filter again).
--
-- Run with:
--   docker exec -i ironrag-postgres-1 psql -U postgres -d ironrag \
--     < scripts/ops/heal-stuck-mutation-state.sql
--
-- The script wraps itself in `BEGIN`/`COMMIT` and prints per-table
-- update counts at the end so the operator can confirm the heal.

\echo == heal-stuck-mutation-state: before counts ==
SELECT
  (SELECT count(*) FROM content_document_head h
     JOIN content_mutation m ON m.id = h.latest_mutation_id
     WHERE h.readable_revision_id IS NOT NULL
       AND m.mutation_state::text IN ('accepted','running')
  ) AS stuck_mutations,
  (SELECT count(*) FROM content_mutation_item mi
     JOIN content_document_head h ON h.document_id = mi.document_id
     WHERE h.readable_revision_id IS NOT NULL
       AND mi.item_state::text IN ('pending','applying')
  ) AS stuck_mutation_items;

BEGIN;

-- 1) Flip `content_mutation.mutation_state` → 'applied' for any
--    mutation whose document already has a `readable_revision_id`
--    on head. We also set `completed_at` when missing (it should be
--    populated by the finalize path; a `coalesce` keeps older
--    timestamps intact).
WITH stuck AS (
  SELECT h.latest_mutation_id AS mutation_id
  FROM content_document_head h
  JOIN content_mutation m ON m.id = h.latest_mutation_id
  WHERE h.readable_revision_id IS NOT NULL
    AND m.mutation_state::text IN ('accepted','running')
)
UPDATE content_mutation m
SET
  mutation_state = 'applied',
  completed_at   = COALESCE(m.completed_at, now())
FROM stuck s
WHERE m.id = s.mutation_id;

-- 2) Flip `content_mutation_item.item_state` → 'applied' for items
--    under those mutations and pin `result_revision_id` to the
--    document's head readable revision when the item is missing one.
WITH stuck_items AS (
  SELECT mi.id AS item_id, h.readable_revision_id AS head_rev
  FROM content_mutation_item mi
  JOIN content_document_head h ON h.document_id = mi.document_id
  WHERE h.readable_revision_id IS NOT NULL
    AND mi.item_state::text IN ('pending','applying')
)
UPDATE content_mutation_item mi
SET
  item_state         = 'applied',
  result_revision_id = COALESCE(mi.result_revision_id, s.head_rev)
FROM stuck_items s
WHERE mi.id = s.item_id;

COMMIT;

\echo == heal-stuck-mutation-state: after counts ==
SELECT
  (SELECT count(*) FROM content_document_head h
     JOIN content_mutation m ON m.id = h.latest_mutation_id
     WHERE h.readable_revision_id IS NOT NULL
       AND m.mutation_state::text IN ('accepted','running')
  ) AS stuck_mutations_remaining,
  (SELECT count(*) FROM content_mutation_item mi
     JOIN content_document_head h ON h.document_id = mi.document_id
     WHERE h.readable_revision_id IS NOT NULL
       AND mi.item_state::text IN ('pending','applying')
  ) AS stuck_mutation_items_remaining;
