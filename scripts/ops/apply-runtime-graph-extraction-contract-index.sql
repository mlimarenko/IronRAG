-- Additive live index for the IRG-197 extraction-contract cache lookup.
--
-- After IRG-197 the semantic cache lookup filters
-- `raw_output_json->>'extraction_contract_hash'` instead of `prompt_hash`, so
-- the pre-existing `idx_runtime_graph_extraction_cache_lookup` (which indexes
-- `prompt_hash`) no longer serves it. This script replaces it with an
-- expression index over the new identity.
--
-- CANONICAL PATH AND ITS CONSTRAINT
-- The repository boots from one baseline migration and the migration lint
-- (`scripts/ops/lint_migrations.sh`) rejects any second migration file, so a
-- new index cannot arrive through sqlx. An additive baseline change adds the object to
-- `apps/api/migrations/0001_init.sql` (so fresh installs receive it) in
-- the same commit as an operator-applied script like this one, and updates
-- `scripts/ops/baseline-checksum.txt`. The IRG-197 review explicitly required
-- the baseline untouched, so this change ships only the operator script:
-- existing databases get the index, fresh installs do not, and the baseline
-- checksum is unchanged — no `_sqlx_migrations` compare-and-set is needed.
-- Adding the index to the baseline is the follow-up that closes the
-- fresh-install gap.
--
-- Run outside a transaction (both statements are CONCURRENTLY):
--
--   psql "<database-url>" -v ON_ERROR_STOP=1 \
--     -f scripts/ops/apply-runtime-graph-extraction-contract-index.sql
--
-- The new index shares the leading (library_id, extraction_version,
-- provider_kind, model_name) columns with the old one, so it also serves the
-- per-library and per-document/version reads the old index served; dropping the
-- old one loses no query plan. Idempotent: both statements are IF [NOT] EXISTS.

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_runtime_graph_extraction_contract_cache_lookup
    ON public.runtime_graph_extraction USING btree (
        library_id,
        extraction_version,
        provider_kind,
        model_name,
        (raw_output_json ->> 'extraction_contract_hash'),
        status,
        created_at DESC
    );

DROP INDEX CONCURRENTLY IF EXISTS public.idx_runtime_graph_extraction_cache_lookup;
