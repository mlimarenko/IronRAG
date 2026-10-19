-- Additive live schema: the decision-catalog AI binding purpose and provider
-- API style.
--
-- Fresh installs receive both enum values from
-- apps/api/migrations/0001_init.sql. Existing databases receive them only
-- through `ironrag-maintenance migrate decision-catalog-schema --apply`, which
-- holds a transaction-scoped advisory lock, applies this DDL, validates the
-- object groups its own target revision declares — the full current set at this
-- revision — and only then compare-and-sets the baseline checksum.
--
-- Every statement is idempotent: a database that already carries a value
-- succeeds unchanged, so a re-run after a failed stamp is safe. The `AFTER`
-- anchors keep the live order identical to a fresh install, where the baseline
-- declares each value at the same position, so enum-ordered arrays do not depend
-- on which path created the database.
ALTER TYPE public.ai_binding_purpose ADD VALUE IF NOT EXISTS 'graph_entity_type' AFTER 'extract_graph';
ALTER TYPE public.ai_provider_api_style ADD VALUE IF NOT EXISTS 'typesafe_system_one' AFTER 'gemini_generate_content';
