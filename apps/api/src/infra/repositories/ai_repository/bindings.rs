use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Debug, Clone, FromRow)]
pub struct AiBindingRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: String,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
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
pub struct EffectiveBindingIdentityRow {
    pub binding_purpose: String,
    pub id: Uuid,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub updated_at: DateTime<Utc>,
}

/// Values required to create an active AI binding.
///
/// Grouping the write payload keeps repository calls self-documenting and
/// prevents positional arguments with the same primitive type from being
/// accidentally swapped.
pub struct CreateAiBindingInput<'a> {
    pub scope_kind: &'a str,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: &'a str,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<&'a str>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
    pub updated_by_principal_id: Option<Uuid>,
}

/// Secret-free row used to assemble an effective provider/model profile.
#[derive(Debug, Clone, FromRow)]
pub struct EffectiveProviderSelectionRow {
    pub binding_purpose: String,
    pub account_base_url: Option<String>,
    pub account_credential_state: String,
    pub model_catalog_id: Uuid,
    pub model_provider_catalog_id: Uuid,
    pub model_name: String,
    pub model_capability_kind: String,
    pub model_modality_kind: String,
    pub model_context_window: Option<i32>,
    pub model_max_output_tokens: Option<i32>,
    pub model_lifecycle_state: String,
    pub model_metadata_json: Value,
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub provider_display_name: String,
    pub provider_api_style: String,
    pub provider_lifecycle_state: String,
    pub provider_default_base_url: Option<String>,
    pub provider_capability_flags_json: Value,
}

/// Fully hydrated effective runtime binding selected for one library purpose.
///
/// This row deliberately has no `Debug` or `Clone` implementation because it
/// carries the persisted account credential. Callers must map it directly into
/// the redacted runtime domain type and keep its plaintext lifetime bounded.
#[derive(FromRow)]
pub struct EffectiveRuntimeBindingRow {
    pub resolved_workspace_id: Uuid,
    pub resolved_library_id: Uuid,
    pub binding_id: Uuid,
    pub binding_purpose: String,
    pub account_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
    pub account_api_key: Option<String>,
    pub account_base_url: Option<String>,
    pub account_credential_state: String,
    pub model_provider_catalog_id: Uuid,
    pub model_name: String,
    pub model_capability_kind: String,
    pub model_modality_kind: String,
    pub model_context_window: Option<i32>,
    pub model_max_output_tokens: Option<i32>,
    pub model_lifecycle_state: String,
    pub model_metadata_json: Value,
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub provider_display_name: String,
    pub provider_api_style: String,
    pub provider_lifecycle_state: String,
    pub provider_default_base_url: Option<String>,
    pub provider_capability_flags_json: Value,
}

