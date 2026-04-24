-- Soft-delete documents whose content (SHA-256 of their revision body)
-- duplicates an earlier document in the SAME library. The earliest
-- document that carries a `readable_revision_id` wins; all siblings
-- are marked deleted so they stop appearing in the UI and excluding
-- them from future content dedup lookups (which filter on
-- document_state <> 'deleted').
--
-- Scope: content_document in Postgres only. ArangoDB knowledge
-- artifacts remain until a later orphan-cleanup pass — they are not
-- user-visible on the documents table once document_state='deleted'.
-- Graph/query residue (runtime_graph_node, etc.) is also left in
-- place by design; operators can reproject the library's graph if
-- stale dedup ghosts start polluting retrieval.
--
-- Idempotent: rerunning the script does nothing once every group has
-- collapsed to a single survivor. Run `BEGIN; <script>; ROLLBACK;` to
-- preview; run with `BEGIN; <script>; COMMIT;` to apply.
--
-- Reports the number of rows soft-deleted in the final SELECT so the
-- operator can sanity-check against the expected cleanup count.

-- One row per *live* document, anchored to its LATEST revision. An
-- earlier heal matched any revision in history and wrongly collapsed
-- documents that briefly shared a body (e.g. Confluence returning the
-- same "login-required" placeholder for every attachment URL during
-- one crawl) — even though their current content diverged. Latest
-- revision is the only honest "is this document the same thing as
-- that one right now?" signal.
with latest_revision as (
    select distinct on (r.document_id)
        r.document_id,
        r.checksum,
        r.created_at as revision_created_at
    from content_revision r
    order by r.document_id, r.created_at desc
),
duplicate_groups as (
    select
        d.library_id,
        lr.checksum,
        d.id as document_id,
        d.created_at,
        h.readable_revision_id is not null as has_readable_head,
        row_number() over (
            partition by d.library_id, lr.checksum
            order by
                (h.readable_revision_id is not null) desc,
                d.created_at asc,
                d.id asc
        ) as rank_within_group,
        count(*) over (partition by d.library_id, lr.checksum) as group_size
    from content_document d
    join latest_revision lr on lr.document_id = d.id
    left join content_document_head h on h.document_id = d.id
    where d.document_state <> 'deleted'
      and d.deleted_at is null
),
losers as (
    select document_id
    from duplicate_groups
    where group_size > 1
      and rank_within_group > 1
)
update content_document
set document_state = 'deleted',
    deleted_at = coalesce(deleted_at, now())
from losers
where content_document.id = losers.document_id
returning content_document.id, content_document.library_id;
