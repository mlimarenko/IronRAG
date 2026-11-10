-- Live baseline delta: financial admission counters are not source truth.
--
-- Fresh installs receive this function from
-- apps/api/migrations/0001_init.sql. Existing databases receive it only
-- through `ironrag-maintenance migrate provider-source-generation-fence
-- --apply`, which holds a transaction-scoped advisory lock, applies this DDL,
-- validates the object groups its own target revision declares, and only then
-- compare-and-sets the baseline checksum.
--
-- `consume_financial_attempt` updates
-- `ai_provider_catalog.capability_flags_json -> 'financialAdmission'` for every
-- physical attempt. The previous AFTER UPDATE trigger treated that counter
-- write as a provider configuration change and bumped every bound library's
-- `source_truth_version`, which invalidated the vector-inventory fence in the
-- same query that had just reserved the attempt. Financial counters do not
-- change documents, chunks or embedding profiles, so only non-financial
-- provider configuration changes may advance source truth. The comparison
-- mirrors the clean-v1 catalog seed's upsert guard.
CREATE OR REPLACE FUNCTION public.invalidate_ai_provider_update_source_generations() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
declare
    affected_library_ids uuid[];
begin
    select coalesce(array_agg(library.id order by library.id), array[]::uuid[])
    into affected_library_ids
    from public.catalog_library library
    where exists (
        select 1
        from public.ai_binding binding
        join public.ai_account account on account.id = binding.account_id
        join public.ai_model_catalog model on model.id = binding.model_catalog_id
        join (
            select new_providers.id
            from new_providers
            join old_providers on old_providers.id = new_providers.id
            where (
                new_providers.provider_kind,
                new_providers.display_name,
                new_providers.api_style,
                new_providers.lifecycle_state,
                new_providers.default_base_url,
                new_providers.capability_flags_json - 'financialAdmission'
            ) is distinct from (
                old_providers.provider_kind,
                old_providers.display_name,
                old_providers.api_style,
                old_providers.lifecycle_state,
                old_providers.default_base_url,
                old_providers.capability_flags_json - 'financialAdmission'
            )
        ) changed_provider
          on changed_provider.id = account.provider_catalog_id
          or changed_provider.id = model.provider_catalog_id
        where binding.binding_state = 'active'::public.ai_binding_state
          and case binding.scope_kind
              when 'instance'::public.ai_scope_kind then true
              when 'workspace'::public.ai_scope_kind then
                  library.workspace_id = binding.workspace_id
              when 'library'::public.ai_scope_kind then
                  library.id = binding.library_id
              else false
          end
    );
    perform public.bump_library_source_truth_versions(affected_library_ids);
    return null;
end
$$;
