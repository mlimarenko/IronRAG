use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
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
    pub provider_catalog_id: Uuid,
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

/// Values required to create an AI binding.
///
/// Grouping the write payload keeps repository calls self-documenting and
/// prevents positional arguments with the same primitive type from being
/// accidentally swapped. `binding_state` is written verbatim; the service owns
/// the `active` default when a caller omits it.
pub struct CreateAiBindingInput<'a> {
    pub scope_kind: &'a str,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub binding_purpose: &'a str,
    pub account_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<&'a str>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
    pub binding_state: &'a str,
    pub updated_by_principal_id: Option<Uuid>,
}

/// Values required to update an AI binding.
///
/// The named payload keeps the write columns from being passed positionally;
/// `binding_state` is written verbatim.
pub struct UpdateAiBindingInput<'a> {
    pub binding_id: Uuid,
    pub account_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_catalog_id: Uuid,
    pub system_prompt: Option<&'a str>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
    pub binding_state: &'a str,
    pub updated_by_principal_id: Option<Uuid>,
}

/// Model fields shared by effective selection and runtime hydration queries.
#[derive(Debug, Clone, FromRow)]
pub struct EffectiveModelRow {
    pub model_catalog_id: Uuid,
    pub model_provider_catalog_id: Uuid,
    pub model_name: String,
    pub model_context_window: Option<i32>,
    pub model_max_output_tokens: Option<i32>,
    pub model_lifecycle_state: String,
    pub model_metadata_json: Value,
    pub model_currency_code: String,
    pub model_input_price_per_1m: Decimal,
    pub model_cached_input_price_per_1m: Decimal,
    pub model_cache_write_input_price_per_1m: Decimal,
    pub model_output_price_per_1m: Decimal,
    pub model_long_context_threshold_tokens: Option<i32>,
    pub model_long_context_input_price_per_1m: Option<Decimal>,
    pub model_long_context_cached_input_price_per_1m: Option<Decimal>,
    pub model_long_context_output_price_per_1m: Option<Decimal>,
    /// The model's own allowed-purpose contract, read from
    /// `ai_model_binding_purpose` rather than assumed from the requested
    /// purpose: a purpose an operator later removed from the model must be
    /// caught here, not treated as still allowed because the binding that
    /// referenced it still exists (IRG-182).
    pub model_allowed_binding_purposes: Vec<String>,
}

/// Secret-free row used to assemble an effective provider/model profile.
#[derive(Debug, Clone, FromRow)]
pub struct EffectiveProviderSelectionRow {
    pub binding_purpose: String,
    pub account_base_url: Option<String>,
    pub account_credential_state: String,
    #[sqlx(flatten)]
    pub model: EffectiveModelRow,
    pub provider_catalog_id: Uuid,
    pub provider_kind: String,
    pub provider_display_name: String,
    pub provider_api_style: String,
    pub provider_lifecycle_state: String,
    pub provider_default_base_url: Option<String>,
    pub provider_capability_flags_json: Value,
}

