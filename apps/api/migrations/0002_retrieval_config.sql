-- 0002: per-library declarative retrieval configuration.
-- Adds retrieval_config jsonb to catalog_library.
-- All statements are idempotent so a repeated apply is safe.

alter table public.catalog_library
    add column if not exists retrieval_config jsonb
        default '{"lexical": {"textSearchConfig": "simple"}}'::jsonb not null;

do $$
begin
    if not exists (
        select 1 from pg_constraint
        where conname = 'catalog_library_retrieval_config_object_check'
          and conrelid = 'public.catalog_library'::regclass
    ) then
        alter table public.catalog_library
            add constraint catalog_library_retrieval_config_object_check
            check (jsonb_typeof(retrieval_config) = 'object'::text);
    end if;
end
$$;
