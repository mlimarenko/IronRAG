-- Async catalog deletion support and workspace billing-summary hot path.
--
-- Catalog-level delete operations are tracked by ops_async_operation. Workspace
-- deletion has no single library subject, and library deletion must not cascade
-- away the operation row before clients finish polling it.

alter table ops_async_operation
    drop constraint if exists ops_async_operation_library_id_workspace_id_fkey;

alter table ops_async_operation
    alter column library_id drop not null;

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'ops_async_operation_library_workspace_fkey'
    ) then
        alter table ops_async_operation
            add constraint ops_async_operation_library_workspace_fkey
                foreign key (library_id, workspace_id)
                references catalog_library(id, workspace_id)
                on delete set null (library_id);
    end if;
end;
$$;

create index if not exists idx_billing_execution_cost_workspace
    on billing_execution_cost (workspace_id, library_id, knowledge_document_id)
    include (total_cost, currency_code, provider_call_count);

create unique index if not exists idx_ops_async_operation_active_workspace_delete
    on ops_async_operation (workspace_id, operation_kind, subject_kind, subject_id)
    where library_id is null
        and operation_kind = 'delete_workspace'
        and status in ('accepted', 'processing');

create unique index if not exists idx_ops_async_operation_active_library_delete
    on ops_async_operation (library_id, operation_kind, subject_kind, subject_id)
    where library_id is not null
        and operation_kind = 'delete_library'
        and status in ('accepted', 'processing');