impl Drop for EffectiveRuntimeBindingRow {
    fn drop(&mut self) {
        if let Some(api_key) = &mut self.account_api_key {
            api_key.zeroize();
        }
    }
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

const BINDING_COLUMNS: &str = "
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            account_id,
            model_catalog_id,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at";

pub async fn list_bindings_exact(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingRow>(sqlx::AssertSqlSafe(format!(
        "select {BINDING_COLUMNS}
         from ai_binding
         where scope_kind = $1::ai_scope_kind
           and workspace_id is not distinct from $2
           and library_id is not distinct from $3
         order by created_at desc, id desc",
    )))
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
                from ai_binding
                where scope_kind = 'library'
                  and library_id = requested_libraries.library_id
                  and binding_state = 'active'
                union all
                select binding_purpose::text as binding_purpose, 2 as precedence
                from ai_binding
                where scope_kind = 'workspace'
                  and workspace_id = library.workspace_id
                  and binding_state = 'active'
                union all
                select binding_purpose::text as binding_purpose, 1 as precedence
                from ai_binding
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

pub async fn get_binding_by_id(
    postgres: &PgPool,
    binding_id: Uuid,
) -> Result<Option<AiBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingRow>(sqlx::AssertSqlSafe(format!(
        "select {BINDING_COLUMNS}
         from ai_binding
         where id = $1",
    )))
    .bind(binding_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_effective_binding_by_purpose(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purpose: &str,
) -> Result<Option<AiBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingRow>(
        "select
            candidate.id,
            candidate.scope_kind,
            candidate.workspace_id,
            candidate.library_id,
            candidate.binding_purpose,
            candidate.account_id,
            candidate.model_catalog_id,
            candidate.system_prompt,
            candidate.temperature,
            candidate.top_p,
            candidate.max_output_tokens_override,
            candidate.extra_parameters_json,
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
                binding.account_id,
                binding.model_catalog_id,
                binding.system_prompt,
                binding.temperature,
                binding.top_p,
                binding.max_output_tokens_override,
                binding.extra_parameters_json,
                binding.binding_state::text as binding_state,
                binding.updated_by_principal_id,
                binding.created_at,
                binding.updated_at,
                3 as precedence
            from ai_binding binding
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
                binding.account_id,
                binding.model_catalog_id,
                binding.system_prompt,
                binding.temperature,
                binding.top_p,
                binding.max_output_tokens_override,
                binding.extra_parameters_json,
                binding.binding_state::text as binding_state,
                binding.updated_by_principal_id,
                binding.created_at,
                binding.updated_at,
                2 as precedence
            from ai_binding binding
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
                binding.account_id,
                binding.model_catalog_id,
                binding.system_prompt,
                binding.temperature,
                binding.top_p,
                binding.max_output_tokens_override,
                binding.extra_parameters_json,
                binding.binding_state::text as binding_state,
                binding.updated_by_principal_id,
                binding.created_at,
                binding.updated_at,
                1 as precedence
            from ai_binding binding
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

/// Resolves complete runtime bindings with one `PostgreSQL` statement and one
/// pool acquisition. Duplicate purposes are collapsed in request order.
///
/// The lateral selection preserves the canonical precedence and tie-breakers:
/// library, workspace, instance; then `updated_at desc, id desc`.
pub async fn list_effective_runtime_bindings(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purposes: &[String],
) -> Result<Vec<EffectiveRuntimeBindingRow>, sqlx::Error> {
    if binding_purposes.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, EffectiveRuntimeBindingRow>(
        "with requested as (
            select
                input.binding_purpose::ai_binding_purpose as binding_purpose,
                min(input.ordinality)::bigint as requested_ordinal
            from unnest($2::text[]) with ordinality
                as input(binding_purpose, ordinality)
            group by input.binding_purpose
         )
         select
            library.workspace_id as resolved_workspace_id,
            library.id as resolved_library_id,
            binding.id as binding_id,
            binding.binding_purpose,
            binding.account_id,
            binding.model_catalog_id,
            binding.system_prompt,
            binding.temperature,
            binding.top_p,
            binding.max_output_tokens_override,
            binding.extra_parameters_json,
            account.api_key as account_api_key,
            account.base_url as account_base_url,
            account.credential_state::text as account_credential_state,
            model.provider_catalog_id as model_provider_catalog_id,
            model.model_name,
            model.capability_kind::text as model_capability_kind,
            model.modality_kind::text as model_modality_kind,
            model.context_window as model_context_window,
            model.max_output_tokens as model_max_output_tokens,
            model.lifecycle_state::text as model_lifecycle_state,
            model.metadata_json as model_metadata_json,
            provider.id as provider_catalog_id,
            provider.provider_kind,
            provider.display_name as provider_display_name,
            provider.api_style::text as provider_api_style,
            provider.lifecycle_state::text as provider_lifecycle_state,
            provider.default_base_url as provider_default_base_url,
            provider.capability_flags_json as provider_capability_flags_json
         from catalog_library library
         cross join requested
         join lateral (
            select candidate.*
            from (
                select
                    library_binding.id,
                    library_binding.binding_purpose::text as binding_purpose,
                    library_binding.account_id,
                    library_binding.model_catalog_id,
                    library_binding.system_prompt,
                    library_binding.temperature,
                    library_binding.top_p,
                    library_binding.max_output_tokens_override,
                    library_binding.extra_parameters_json,
                    library_binding.updated_at,
                    3 as precedence
                from ai_binding library_binding
                where library_binding.scope_kind = 'library'
                  and library_binding.library_id = library.id
                  and library_binding.binding_purpose = requested.binding_purpose
                  and library_binding.binding_state = 'active'
                union all
                select
                    workspace_binding.id,
                    workspace_binding.binding_purpose::text as binding_purpose,
                    workspace_binding.account_id,
                    workspace_binding.model_catalog_id,
                    workspace_binding.system_prompt,
                    workspace_binding.temperature,
                    workspace_binding.top_p,
                    workspace_binding.max_output_tokens_override,
                    workspace_binding.extra_parameters_json,
                    workspace_binding.updated_at,
                    2 as precedence
                from ai_binding workspace_binding
                where workspace_binding.scope_kind = 'workspace'
                  and workspace_binding.workspace_id = library.workspace_id
                  and workspace_binding.binding_purpose = requested.binding_purpose
                  and workspace_binding.binding_state = 'active'
                union all
                select
                    instance_binding.id,
                    instance_binding.binding_purpose::text as binding_purpose,
                    instance_binding.account_id,
                    instance_binding.model_catalog_id,
                    instance_binding.system_prompt,
                    instance_binding.temperature,
                    instance_binding.top_p,
                    instance_binding.max_output_tokens_override,
                    instance_binding.extra_parameters_json,
                    instance_binding.updated_at,
                    1 as precedence
                from ai_binding instance_binding
                where instance_binding.scope_kind = 'instance'
                  and instance_binding.binding_purpose = requested.binding_purpose
                  and instance_binding.binding_state = 'active'
            ) candidate
            order by candidate.precedence desc, candidate.updated_at desc, candidate.id desc
            limit 1
         ) binding on true
         join ai_account account on account.id = binding.account_id
         join ai_model_catalog model on model.id = binding.model_catalog_id
         join ai_provider_catalog provider on provider.id = account.provider_catalog_id
         where library.id = $1
         order by requested.requested_ordinal",
    )
    .bind(library_id)
    .bind(binding_purposes)
    .fetch_all(postgres)
    .await
}

pub async fn get_effective_runtime_binding(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purpose: &str,
) -> Result<Option<EffectiveRuntimeBindingRow>, sqlx::Error> {
    let mut rows =
        list_effective_runtime_bindings(postgres, library_id, &[binding_purpose.to_string()])
            .await?;
    Ok(rows.pop())
}

/// Resolves provider/model selections for a complete runtime profile with one
/// statement, without selecting account credentials or binding prompts.
pub async fn list_effective_provider_selections(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purposes: &[String],
) -> Result<Vec<EffectiveProviderSelectionRow>, sqlx::Error> {
    if binding_purposes.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, EffectiveProviderSelectionRow>(
        "with requested as (
            select
                input.binding_purpose::ai_binding_purpose as binding_purpose,
                min(input.ordinality)::bigint as requested_ordinal
            from unnest($2::text[]) with ordinality
                as input(binding_purpose, ordinality)
            group by input.binding_purpose
         )
         select
            binding.binding_purpose,
            account.base_url as account_base_url,
            account.credential_state::text as account_credential_state,
            model.id as model_catalog_id,
            model.provider_catalog_id as model_provider_catalog_id,
            model.model_name,
            model.capability_kind::text as model_capability_kind,
            model.modality_kind::text as model_modality_kind,
            model.context_window as model_context_window,
            model.max_output_tokens as model_max_output_tokens,
            model.lifecycle_state::text as model_lifecycle_state,
            model.metadata_json as model_metadata_json,
            provider.id as provider_catalog_id,
            provider.provider_kind,
            provider.display_name as provider_display_name,
            provider.api_style::text as provider_api_style,
            provider.lifecycle_state::text as provider_lifecycle_state,
            provider.default_base_url as provider_default_base_url,
            provider.capability_flags_json as provider_capability_flags_json
         from catalog_library library
         cross join requested
         join lateral (
            select candidate.*
            from (
                select
                    library_binding.id,
                    library_binding.binding_purpose::text as binding_purpose,
                    library_binding.account_id,
                    library_binding.model_catalog_id,
                    library_binding.updated_at,
                    3 as precedence
                from ai_binding library_binding
                where library_binding.scope_kind = 'library'
                  and library_binding.library_id = library.id
                  and library_binding.binding_purpose = requested.binding_purpose
                  and library_binding.binding_state = 'active'
                union all
                select
                    workspace_binding.id,
                    workspace_binding.binding_purpose::text as binding_purpose,
                    workspace_binding.account_id,
                    workspace_binding.model_catalog_id,
                    workspace_binding.updated_at,
                    2 as precedence
                from ai_binding workspace_binding
                where workspace_binding.scope_kind = 'workspace'
                  and workspace_binding.workspace_id = library.workspace_id
                  and workspace_binding.binding_purpose = requested.binding_purpose
                  and workspace_binding.binding_state = 'active'
                union all
                select
                    instance_binding.id,
                    instance_binding.binding_purpose::text as binding_purpose,
                    instance_binding.account_id,
                    instance_binding.model_catalog_id,
                    instance_binding.updated_at,
                    1 as precedence
                from ai_binding instance_binding
                where instance_binding.scope_kind = 'instance'
                  and instance_binding.binding_purpose = requested.binding_purpose
                  and instance_binding.binding_state = 'active'
            ) candidate
            order by candidate.precedence desc, candidate.updated_at desc, candidate.id desc
            limit 1
         ) binding on true
         join ai_account account on account.id = binding.account_id
         join ai_model_catalog model on model.id = binding.model_catalog_id
         join ai_provider_catalog provider on provider.id = account.provider_catalog_id
         where library.id = $1
         order by requested.requested_ordinal",
    )
    .bind(library_id)
    .bind(binding_purposes)
    .fetch_all(postgres)
    .await
}

/// Resolves lightweight effective binding identities for a cache fingerprint
/// in one set-based query. Secret-bearing prompt/credential fields are never
/// selected.
pub async fn list_effective_binding_identities(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purposes: &[String],
) -> Result<Vec<EffectiveBindingIdentityRow>, sqlx::Error> {
    if binding_purposes.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, EffectiveBindingIdentityRow>(
        "select distinct on (candidate.binding_purpose)
            candidate.binding_purpose,
            candidate.id,
            candidate.account_id,
            candidate.model_catalog_id,
            candidate.updated_at
         from catalog_library library
         join lateral (
            select
                binding.binding_purpose::text as binding_purpose,
                binding.id,
                binding.account_id,
                binding.model_catalog_id,
                binding.updated_at,
                3 as precedence
            from ai_binding binding
            where binding.scope_kind = 'library'
              and binding.library_id = library.id
              and binding.binding_state = 'active'
              and binding.binding_purpose::text = any($2::text[])
            union all
            select
                binding.binding_purpose::text as binding_purpose,
                binding.id,
                binding.account_id,
                binding.model_catalog_id,
                binding.updated_at,
                2 as precedence
            from ai_binding binding
            where binding.scope_kind = 'workspace'
              and binding.workspace_id = library.workspace_id
              and binding.binding_state = 'active'
              and binding.binding_purpose::text = any($2::text[])
            union all
            select
                binding.binding_purpose::text as binding_purpose,
                binding.id,
                binding.account_id,
                binding.model_catalog_id,
                binding.updated_at,
                1 as precedence
            from ai_binding binding
            where binding.scope_kind = 'instance'
              and binding.binding_state = 'active'
              and binding.binding_purpose::text = any($2::text[])
         ) candidate on true
         where library.id = $1
         order by candidate.binding_purpose, candidate.precedence desc, candidate.updated_at desc, candidate.id desc",
    )
    .bind(library_id)
    .bind(binding_purposes)
    .fetch_all(postgres)
    .await
}

pub async fn create_binding(
    postgres: &PgPool,
    input: CreateAiBindingInput<'_>,
) -> Result<AiBindingRow, sqlx::Error> {
    create_binding_in_connection(postgres, &input).await
}

async fn create_binding_in_connection<'e, E>(
    executor: E,
    input: &CreateAiBindingInput<'_>,
) -> Result<AiBindingRow, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, AiBindingRow>(sqlx::AssertSqlSafe(format!(
        "insert into ai_binding (
            id,
            scope_kind,
            workspace_id,
            library_id,
            binding_purpose,
            account_id,
            model_catalog_id,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
        )
        values (
            $1, $2::ai_scope_kind, $3, $4, $5::ai_binding_purpose, $6, $7, $8, $9, $10, $11, $12,
            'active', $13, now(), now()
        )
        returning {BINDING_COLUMNS}",
    )))
    .bind(Uuid::now_v7())
    .bind(input.scope_kind)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.binding_purpose)
    .bind(input.account_id)
    .bind(input.model_catalog_id)
    .bind(input.system_prompt)
    .bind(input.temperature)
    .bind(input.top_p)
    .bind(input.max_output_tokens_override)
    .bind(input.extra_parameters_json.clone())
    .bind(input.updated_by_principal_id)
    .fetch_one(executor)
    .await
}