/// Secret-free inputs needed to reproduce the exact embedding execution
/// profile key inside a caller-owned transaction.
#[derive(Debug, Clone, FromRow)]
pub struct EffectiveEmbeddingProfileRow {
    pub binding_purpose: String,
    pub binding_extra_parameters_json: Value,
    pub account_base_url: Option<String>,
    pub account_credential_state: String,
    #[sqlx(flatten)]
    pub model: EffectiveModelRow,
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
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
    pub account_api_key: Option<String>,
    pub account_base_url: Option<String>,
    pub account_credential_state: String,
    #[sqlx(flatten)]
    pub model: EffectiveModelRow,
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
            provider_catalog_id,
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

const REQUESTED_EFFECTIVE_BINDING_CTE: &str = "
        with requested as (
            select
                input.binding_purpose::ai_binding_purpose as binding_purpose,
                min(input.ordinality)::bigint as requested_ordinal
            from unnest($2::text[]) with ordinality
                as input(binding_purpose, ordinality)
            group by input.binding_purpose
        )";

const SINGLE_REQUESTED_EFFECTIVE_BINDING_CTE: &str = "
        with requested as (
            select
                $2::ai_binding_purpose as binding_purpose,
                1::bigint as requested_ordinal
        )";

const EFFECTIVE_PROVIDER_SELECTION_COLUMNS: &str = "
            account.base_url as account_base_url,
            account.credential_state::text as account_credential_state,
            model.id as model_catalog_id,
            model.provider_catalog_id as model_provider_catalog_id,
            model.model_name,
            model.context_window as model_context_window,
            model.max_output_tokens as model_max_output_tokens,
            model.lifecycle_state::text as model_lifecycle_state,
            model.metadata_json as model_metadata_json,
            model.currency_code as model_currency_code,
            model.input_price_per_1m as model_input_price_per_1m,
            model.cached_input_price_per_1m as model_cached_input_price_per_1m,
            model.cache_write_input_price_per_1m as model_cache_write_input_price_per_1m,
            model.output_price_per_1m as model_output_price_per_1m,
            model.long_context_threshold_tokens as model_long_context_threshold_tokens,
            model.long_context_input_price_per_1m as model_long_context_input_price_per_1m,
            model.long_context_cached_input_price_per_1m as model_long_context_cached_input_price_per_1m,
            model.long_context_output_price_per_1m as model_long_context_output_price_per_1m,
            array(
                select relation.binding_purpose::text
                from ai_model_binding_purpose relation
                where relation.model_catalog_id = model.id
                order by relation.binding_purpose
            ) as model_allowed_binding_purposes,
            provider.id as provider_catalog_id,
            provider.provider_kind,
            provider.display_name as provider_display_name,
            provider.api_style::text as provider_api_style,
            provider.lifecycle_state::text as provider_lifecycle_state,
            provider.default_base_url as provider_default_base_url,
            provider.capability_flags_json as provider_capability_flags_json";

const EFFECTIVE_PROVIDER_SELECTION_JOINS: &str = "
         join ai_account account
           on (account.id, account.provider_catalog_id) =
              (binding.account_id, binding.provider_catalog_id)
         join ai_model_catalog model
           on (model.id, model.provider_catalog_id) =
              (binding.model_catalog_id, binding.provider_catalog_id)
         join ai_provider_catalog provider on provider.id = binding.provider_catalog_id
         where library.id = $1
         order by requested.requested_ordinal";

const REQUESTED_EFFECTIVE_BINDING_JOIN: &str ="
        cross join requested
        join lateral (
            select binding.*
            from ai_binding binding
            where binding.binding_purpose = requested.binding_purpose
              and binding.binding_state = 'active'
              and (
                  (binding.scope_kind = 'library' and binding.library_id = library.id)
                  or (binding.scope_kind = 'workspace' and binding.workspace_id = library.workspace_id)
                  or binding.scope_kind = 'instance'
              )
            order by
                case binding.scope_kind
                    when 'library' then 3
                    when 'workspace' then 2
                    when 'instance' then 1
                end desc,
                binding.updated_at desc,
                binding.id desc
            limit 1
        ) binding on true";

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
    sqlx::query_as::<_, AiBindingRow>(sqlx::AssertSqlSafe(format!(
        "{SINGLE_REQUESTED_EFFECTIVE_BINDING_CTE}
         select
            binding.id,
            binding.scope_kind::text as scope_kind,
            binding.workspace_id,
            binding.library_id,
            binding.binding_purpose::text as binding_purpose,
            binding.account_id,
            binding.provider_catalog_id,
            binding.model_catalog_id,
            binding.system_prompt,
            binding.temperature,
            binding.top_p,
            binding.max_output_tokens_override,
            binding.extra_parameters_json,
            binding.binding_state::text as binding_state,
            binding.updated_by_principal_id,
            binding.created_at,
            binding.updated_at
         from catalog_library library
         {REQUESTED_EFFECTIVE_BINDING_JOIN}
         where library.id = $1",
    )))
    .bind(library_id)
    .bind(binding_purpose)
    .fetch_optional(postgres)
    .await
}

/// Reports whether any requested library resolves the selected binding purpose
/// to a model that supports the required model purpose.
///
/// Effective binding precedence and tie-breaking are shared with the runtime
/// hydration queries. An empty library set performs no database I/O.
pub async fn any_effective_binding_model_supports_purpose(
    postgres: &PgPool,
    library_ids: &[Uuid],
    binding_purpose: &str,
    supported_model_purpose: &str,
) -> Result<bool, sqlx::Error> {
    if library_ids.is_empty() {
        return Ok(false);
    }

    sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
        "{SINGLE_REQUESTED_EFFECTIVE_BINDING_CTE}
         select exists (
            select 1
            from unnest($1::uuid[]) as requested_library(library_id)
            join catalog_library library on library.id = requested_library.library_id
            {REQUESTED_EFFECTIVE_BINDING_JOIN}
            join ai_account account
              on (account.id, account.provider_catalog_id) =
                 (binding.account_id, binding.provider_catalog_id)
            join ai_model_catalog model
              on (model.id, model.provider_catalog_id) =
                 (binding.model_catalog_id, binding.provider_catalog_id)
            join ai_provider_catalog provider on provider.id = binding.provider_catalog_id
            join ai_model_binding_purpose supported_purpose
              on supported_purpose.model_catalog_id = model.id
             and supported_purpose.binding_purpose = $3::ai_binding_purpose
         )",
    )))
    .bind(library_ids)
    .bind(binding_purpose)
    .bind(supported_model_purpose)
    .fetch_one(postgres)
    .await
}

