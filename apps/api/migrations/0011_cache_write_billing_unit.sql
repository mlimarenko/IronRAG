-- Cache writes are a distinct usage dimension. The row-based price catalog
-- represents an unknown price by the absence of a matching row; this migration
-- deliberately seeds no cache-write prices and never falls back to input rates.
alter type public.billing_unit
    add value if not exists 'per_1m_cache_write_input_tokens';

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conrelid = 'public.ai_price_catalog'::regclass
          and conname = 'ai_price_catalog_unit_price_nonnegative'
    ) then
        alter table public.ai_price_catalog
            add constraint ai_price_catalog_unit_price_nonnegative
            check (unit_price >= 0);
    end if;
end
$$;

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conrelid = 'public.billing_usage'::regclass
          and conname = 'billing_usage_quantity_nonnegative'
    ) then
        alter table public.billing_usage
            add constraint billing_usage_quantity_nonnegative
            check (quantity >= 0);
    end if;
end
$$;