pub async fn update_binding(
    postgres: &PgPool,
    binding_id: Uuid,
    account_id: Uuid,
    model_catalog_id: Uuid,
    system_prompt: Option<&str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
    extra_parameters_json: Value,
    binding_state: &str,
    updated_by_principal_id: Option<Uuid>,
) -> Result<Option<AiBindingRow>, sqlx::Error> {
    update_binding_in_connection(
        postgres,
        binding_id,
        account_id,
        model_catalog_id,
        system_prompt,
        temperature,
        top_p,
        max_output_tokens_override,
        extra_parameters_json,
        binding_state,
        updated_by_principal_id,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn update_binding_in_connection<'e, E>(
    executor: E,
    binding_id: Uuid,
    account_id: Uuid,
    model_catalog_id: Uuid,
    system_prompt: Option<&str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
    extra_parameters_json: Value,
    binding_state: &str,
    updated_by_principal_id: Option<Uuid>,
) -> Result<Option<AiBindingRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, AiBindingRow>(sqlx::AssertSqlSafe(format!(
        "update ai_binding
         set account_id = $2,
             model_catalog_id = $3,
             system_prompt = $4,
             temperature = $5,
             top_p = $6,
             max_output_tokens_override = $7,
             extra_parameters_json = $8,
             binding_state = $9::ai_binding_state,
             updated_by_principal_id = $10,
             updated_at = now()
         where id = $1
         returning {BINDING_COLUMNS}",
    )))
    .bind(binding_id)
    .bind(account_id)
    .bind(model_catalog_id)
    .bind(system_prompt)
    .bind(temperature)
    .bind(top_p)
    .bind(max_output_tokens_override)
    .bind(extra_parameters_json)
    .bind(binding_state)
    .bind(updated_by_principal_id)
    .fetch_optional(executor)
    .await
}

