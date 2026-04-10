create type ai_scope_kind as enum ('instance', 'workspace', 'library');

alter table ai_library_model_binding
    drop constraint if exists ai_library_model_binding_library_id_workspace_id_fkey,
    drop constraint if exists ai_library_model_binding_provider_credential_id_workspace_id_fkey,
    drop constraint if exists ai_library_model_binding_provider_credential_id_workspace__fkey,
    drop constraint if exists ai_library_model_binding_model_preset_id_workspace_id_fkey,
    drop constraint if exists ai_library_model_binding_library_id_binding_purpose_key;

alter table ai_provider_credential
    add column scope_kind ai_scope_kind not null default 'workspace',
    add column library_id uuid,
    alter column workspace_id drop not null;

alter table ai_provider_credential
    drop constraint if exists ai_provider_credential_workspace_id_provider_catalog_id_label_key cascade,
    drop constraint if exists ai_provider_credential_workspace_id_provider_catalog_id_lab_key cascade,
    drop constraint if exists ai_provider_credential_id_workspace_id_key cascade;

alter table ai_provider_credential
    add constraint ai_provider_credential_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    add constraint ai_provider_credential_scope_check
        check (
            (scope_kind = 'instance' and workspace_id is null and library_id is null)
            or (scope_kind = 'workspace' and workspace_id is not null and library_id is null)
            or (scope_kind = 'library' and workspace_id is not null and library_id is not null)
        );

create unique index ai_provider_credential_instance_label_key
    on ai_provider_credential (provider_catalog_id, label)
    where scope_kind = 'instance';

create unique index ai_provider_credential_workspace_label_key
    on ai_provider_credential (workspace_id, provider_catalog_id, label)
    where scope_kind = 'workspace';

create unique index ai_provider_credential_library_label_key
    on ai_provider_credential (library_id, provider_catalog_id, label)
    where scope_kind = 'library';

create index ai_provider_credential_scope_idx
    on ai_provider_credential (scope_kind, workspace_id, library_id, provider_catalog_id);

alter table ai_provider_credential alter column scope_kind drop default;

alter table ai_model_preset
    add column scope_kind ai_scope_kind not null default 'workspace',
    add column library_id uuid,
    alter column workspace_id drop not null;

alter table ai_model_preset
    drop constraint if exists ai_model_preset_workspace_id_model_catalog_id_preset_name_key cascade,
    drop constraint if exists ai_model_preset_id_workspace_id_key cascade;

alter table ai_model_preset
    add constraint ai_model_preset_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    add constraint ai_model_preset_scope_check
        check (
            (scope_kind = 'instance' and workspace_id is null and library_id is null)
            or (scope_kind = 'workspace' and workspace_id is not null and library_id is null)
            or (scope_kind = 'library' and workspace_id is not null and library_id is not null)
        );

create unique index ai_model_preset_instance_name_key
    on ai_model_preset (model_catalog_id, preset_name)
    where scope_kind = 'instance';

create unique index ai_model_preset_workspace_name_key
    on ai_model_preset (workspace_id, model_catalog_id, preset_name)
    where scope_kind = 'workspace';

create unique index ai_model_preset_library_name_key
    on ai_model_preset (library_id, model_catalog_id, preset_name)
    where scope_kind = 'library';

create index ai_model_preset_scope_idx
    on ai_model_preset (scope_kind, workspace_id, library_id, model_catalog_id);

alter table ai_model_preset alter column scope_kind drop default;

alter table ai_library_model_binding rename to ai_binding_assignment;

alter table ai_binding_assignment
    add column scope_kind ai_scope_kind not null default 'library',
    alter column workspace_id drop not null,
    alter column library_id drop not null;

alter table ai_binding_assignment
    add constraint ai_binding_assignment_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    add constraint ai_binding_assignment_provider_credential_fkey
        foreign key (provider_credential_id)
        references ai_provider_credential(id)
        on delete restrict,
    add constraint ai_binding_assignment_model_preset_fkey
        foreign key (model_preset_id)
        references ai_model_preset(id)
        on delete restrict,
    add constraint ai_binding_assignment_scope_check
        check (
            (scope_kind = 'instance' and workspace_id is null and library_id is null)
            or (scope_kind = 'workspace' and workspace_id is not null and library_id is null)
            or (scope_kind = 'library' and workspace_id is not null and library_id is not null)
        );

create unique index ai_binding_assignment_instance_purpose_key
    on ai_binding_assignment (binding_purpose)
    where scope_kind = 'instance';

create unique index ai_binding_assignment_workspace_purpose_key
    on ai_binding_assignment (workspace_id, binding_purpose)
    where scope_kind = 'workspace';

create unique index ai_binding_assignment_library_purpose_key
    on ai_binding_assignment (library_id, binding_purpose)
    where scope_kind = 'library';

create index ai_binding_assignment_scope_idx
    on ai_binding_assignment (scope_kind, workspace_id, library_id, binding_purpose, binding_state);

alter table ai_binding_assignment alter column scope_kind drop default;
