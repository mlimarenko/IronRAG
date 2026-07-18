-- Preserve the admitted source/display file name on the canonical content
-- plane. The query-facing knowledge_document value is a projection and must
-- never be the only surviving copy used by rematerialization.
alter table content_document
    add column if not exists source_file_name text;

-- Legacy rows did not have a canonical file name. Seed the new canonical field
-- once from the scoped legacy projection when available, then fall back to the
-- external key. Runtime materialization never reads the projection back; this
-- join is migration-only preservation of already-visible user metadata.
update content_document as document
set source_file_name = coalesce(
    (
        select projection.file_name
        from knowledge_document as projection
        where projection.document_id = document.id
          and projection.workspace_id = document.workspace_id
          and projection.library_id = document.library_id
          and nullif(trim(projection.file_name), '') is not null
    ),
    document.external_key
)
where document.source_file_name is null;
