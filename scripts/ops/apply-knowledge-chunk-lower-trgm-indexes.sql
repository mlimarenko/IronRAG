-- Additive live indexes for matching-terms LIKE prefilter.
--
-- Fresh installs get these from apps/api/migrations/0001_init.sql.
-- A database that already applied the previous baseline cannot receive them
-- through sqlx (editing 0001 would fail the checksum at boot). Apply this
-- file on the live Postgres with CONCURRENTLY, then update
-- `_sqlx_migrations.checksum` to the new binary's baseline checksum before
-- starting the new API image. See docs/en/CLI.md#schema-rollout.
--
-- Must be run outside a transaction (CREATE INDEX CONCURRENTLY).
-- After the three indexes exist, update sqlx's recorded checksum (SHA-384 of
-- apps/api/migrations/0001_init.sql) before booting the new binary:
--
--   UPDATE _sqlx_migrations
--   SET checksum = decode(
--     'bda1e4339498a6e35b23a3c990ddb76484b447c6cf0423963ed304f3ec895597bc9874b4922509deda5b2fd881f060bc',
--     'hex'
--   )
--   WHERE version = 1;

CREATE INDEX CONCURRENTLY IF NOT EXISTS knowledge_chunk_normalized_text_lower_trgm
    ON public.knowledge_chunk USING gin (lower(normalized_text) public.gin_trgm_ops);

CREATE INDEX CONCURRENTLY IF NOT EXISTS knowledge_chunk_content_text_lower_trgm
    ON public.knowledge_chunk USING gin (lower(content_text) public.gin_trgm_ops);

CREATE INDEX CONCURRENTLY IF NOT EXISTS knowledge_chunk_window_text_lower_trgm
    ON public.knowledge_chunk USING gin (lower(COALESCE(window_text, ''::text)) public.gin_trgm_ops);

CREATE INDEX CONCURRENTLY IF NOT EXISTS knowledge_chunk_library_lower_text_trgm
    ON public.knowledge_chunk USING gin (
        library_id,
        (
            lower(normalized_text)
            || chr(1)
            || lower(content_text)
            || chr(1)
            || lower(COALESCE(window_text, ''::text))
        ) public.gin_trgm_ops
    );
