-- 0017_ai_catalog_hard_delete_restrict.sql
--
-- REST API redesign (memory/2026-07-17-rest-api-query-refactor-plan.md, AI
-- domain): DELETE /v1/ai/providers/{providerId} and DELETE
-- /v1/ai/models/{modelId} become real hard deletes and must 409 when the row
-- is still referenced (models under a provider; prices under a model),
-- matching the existing account/binding/price-override delete contract
-- (foreign-key violation -> 409 via map_ai_delete_error). Both FKs were
-- declared `on delete cascade`, which would silently delete dependents
-- instead of blocking the parent delete. Flip them to `on delete restrict`
-- so a referenced provider/model cannot be hard-deleted out from under the
-- rows that still point at it. `ai_binding` already references
-- `ai_model_catalog`/`ai_account` with `on delete restrict` (see
-- 0004_ai_config_simplification.sql), so this brings the remaining two FKs
-- in line with the same convention.
--
-- Idempotent: each block only fires when the constraint is not already
-- `on delete restrict` (confdeltype <> 'r'), so re-running this file is a
-- clean no-op.

do $$
begin
    if exists (
        select 1 from pg_constraint
        where conname = 'ai_model_catalog_provider_catalog_id_fkey'
          and conrelid = 'public.ai_model_catalog'::regclass
          and confdeltype <> 'r'
    ) then
        alter table public.ai_model_catalog
            drop constraint ai_model_catalog_provider_catalog_id_fkey;
        alter table public.ai_model_catalog
            add constraint ai_model_catalog_provider_catalog_id_fkey
                foreign key (provider_catalog_id)
                references public.ai_provider_catalog(id)
                on delete restrict;
    end if;
end
$$;

do $$
begin
    if exists (
        select 1 from pg_constraint
        where conname = 'ai_price_catalog_model_catalog_id_fkey'
          and conrelid = 'public.ai_price_catalog'::regclass
          and confdeltype <> 'r'
    ) then
        alter table public.ai_price_catalog
            drop constraint ai_price_catalog_model_catalog_id_fkey;
        alter table public.ai_price_catalog
            add constraint ai_price_catalog_model_catalog_id_fkey
                foreign key (model_catalog_id)
                references public.ai_model_catalog(id)
                on delete restrict;
    end if;
end
$$;
