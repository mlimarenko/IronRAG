\set ON_ERROR_STOP on

-- Additive live indexes for ON DELETE SET NULL references to structured blocks.
--
-- Fresh installs get these from apps/api/migrations/0001_init.sql.
-- Run this file with psql outside an explicit transaction. CREATE INDEX
-- CONCURRENTLY waits for transactions that retain snapshots; do not cancel a
-- session until its statement has been verified as the redundant DELETE.
--
-- The preflight rejects a same-name index with a different definition or
-- validity state, and an equivalent index under another name. It therefore
-- never treats an invalid or differently named object as proof of completion.

DO $$
DECLARE
    required record;
    conflicting_index_name text;
BEGIN
    FOR required IN
        SELECT *
        FROM (
            VALUES
                (
                    'idx_knowledge_chunk_primary_block_fk',
                    'CREATE INDEX idx_knowledge_chunk_primary_block_fk ON public.knowledge_chunk USING btree (primary_block_id) WHERE (primary_block_id IS NOT NULL)'
                ),
                (
                    'idx_knowledge_evidence_block_fk',
                    'CREATE INDEX idx_knowledge_evidence_block_fk ON public.knowledge_evidence USING btree (block_id) WHERE (block_id IS NOT NULL)'
                ),
                (
                    'idx_knowledge_structured_block_parent_block_fk',
                    'CREATE INDEX idx_knowledge_structured_block_parent_block_fk ON public.knowledge_structured_block USING btree (parent_block_id) WHERE (parent_block_id IS NOT NULL)'
                )
        ) AS definitions(index_name, index_definition)
    LOOP
        SELECT index_relation.relname
        INTO conflicting_index_name
        FROM pg_catalog.pg_class AS index_relation
        LEFT JOIN pg_catalog.pg_index AS index_metadata
            ON index_metadata.indexrelid = index_relation.oid
        WHERE index_relation.relnamespace = 'public'::regnamespace
          AND index_relation.relname = required.index_name
          AND (
                index_metadata.indexrelid IS NULL
             OR NOT index_metadata.indisvalid
             OR NOT index_metadata.indisready
             OR NOT index_metadata.indislive
             OR pg_catalog.pg_get_indexdef(index_relation.oid) <> required.index_definition
          );
        IF FOUND THEN
            RAISE EXCEPTION
                'index % exists but is not the required valid definition',
                conflicting_index_name;
        END IF;

        SELECT index_relation.relname
        INTO conflicting_index_name
        FROM pg_catalog.pg_class AS index_relation
        JOIN pg_catalog.pg_index AS index_metadata
            ON index_metadata.indexrelid = index_relation.oid
        WHERE index_relation.relnamespace = 'public'::regnamespace
          AND index_relation.relname <> required.index_name
          AND index_metadata.indisvalid
          AND index_metadata.indisready
          AND index_metadata.indislive
          AND pg_catalog.pg_get_indexdef(index_relation.oid) = required.index_definition;
        IF FOUND THEN
            RAISE EXCEPTION
                'equivalent index % exists under a noncanonical name; reconcile it before applying IRG112',
                conflicting_index_name;
        END IF;
    END LOOP;
END
$$;

WITH required(index_name, ddl) AS (
    VALUES
        (
            'idx_knowledge_chunk_primary_block_fk',
            'CREATE INDEX CONCURRENTLY idx_knowledge_chunk_primary_block_fk ON public.knowledge_chunk USING btree (primary_block_id) WHERE (primary_block_id IS NOT NULL)'
        ),
        (
            'idx_knowledge_evidence_block_fk',
            'CREATE INDEX CONCURRENTLY idx_knowledge_evidence_block_fk ON public.knowledge_evidence USING btree (block_id) WHERE (block_id IS NOT NULL)'
        ),
        (
            'idx_knowledge_structured_block_parent_block_fk',
            'CREATE INDEX CONCURRENTLY idx_knowledge_structured_block_parent_block_fk ON public.knowledge_structured_block USING btree (parent_block_id) WHERE (parent_block_id IS NOT NULL)'
        )
)
SELECT ddl
FROM required
WHERE NOT EXISTS (
    SELECT 1
    FROM pg_catalog.pg_class AS index_relation
    WHERE index_relation.relnamespace = 'public'::regnamespace
      AND index_relation.relname = required.index_name
)
ORDER BY index_name
\gexec

DO $$
DECLARE
    required record;
BEGIN
    FOR required IN
        SELECT *
        FROM (
            VALUES
                (
                    'idx_knowledge_chunk_primary_block_fk',
                    'CREATE INDEX idx_knowledge_chunk_primary_block_fk ON public.knowledge_chunk USING btree (primary_block_id) WHERE (primary_block_id IS NOT NULL)'
                ),
                (
                    'idx_knowledge_evidence_block_fk',
                    'CREATE INDEX idx_knowledge_evidence_block_fk ON public.knowledge_evidence USING btree (block_id) WHERE (block_id IS NOT NULL)'
                ),
                (
                    'idx_knowledge_structured_block_parent_block_fk',
                    'CREATE INDEX idx_knowledge_structured_block_parent_block_fk ON public.knowledge_structured_block USING btree (parent_block_id) WHERE (parent_block_id IS NOT NULL)'
                )
        ) AS definitions(index_name, index_definition)
    LOOP
        IF NOT EXISTS (
            SELECT 1
            FROM pg_catalog.pg_class AS index_relation
            JOIN pg_catalog.pg_index AS index_metadata
                ON index_metadata.indexrelid = index_relation.oid
            WHERE index_relation.relnamespace = 'public'::regnamespace
              AND index_relation.relname = required.index_name
              AND index_metadata.indisvalid
              AND index_metadata.indisready
              AND index_metadata.indislive
              AND pg_catalog.pg_get_indexdef(index_relation.oid) = required.index_definition
        ) THEN
            RAISE EXCEPTION 'IRG112 did not create required valid index %', required.index_name;
        END IF;
    END LOOP;
END
$$;

-- Update `_sqlx_migrations.checksum` only after all three indexes are valid,
-- release artifacts are ready, and the previous checksum is verified. Use the
-- compare-and-set from docs/en/CLI.md; do not replace unknown
-- metadata. The statement must return exactly one row:
--
-- UPDATE _sqlx_migrations
-- SET checksum = decode('<new baseline SHA-384>', 'hex')
-- WHERE version = 1 AND success
--   AND checksum = decode('<verified previous baseline SHA-384>', 'hex')
-- RETURNING version;
