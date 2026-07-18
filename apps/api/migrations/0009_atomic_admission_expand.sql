-- Expand-only schema for atomic content and web-ingest admission.
--
-- This migration deliberately keeps every new column nullable. Runtime code
-- dual-writes the new identity/linkage fields first; historical rows are
-- audited and backfilled before a later contract migration can make any of
-- these invariants mandatory.

alter table public.content_mutation
    add column if not exists idempotency_scope text,
    add column if not exists request_fingerprint text;

comment on column public.content_mutation.idempotency_scope is
    'Versioned actor/workspace scope used to serialize idempotent admission, including system requests without a principal.';

comment on column public.content_mutation.request_fingerprint is
    'Versioned SHA-256 fingerprint of the complete structural mutation request (target, source identity, and payload identity).';

create unique index if not exists idx_content_mutation_scoped_idempotency
    on public.content_mutation (idempotency_scope, request_surface, idempotency_key)
    where idempotency_scope is not null and idempotency_key is not null;

create index if not exists idx_content_mutation_request_fingerprint
    on public.content_mutation (request_fingerprint)
    where request_fingerprint is not null;

alter table public.ingest_job
    add column if not exists mutation_item_id uuid;

comment on column public.ingest_job.mutation_item_id is
    'Explicit content-mutation item owning this queue job. Nullable during expand/backfill rollout.';

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'ingest_job_mutation_item_id_fkey'
          and conrelid = 'public.ingest_job'::regclass
    ) then
        alter table public.ingest_job
            add constraint ingest_job_mutation_item_id_fkey
            foreign key (mutation_item_id)
            references public.content_mutation_item(id)
            on delete set null
            not valid;
    end if;
end
$$;

create unique index if not exists idx_ingest_job_content_mutation_item
    on public.ingest_job (mutation_item_id)
    where mutation_item_id is not null and job_kind = 'content_mutation';

create index if not exists idx_ops_async_operation_subject
    on public.ops_async_operation (subject_kind, subject_id, created_at desc, id desc)
    where subject_id is not null;