pub async fn delete_binding(postgres: &PgPool, binding_id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from ai_binding where id = $1")
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

pub async fn get_binding_validation_by_id(
    postgres: &PgPool,
    validation_id: Uuid,
) -> Result<Option<AiBindingValidationRow>, sqlx::Error> {
    sqlx::query_as::<_, AiBindingValidationRow>(
        "select
            id,
            binding_id,
            validation_state::text as validation_state,
            checked_at,
            failure_code,
            message
         from ai_binding_validation
         where id = $1",
    )
    .bind(validation_id)
    .fetch_optional(postgres)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::postgres::PgPoolOptions;

    use super::*;

    #[tokio::test]
    async fn empty_provider_selection_batch_does_not_acquire_a_connection() {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(10))
            .connect_lazy("postgresql://127.0.0.1:9/ironrag_unreachable")
            .expect("lazy pool configuration should parse");

        let selections = list_effective_provider_selections(&pool, Uuid::now_v7(), &[])
            .await
            .expect("empty profile should return before touching the pool");

        assert!(selections.is_empty());
        assert_eq!(pool.size(), 0);
    }

    #[tokio::test]
    async fn empty_runtime_binding_batch_does_not_acquire_a_connection() {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(10))
            .connect_lazy("postgresql://127.0.0.1:9/ironrag_unreachable")
            .expect("lazy pool configuration should parse");

        let bindings = list_effective_runtime_bindings(&pool, Uuid::now_v7(), &[])
            .await
            .expect("empty profile should return before touching the pool");

        assert!(bindings.is_empty());
        assert_eq!(pool.size(), 0);
    }
}
