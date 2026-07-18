-- 0018_knowledge_cascade_fk_indexes.sql
--
-- Deleting a knowledge document cascades through revisions, chunks, and
-- their dependents (evidence, candidates, bundles, structured rows, facts).
-- Postgres enforces every ON DELETE CASCADE / SET NULL by looking up the
-- referencing rows per deleted parent row, and several referencing columns
-- had no index with that column in the leading position, so each parent
-- delete degenerated into a sequential scan of the referencing table. On
-- populated libraries a single document delete (connector reap, operator
-- purge, `repair orphan-knowledge-documents`) could run for minutes.
--
-- One index per uncovered foreign-key column, matching the FK lookup shape
-- exactly. SET NULL columns are nullable, so their indexes are partial: the
-- FK trigger only ever probes non-null values and the null majority would
-- bloat a full index for nothing.
--
-- Idempotent: `create index if not exists` throughout; re-running this file
-- is a clean no-op.

-- FKs referencing knowledge_document(document_id).
create index if not exists idx_knowledge_chunk_document_fk
    on knowledge_chunk (document_id);
create index if not exists idx_knowledge_evidence_document_fk
    on knowledge_evidence (document_id);
create index if not exists idx_knowledge_structured_block_document_fk
    on knowledge_structured_block (document_id);
create index if not exists idx_knowledge_structured_revision_document_fk
    on knowledge_structured_revision (document_id);
create index if not exists idx_knowledge_technical_fact_document_fk
    on knowledge_technical_fact (document_id);

-- FKs referencing knowledge_revision(revision_id).
create index if not exists idx_knowledge_evidence_revision_fk
    on knowledge_evidence (revision_id);
create index if not exists idx_knowledge_entity_candidate_revision_fk
    on knowledge_entity_candidate (revision_id);
create index if not exists idx_knowledge_relation_candidate_revision_fk
    on knowledge_relation_candidate (revision_id);

-- FKs referencing knowledge_chunk(chunk_id).
create index if not exists idx_knowledge_bundle_chunk_chunk_fk
    on knowledge_bundle_chunk (chunk_id);
create index if not exists idx_knowledge_evidence_chunk_fk
    on knowledge_evidence (chunk_id) where chunk_id is not null;
create index if not exists idx_knowledge_entity_candidate_chunk_fk
    on knowledge_entity_candidate (chunk_id) where chunk_id is not null;
create index if not exists idx_knowledge_relation_candidate_chunk_fk
    on knowledge_relation_candidate (chunk_id) where chunk_id is not null;
