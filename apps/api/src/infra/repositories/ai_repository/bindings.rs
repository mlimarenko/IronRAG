use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiBindingAssignmentRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: String,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub binding_state: String,
    pub updated_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ActiveLibraryBindingPurposeRow {
    pub library_id: Uuid,
    pub binding_purpose: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiBindingValidationRow {
    pub id: Uuid,
    pub binding_id: Uuid,
    pub validation_state: String,
    pub checked_at: DateTime<Utc>,
    pub failure_code: Option<String>,
    pub message: Option<String>,
}

pub async fn list_binding_assignments_exact(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiBindingAssignmentRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingAssignmentRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
         from ai_binding_assignment
         where scope_kind = $1::ai_scope_kind
           and workspace_id is not distinct from $2
           and library_id is not distinct from $3
         order by created_at desc, id desc",
    )
    .bind(scope_kind)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_effective_binding_purposes_for_libraries(
    postgres: &PgPool,
    library_ids: &[Uuid],
) -> Result<Vec<ActiveLibraryBindingPurposeRow>, sqlx::Error> {
    if library_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ActiveLibraryBindingPurposeRow>(
        "with requested_libraries as (
            select unnest($1::uuid[]) as library_id
         )
         select
            requested_libraries.library_id,
            effective.binding_purpose
         from requested_libraries
         join catalog_library library on library.id = requested_libraries.library_id
         join lateral (
            select distinct on (candidate.binding_purpose)
                candidate.binding_purpose
            from (
                select binding_purpose::text as binding_purpose, 3 as precedence
                from ai_binding_assignment
                where scope_kind = 'library'
                  and library_id = requested_libraries.library_id
                  and binding_state = 'active'
                union all
                select binding_purpose::text as binding_purpose, 2 as precedence
                from ai_binding_assignment
                where scope_kind = 'workspace'
                  and workspace_id = library.workspace_id
                  and binding_state = 'active'
                union all
                select binding_purpose::text as binding_purpose, 1 as precedence
                from ai_binding_assignment
                where scope_kind = 'instance'
                  and binding_state = 'active'
            ) candidate
            order by candidate.binding_purpose, candidate.precedence desc
         ) effective on true",
    )
    .bind(library_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_binding_assignment_by_id(
    postgres: &PgPool,
    binding_id: Uuid,
) -> Result<Option<AiBindingAssignmentRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingAssignmentRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
         from ai_binding_assignment
         where id = $1",
    )
    .bind(binding_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_effective_binding_assignment_by_purpose(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purpose: &str,
) -> Result<Option<AiBindingAssignmentRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingAssignmentRow>(
        "select
            candidate.id,
            candidate.scope_kind,
            candidate.workspace_id,
            candidate.library_id,
            candidate.binding_purpose,
            candidate.provider_credential_id,
            candidate.model_preset_id,
            candidate.binding_state,
            candidate.updated_by_principal_id,
            candidate.created_at,
            candidate.updated_at
         from catalog_library library
         join lateral (
            select
                binding.id,
                binding.scope_kind::text as scope_kind,
                binding.workspace_id,
                binding.library_id,
                binding.binding_purpose::text as binding_purpose,
                binding.provider_credential_id,
                binding.model_preset_id,
                binding.binding_state::text as binding_state,
                binding.updated_by_principal_id,
                binding.created_at,
                binding.updated_at,
                3 as precedence
            from ai_binding_assignment binding
            where binding.scope_kind = 'library'
              and binding.library_id = library.id
              and binding.binding_purpose = $2::ai_binding_purpose
              and binding.binding_state = 'active'
            union all
            select
                binding.id,
                binding.scope_kind::text as scope_kind,
                binding.workspace_id,
                binding.library_id,
                binding.binding_purpose::text as binding_purpose,
                binding.provider_credential_id,
                binding.model_preset_id,
                binding.binding_state::text as binding_state,
                binding.updated_by_principal_id,
                binding.created_at,
                binding.updated_at,
                2 as precedence
            from ai_binding_assignment binding
            where binding.scope_kind = 'workspace'
              and binding.workspace_id = library.workspace_id
              and binding.binding_purpose = $2::ai_binding_purpose
              and binding.binding_state = 'active'
            union all
            select
                binding.id,
                binding.scope_kind::text as scope_kind,
                binding.workspace_id,
                binding.library_id,
                binding.binding_purpose::text as binding_purpose,
                binding.provider_credential_id,
                binding.model_preset_id,
                binding.binding_state::text as binding_state,
                binding.updated_by_principal_id,
                binding.created_at,
                binding.updated_at,
                1 as precedence
            from ai_binding_assignment binding
            where binding.scope_kind = 'instance'
              and binding.binding_purpose = $2::ai_binding_purpose
              and binding.binding_state = 'active'
            order by precedence desc, updated_at desc, id desc
            limit 1
         ) candidate on true
         where library.id = $1",
    )
    .bind(library_id)
    .bind(binding_purpose)
    .fetch_optional(postgres)
    .await
}

pub async fn create_binding_assignment(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    binding_purpose: &str,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    updated_by_principal_id: Option<Uuid>,
) -> Result<AiBindingAssignmentRow, sqlx::Error> {
    sqlx::query_as::<_, AiBindingAssignmentRow>(
        "insert into ai_binding_assignment (
            id,
            scope_kind,
            workspace_id,
            library_id,
            binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2::ai_scope_kind, $3, $4, $5::ai_binding_purpose, $6, $7, 'active', $8, now(), now())
        returning
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(scope_kind)
    .bind(workspace_id)
    .bind(library_id)
    .bind(binding_purpose)
    .bind(provider_credential_id)
    .bind(model_preset_id)
    .bind(updated_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_binding_assignment(
    postgres: &PgPool,
    binding_id: Uuid,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    binding_state: &str,
    updated_by_principal_id: Option<Uuid>,
) -> Result<Option<AiBindingAssignmentRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingAssignmentRow>(
        "update ai_binding_assignment
         set provider_credential_id = $2,
             model_preset_id = $3,
             binding_state = $4::ai_binding_state,
             updated_by_principal_id = $5,
             updated_at = now()
         where id = $1
         returning
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(binding_id)
    .bind(provider_credential_id)
    .bind(model_preset_id)
    .bind(binding_state)
    .bind(updated_by_principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_binding_assignment(
    postgres: &PgPool,
    binding_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from ai_binding_assignment where id = $1")
        .bind(binding_id)
        .execute(postgres)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn create_binding_validation(
    postgres: &PgPool,
    binding_id: Uuid,
    validation_state: &str,
    failure_code: Option<&str>,
    message: Option<&str>,
) -> Result<AiBindingValidationRow, sqlx::Error> {
    sqlx::query_as::<_, AiBindingValidationRow>(
        "insert into ai_binding_validation (
            id,
            binding_id,
            validation_state,
            checked_at,
            failure_code,
            message
        )
        values ($1, $2, $3::ai_validation_state, now(), $4, $5)
        returning
            id,
            binding_id,
            validation_state::text as validation_state,
            checked_at,
            failure_code,
            message",
    )
    .bind(Uuid::now_v7())
    .bind(binding_id)
    .bind(validation_state)
    .bind(failure_code)
    .bind(message)
    .fetch_one(postgres)
    .await
}

pub async fn list_binding_validations(
    postgres: &PgPool,
    binding_id: Uuid,
) -> Result<Vec<AiBindingValidationRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingValidationRow>(
        "select
            id,
            binding_id,
            validation_state::text as validation_state,
            checked_at,
            failure_code,
            message
         from ai_binding_validation
         where binding_id = $1
         order by checked_at desc",
    )
    .bind(binding_id)
    .fetch_all(postgres)
    .await
}