/// Resolves complete runtime bindings with one `PostgreSQL` statement and one
/// pool acquisition. Duplicate purposes are collapsed in request order.
///
/// The lateral selection preserves the defined precedence and tie-breakers:
/// library, workspace, instance; then `updated_at desc, id desc`.
pub async fn list_effective_runtime_bindings(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purposes: &[String],
) -> Result<Vec<EffectiveRuntimeBindingRow>, sqlx::Error> {
    if binding_purposes.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, EffectiveRuntimeBindingRow>(sqlx::AssertSqlSafe(format!(
        "{REQUESTED_EFFECTIVE_BINDING_CTE}
         select
            library.workspace_id as resolved_workspace_id,
            library.id as resolved_library_id,
            binding.id as binding_id,
            binding.binding_purpose::text as binding_purpose,
            binding.account_id,
            binding.model_catalog_id,
            binding.system_prompt,
            binding.temperature,
            binding.top_p,
            binding.max_output_tokens_override,
            binding.extra_parameters_json,
            account.api_key as account_api_key,
            {EFFECTIVE_PROVIDER_SELECTION_COLUMNS}
         from catalog_library library
         {REQUESTED_EFFECTIVE_BINDING_JOIN}
         {EFFECTIVE_PROVIDER_SELECTION_JOINS}",
    )))
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

    sqlx::query_as::<_, EffectiveProviderSelectionRow>(sqlx::AssertSqlSafe(format!(
        "{REQUESTED_EFFECTIVE_BINDING_CTE}
         select
            binding.binding_purpose::text as binding_purpose,
            {EFFECTIVE_PROVIDER_SELECTION_COLUMNS}
         from catalog_library library
         {REQUESTED_EFFECTIVE_BINDING_JOIN}
         {EFFECTIVE_PROVIDER_SELECTION_JOINS}",
    )))
    .bind(library_id)
    .bind(binding_purposes)
    .fetch_all(postgres)
    .await
}

/// Resolves the active embedding profile without credentials on the supplied
/// lifecycle transaction. Publication uses this instead of acquiring another
/// pool connection while holding the library row lock.
pub async fn get_effective_embedding_profile_with_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> Result<Option<EffectiveEmbeddingProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, EffectiveEmbeddingProfileRow>(sqlx::AssertSqlSafe(format!(
        "{SINGLE_REQUESTED_EFFECTIVE_BINDING_CTE}
         select
            binding.binding_purpose::text as binding_purpose,
            binding.extra_parameters_json as binding_extra_parameters_json,
            {EFFECTIVE_PROVIDER_SELECTION_COLUMNS}
         from catalog_library library
         {REQUESTED_EFFECTIVE_BINDING_JOIN}
         {EFFECTIVE_PROVIDER_SELECTION_JOINS}",
    )))
    .bind(library_id)
    .bind("embed_chunk")
    .fetch_optional(&mut **transaction)
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

    sqlx::query_as::<_, EffectiveBindingIdentityRow>(sqlx::AssertSqlSafe(format!(
        "{REQUESTED_EFFECTIVE_BINDING_CTE}
         select
            binding.binding_purpose::text as binding_purpose,
            binding.id,
            binding.account_id,
            binding.model_catalog_id,
            binding.updated_at
         from catalog_library library
         {REQUESTED_EFFECTIVE_BINDING_JOIN}
         where library.id = $1
         order by binding.binding_purpose",
    )))
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
            provider_catalog_id,
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
            $1, $2::ai_scope_kind, $3, $4, $5::ai_binding_purpose, $6, $7, $8, $9, $10, $11,
            $12, $13, $14::ai_binding_state, $15, now(), now()
        )
        returning {BINDING_COLUMNS}",
    )))
    .bind(Uuid::now_v7())
    .bind(input.scope_kind)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.binding_purpose)
    .bind(input.account_id)
    .bind(input.provider_catalog_id)
    .bind(input.model_catalog_id)
    .bind(input.system_prompt)
    .bind(input.temperature)
    .bind(input.top_p)
    .bind(input.max_output_tokens_override)
    .bind(input.extra_parameters_json.clone())
    .bind(input.binding_state)
    .bind(input.updated_by_principal_id)
    .fetch_one(executor)
    .await
}

pub async fn update_binding(
    postgres: &PgPool,
    input: UpdateAiBindingInput<'_>,
) -> Result<Option<AiBindingRow>, sqlx::Error> {
    update_binding_in_connection(postgres, input).await
}

