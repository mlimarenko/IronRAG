-- Soft-delete documents whose latest content duplicates an earlier document
-- in the SAME library. A document with a readable head wins over one without;
-- remaining ties are resolved by oldest document first.
--
-- The canonical `content_document` tombstone and the retrieval-facing
-- `knowledge_document` tombstone are written in one transaction. Duplicate
-- groups also retain their already-deleted members so a run can repair active
-- knowledge residue left by an older version of this script. Every library
-- with a real visibility change in either projection advances
-- `source_truth_version` exactly once, invalidating cached answers and
-- projection fingerprints for the old generation.
--
-- Safe operator usage (psql only):
--
--   # Preview. This is also the default when `apply` is omitted.
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
--     -f scripts/ops/heal-content-duplicates.sql
--
--   # Apply after reviewing the preview counts.
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -v apply=true \
--     -f scripts/ops/heal-content-duplicates.sql
--
-- The script takes write-conflicting locks on the five participating tables.
-- Reads may continue, but content/catalog writers are briefly drained so the
-- duplicate snapshot cannot race a newly-created revision. Lock acquisition
-- fails after 30 seconds instead of waiting indefinitely. Run during a
-- maintenance window for large catalogs.
--
-- Idempotency:
--   * only non-surviving live documents receive a new canonical tombstone;
--   * already-deleted duplicate members may only repair their own knowledge
--     tombstone;
--   * generations are derived only from rows that were visible before their
--     update.
-- A second successful apply therefore reports zero changes and performs no
-- generation bump.
--
-- Graph rows are not deleted here. The generation bump fences answer/cache
-- state derived before the tombstone; normal graph maintenance can rebuild the
-- affected library afterward.

\set ON_ERROR_STOP on
\if :{?apply}
\else
    \set apply false
\endif

begin;

set local lock_timeout = '30s';

-- Lock the catalog parent before child tables, matching the application's
-- lifecycle lock order. SHARE ROW EXCLUSIVE blocks concurrent writers while
-- allowing ordinary reads and is sufficient for a stable duplicate snapshot.
lock table
    public.catalog_library,
    public.content_document,
    public.content_document_head,
    public.content_revision,
    public.knowledge_document
in share row exclusive mode;

-- One row per document, anchored to its latest revision. Matching any
-- historical revision would incorrectly collapse documents that once shared a
-- transient placeholder body but whose current content later diverged. Deleted
-- members remain in the grouping only so stale knowledge projections from an
-- earlier heal can be retired; a live member always ranks ahead of them.
with latest_revision as materialized (
    select distinct on (revision.document_id)
        revision.document_id,
        revision.checksum
    from public.content_revision as revision
    order by
        revision.document_id,
        revision.created_at desc,
        revision.id desc
),
duplicate_groups as materialized (
    select
        document.library_id,
        latest.checksum,
        document.id as document_id,
        (
            document.document_state <> 'deleted'
            and document.deleted_at is null
        ) as canonical_is_live,
        (
            document.document_state = 'active'
            and document.deleted_at is null
            and head.readable_revision_id is not null
        ) as canonical_was_visible,
        row_number() over (
            partition by document.library_id, latest.checksum
            order by
                (
                    document.document_state <> 'deleted'
                    and document.deleted_at is null
                ) desc,
                (head.readable_revision_id is not null) desc,
                document.created_at asc,
                document.id asc
        ) as rank_within_group,
        count(*) over (
            partition by document.library_id, latest.checksum
        ) as group_size
    from public.content_document as document
    join latest_revision as latest
      on latest.document_id = document.id
    left join public.content_document_head as head
      on head.document_id = document.id
),
selected_duplicates as materialized (
    select
        duplicate.library_id,
        duplicate.document_id,
        duplicate.canonical_is_live,
        duplicate.canonical_was_visible
    from duplicate_groups as duplicate
    where duplicate.group_size > 1
      and (
          not duplicate.canonical_is_live
          or duplicate.rank_within_group > 1
      )
),
affected_libraries as materialized (
    select distinct duplicate.library_id
    from selected_duplicates as duplicate
),
locked_libraries as materialized (
    select library.id
    from public.catalog_library as library
    join affected_libraries as affected
      on affected.library_id = library.id
    order by library.id
    for no key update of library
),
selected_tombstones as materialized (
    select
        document.id as document_id,
        document.library_id,
        coalesce(document.deleted_at, statement_timestamp()) as deleted_at,
        duplicate.canonical_is_live,
        duplicate.canonical_was_visible
    from selected_duplicates as duplicate
    join locked_libraries as locked
      on locked.id = duplicate.library_id
    join public.content_document as document
      on document.id = duplicate.document_id
     and document.library_id = duplicate.library_id
),
tombstoned_content as (
    update public.content_document as document
    set
        document_state = 'deleted',
        deleted_at = target.deleted_at
    from selected_tombstones as target
    where document.id = target.document_id
      and document.library_id = target.library_id
      and (
          document.document_state is distinct from 'deleted'
          or document.deleted_at is distinct from target.deleted_at
      )
    returning
        document.id as document_id,
        document.library_id,
        document.deleted_at
),
canonical_answer_changes as materialized (
    select distinct tombstone.library_id
    from tombstoned_content as tombstone
    join selected_tombstones as target
      on target.document_id = tombstone.document_id
     and target.library_id = tombstone.library_id
    where target.canonical_was_visible
),
knowledge_targets as materialized (
    select
        knowledge.document_id,
        knowledge.library_id,
        target.deleted_at,
        (
            knowledge.document_state = 'active'
            and knowledge.deleted_at is null
            and knowledge.readable_revision_id is not null
        ) as knowledge_was_visible
    from public.knowledge_document as knowledge
    join selected_tombstones as target
      on target.document_id = knowledge.document_id
     and target.library_id = knowledge.library_id
    where knowledge.document_state is distinct from 'deleted'
       or knowledge.active_revision_id is not null
       or knowledge.deleted_at is distinct from target.deleted_at
),
tombstoned_knowledge as (
    update public.knowledge_document as knowledge
    set
        document_state = 'deleted',
        active_revision_id = null,
        deleted_at = target.deleted_at,
        updated_at = statement_timestamp()
    from knowledge_targets as target
    where knowledge.document_id = target.document_id
      and knowledge.library_id = target.library_id
    returning
        knowledge.document_id,
        knowledge.library_id
),
knowledge_answer_changes as materialized (
    select distinct tombstone.library_id
    from tombstoned_knowledge as tombstone
    join knowledge_targets as target
      on target.document_id = tombstone.document_id
     and target.library_id = tombstone.library_id
    where target.knowledge_was_visible
),
changed_libraries as materialized (
    select library_id
    from canonical_answer_changes
    union
    select library_id
    from knowledge_answer_changes
),
bumped_libraries as (
    update public.catalog_library as library
    set source_truth_version = greatest(
        coalesce(library.source_truth_version, 0) + 1,
        (extract(epoch from clock_timestamp()) * 1000000)::bigint
    )
    from changed_libraries as changed
    where library.id = changed.library_id
    returning library.id
)
select
    (select count(*) from tombstoned_content)
        as content_documents_tombstoned,
    (select count(*) from tombstoned_knowledge)
        as knowledge_documents_tombstoned,
    (select count(*) from bumped_libraries)
        as libraries_generation_bumped;

\if :apply
    commit;
    \echo 'heal-content-duplicates: changes committed'
\else
    rollback;
    \echo 'heal-content-duplicates: dry-run only; changes rolled back'
\endif