async fn update_binding_in_connection<'e, E>(
    executor: E,
    input: UpdateAiBindingInput<'_>,
) -> Result<Option<AiBindingRow>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, AiBindingRow>(sqlx::AssertSqlSafe(format!(
        "update ai_binding
         set account_id = $2,
             provider_catalog_id = $3,
             model_catalog_id = $4,
             system_prompt = $5,
             temperature = $6,
             top_p = $7,
             max_output_tokens_override = $8,
             extra_parameters_json = $9,
             binding_state = $10::ai_binding_state,
             updated_by_principal_id = $11,
             updated_at = now()
         where id = $1
         returning {BINDING_COLUMNS}",
    )))
    .bind(input.binding_id)
    .bind(input.account_id)
    .bind(input.provider_catalog_id)
    .bind(input.model_catalog_id)
    .bind(input.system_prompt)
    .bind(input.temperature)
    .bind(input.top_p)
    .bind(input.max_output_tokens_override)
    .bind(input.extra_parameters_json)
    .bind(input.binding_state)
    .bind(input.updated_by_principal_id)
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

    fn unavailable_pool() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(1))
            .connect_lazy("postgresql://postgres@127.0.0.1:1/ironrag")
            .expect("static PostgreSQL URL must parse")
    }

    #[tokio::test]
    async fn empty_provider_selection_batch_does_not_acquire_a_connection() {
        let pool = unavailable_pool();

        let selections = list_effective_provider_selections(&pool, Uuid::now_v7(), &[])
            .await
            .expect("empty profile should return before touching the pool");

        assert!(selections.is_empty());
        assert_eq!(pool.size(), 0);
    }

    #[test]
    fn single_binding_request_supports_shared_provider_ordering() {
        assert!(SINGLE_REQUESTED_EFFECTIVE_BINDING_CTE.contains("1::bigint as requested_ordinal"));
        assert!(EFFECTIVE_PROVIDER_SELECTION_JOINS.contains("requested.requested_ordinal"));
    }

    #[tokio::test]
    async fn empty_runtime_binding_batch_does_not_acquire_a_connection() {
        let pool = unavailable_pool();

        let bindings = list_effective_runtime_bindings(&pool, Uuid::now_v7(), &[])
            .await
            .expect("empty profile should return before touching the pool");

        assert!(bindings.is_empty());
        assert_eq!(pool.size(), 0);
    }

    #[tokio::test]
    async fn empty_binding_model_support_batch_does_not_acquire_a_connection() {
        let pool = unavailable_pool();

        let supports_purpose =
            any_effective_binding_model_supports_purpose(&pool, &[], "agent", "extract_text")
                .await
                .expect("empty library set should return before touching the pool");

        assert!(!supports_purpose);
        assert_eq!(pool.size(), 0);
    }

    #[tokio::test]
    async fn binding_queries_propagate_database_unavailability() {
        let postgres = unavailable_pool();
        let id = Uuid::nil();
        let purposes = vec!["query_answer".to_string()];

        assert!(list_bindings_exact(&postgres, "instance", None, None).await.is_err());
        assert!(list_effective_binding_purposes_for_libraries(&postgres, &[id]).await.is_err());
        assert!(get_binding_by_id(&postgres, id).await.is_err());
        assert!(get_effective_binding_by_purpose(&postgres, id, "query_answer").await.is_err());
        assert!(
            any_effective_binding_model_supports_purpose(
                &postgres,
                &[id],
                "agent",
                "extract_text",
            )
            .await
            .is_err()
        );
        assert!(list_effective_runtime_bindings(&postgres, id, &purposes).await.is_err());
        assert!(get_effective_runtime_binding(&postgres, id, "query_answer").await.is_err());
        assert!(list_effective_provider_selections(&postgres, id, &purposes).await.is_err());
        assert!(list_effective_binding_identities(&postgres, id, &purposes).await.is_err());
        assert!(
            create_binding(
                &postgres,
                CreateAiBindingInput {
                    scope_kind: "instance",
                    workspace_id: None,
                    library_id: None,
                    binding_purpose: "query_answer",
                    account_id: id,
                    provider_catalog_id: id,
                    model_catalog_id: id,
                    system_prompt: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens_override: None,
                    extra_parameters_json: serde_json::json!({}),
                    binding_state: "active",
                    updated_by_principal_id: None,
                },
            )
            .await
            .is_err()
        );
        assert!(
            update_binding(
                &postgres,
                UpdateAiBindingInput {
                    binding_id: id,
                    account_id: id,
                    provider_catalog_id: id,
                    model_catalog_id: id,
                    system_prompt: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens_override: None,
                    extra_parameters_json: serde_json::json!({}),
                    binding_state: "active",
                    updated_by_principal_id: None,
                },
            )
            .await
            .is_err()
        );
        assert!(delete_binding(&postgres, id).await.is_err());
        assert!(create_binding_validation(&postgres, id, "valid", None, None).await.is_err());
        assert!(get_binding_validation_by_id(&postgres, id).await.is_err());
        assert!(list_binding_validations(&postgres, id).await.is_err());
    }
}
