use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiProviderCatalogRow {
    pub id: Uuid,
    pub provider_kind: String,
    pub display_name: String,
    pub api_style: String,
    pub lifecycle_state: String,
    pub default_base_url: Option<String>,
    pub capability_flags_json: Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiModelCatalogRow {
    pub id: Uuid,
    pub provider_catalog_id: Uuid,
    pub model_name: String,
    pub capability_kind: String,
    pub modality_kind: String,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub lifecycle_state: String,
    pub metadata_json: Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiPriceCatalogRow {
    pub id: Uuid,
    pub model_catalog_id: Uuid,
    pub billing_unit: String,
    pub unit_price: Decimal,
    pub currency_code: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub catalog_scope: String,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiProviderCredentialRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: String,
    pub credential_state: String,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiModelPresetRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub model_catalog_id: Uuid,
    pub preset_name: String,
    pub system_prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens_override: Option<i32>,
    pub extra_parameters_json: Value,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AiLibraryModelBindingRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub binding_purpose: String,
    pub provider_credential_id: Uuid,
    pub model_preset_id: Uuid,
    pub binding_state: String,
    pub updated_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

pub async fn list_provider_catalog(
    postgres: &PgPool,
) -> Result<Vec<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "select
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json
         from ai_provider_catalog
         order by provider_kind asc, id asc",
    )
    .fetch_all(postgres)
    .await
}

pub async fn list_model_catalog(
    postgres: &PgPool,
    provider_catalog_id: Option<Uuid>,
) -> Result<Vec<AiModelCatalogRow>, sqlx::Error> {
    match provider_catalog_id {
        Some(provider_catalog_id) => {
            sqlx::query_as::<_, AiModelCatalogRow>(
                "select
                    id,
                    provider_catalog_id,
                    model_name,
                    capability_kind::text as capability_kind,
                    modality_kind::text as modality_kind,
                    context_window,
                    max_output_tokens,
                    lifecycle_state::text as lifecycle_state,
                    metadata_json
                 from ai_model_catalog
                 where provider_catalog_id = $1
                 order by model_name asc, capability_kind asc, id asc",
            )
            .bind(provider_catalog_id)
            .fetch_all(postgres)
            .await
        }
        None => {
            sqlx::query_as::<_, AiModelCatalogRow>(
                "select
                    id,
                    provider_catalog_id,
                    model_name,
                    capability_kind::text as capability_kind,
                    modality_kind::text as modality_kind,
                    context_window,
                    max_output_tokens,
                    lifecycle_state::text as lifecycle_state,
                    metadata_json
                 from ai_model_catalog
                 order by model_name asc, capability_kind asc, id asc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn list_price_catalog(
    postgres: &PgPool,
    model_catalog_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
) -> Result<Vec<AiPriceCatalogRow>, sqlx::Error> {
    match (model_catalog_id, workspace_id) {
        (Some(model_catalog_id), Some(workspace_id)) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                   and (
                        (catalog_scope = 'workspace_override' and workspace_id = $2)
                        or (catalog_scope = 'system' and workspace_id is null)
                   )
                 order by catalog_scope asc, effective_from desc, id desc",
            )
            .bind(model_catalog_id)
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (Some(model_catalog_id), None) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc",
            )
            .bind(model_catalog_id)
            .fetch_all(postgres)
            .await
        }
        (None, Some(workspace_id)) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where workspace_id = $1 or workspace_id is null
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn get_price_catalog_by_id(
    postgres: &PgPool,
    price_id: Uuid,
) -> Result<Option<AiPriceCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiPriceCatalogRow>(
        "select
            id,
            model_catalog_id,
            billing_unit::text as billing_unit,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope::text as catalog_scope,
            workspace_id
         from ai_price_catalog
         where id = $1",
    )
    .bind(price_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_provider_catalog_by_kind(
    postgres: &PgPool,
    provider_kind: &str,
) -> Result<Option<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "select
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json
         from ai_provider_catalog
         where provider_kind = $1
           and lifecycle_state = 'active'",
    )
    .bind(provider_kind)
    .fetch_optional(postgres)
    .await
}

pub async fn get_model_catalog_by_provider_and_name(
    postgres: &PgPool,
    provider_kind: &str,
    model_name: &str,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(
        "select
            amc.id,
            amc.provider_catalog_id,
            amc.model_name,
            amc.capability_kind::text as capability_kind,
            amc.modality_kind::text as modality_kind,
            amc.context_window,
            amc.max_output_tokens,
            amc.lifecycle_state::text as lifecycle_state,
            amc.metadata_json
         from ai_model_catalog amc
         join ai_provider_catalog apc on apc.id = amc.provider_catalog_id
         where apc.provider_kind = $1
           and apc.lifecycle_state = 'active'
           and amc.model_name = $2
           and amc.lifecycle_state = 'active'
         order by
            amc.capability_kind asc,
            amc.id asc
         limit 1",
    )
    .bind(provider_kind)
    .bind(model_name)
    .fetch_optional(postgres)
    .await
}

pub async fn get_effective_price_catalog_entry(
    postgres: &PgPool,
    model_catalog_id: Uuid,
    billing_unit: &str,
    workspace_id: Option<Uuid>,
    effective_at: DateTime<Utc>,
) -> Result<Option<AiPriceCatalogRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                   and billing_unit = $2::billing_unit
                   and effective_from <= $3
                   and (effective_to is null or effective_to > $3)
                   and (
                        (catalog_scope = 'workspace_override' and workspace_id = $4)
                        or (catalog_scope = 'system' and workspace_id is null)
                   )
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc
                 limit 1",
            )
            .bind(model_catalog_id)
            .bind(billing_unit)
            .bind(effective_at)
            .bind(workspace_id)
            .fetch_optional(postgres)
            .await
        }
        None => {
            sqlx::query_as::<_, AiPriceCatalogRow>(
                "select
                    id,
                    model_catalog_id,
                    billing_unit::text as billing_unit,
                    unit_price,
                    currency_code,
                    effective_from,
                    effective_to,
                    catalog_scope::text as catalog_scope,
                    workspace_id
                 from ai_price_catalog
                 where model_catalog_id = $1
                   and billing_unit = $2::billing_unit
                   and effective_from <= $3
                   and (effective_to is null or effective_to > $3)
                 order by
                    case catalog_scope
                        when 'workspace_override' then 0
                        else 1
                    end,
                    effective_from desc,
                    id desc
                 limit 1",
            )
            .bind(model_catalog_id)
            .bind(billing_unit)
            .bind(effective_at)
            .fetch_optional(postgres)
            .await
        }
    }
}

pub async fn list_provider_credentials(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<AiProviderCredentialRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
        "select
            id,
            workspace_id,
            provider_catalog_id,
            label,
            api_key,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_provider_credential
         where workspace_id = $1
         order by created_at desc",
    )
    .bind(workspace_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_provider_credential_by_id(
    postgres: &PgPool,
    credential_id: Uuid,
) -> Result<Option<AiProviderCredentialRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
        "select
            id,
            workspace_id,
            provider_catalog_id,
            label,
            api_key,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_provider_credential
         where id = $1",
    )
    .bind(credential_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_provider_credential(
    postgres: &PgPool,
    workspace_id: Uuid,
    provider_catalog_id: Uuid,
    label: &str,
    api_key: &str,
    created_by_principal_id: Option<Uuid>,
) -> Result<AiProviderCredentialRow, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
        "insert into ai_provider_credential (
            id,
            workspace_id,
            provider_catalog_id,
            label,
            api_key,
            credential_state,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, 'active', $6, now(), now())
        returning
            id,
            workspace_id,
            provider_catalog_id,
            label,
            api_key,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(provider_catalog_id)
    .bind(label)
    .bind(api_key)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_provider_credential(
    postgres: &PgPool,
    credential_id: Uuid,
    label: &str,
    api_key: Option<&str>,
    credential_state: &str,
) -> Result<Option<AiProviderCredentialRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
        "update ai_provider_credential
         set label = $2,
             api_key = coalesce($3, api_key),
             credential_state = $4::ai_credential_state,
             updated_at = now()
         where id = $1
         returning
            id,
            workspace_id,
            provider_catalog_id,
            label,
            api_key,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(credential_id)
    .bind(label)
    .bind(api_key)
    .bind(credential_state)
    .fetch_optional(postgres)
    .await
}

pub async fn create_workspace_price_override(
    postgres: &PgPool,
    workspace_id: Uuid,
    model_catalog_id: Uuid,
    billing_unit: &str,
    unit_price: Decimal,
    currency_code: &str,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
) -> Result<AiPriceCatalogRow, sqlx::Error> {
    sqlx::query_as::<_, AiPriceCatalogRow>(
        "insert into ai_price_catalog (
            id,
            model_catalog_id,
            billing_unit,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope,
            workspace_id
        )
        values ($1, $2, $3::billing_unit, $4, $5, $6, $7, 'workspace_override', $8)
        returning
            id,
            model_catalog_id,
            billing_unit::text as billing_unit,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope::text as catalog_scope,
            workspace_id",
    )
    .bind(Uuid::now_v7())
    .bind(model_catalog_id)
    .bind(billing_unit)
    .bind(unit_price)
    .bind(currency_code)
    .bind(effective_from)
    .bind(effective_to)
    .bind(workspace_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_workspace_price_override(
    postgres: &PgPool,
    price_id: Uuid,
    model_catalog_id: Uuid,
    billing_unit: &str,
    unit_price: Decimal,
    currency_code: &str,
    effective_from: DateTime<Utc>,
    effective_to: Option<DateTime<Utc>>,
) -> Result<Option<AiPriceCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiPriceCatalogRow>(
        "update ai_price_catalog
         set model_catalog_id = $2,
             billing_unit = $3::billing_unit,
             unit_price = $4,
             currency_code = $5,
             effective_from = $6,
             effective_to = $7
         where id = $1
           and catalog_scope = 'workspace_override'
         returning
            id,
            model_catalog_id,
            billing_unit::text as billing_unit,
            unit_price,
            currency_code,
            effective_from,
            effective_to,
            catalog_scope::text as catalog_scope,
            workspace_id",
    )
    .bind(price_id)
    .bind(model_catalog_id)
    .bind(billing_unit)
    .bind(unit_price)
    .bind(currency_code)
    .bind(effective_from)
    .bind(effective_to)
    .fetch_optional(postgres)
    .await
}

pub async fn list_model_presets(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<AiModelPresetRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelPresetRow>(
        "select
            id,
            workspace_id,
            model_catalog_id,
            preset_name,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_model_preset
         where workspace_id = $1
         order by created_at desc",
    )
    .bind(workspace_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_model_preset_by_id(
    postgres: &PgPool,
    preset_id: Uuid,
) -> Result<Option<AiModelPresetRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelPresetRow>(
        "select
            id,
            workspace_id,
            model_catalog_id,
            preset_name,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_model_preset
         where id = $1",
    )
    .bind(preset_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_model_preset(
    postgres: &PgPool,
    workspace_id: Uuid,
    model_catalog_id: Uuid,
    preset_name: &str,
    system_prompt: Option<&str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
    extra_parameters_json: Value,
    created_by_principal_id: Option<Uuid>,
) -> Result<AiModelPresetRow, sqlx::Error> {
    sqlx::query_as::<_, AiModelPresetRow>(
        "insert into ai_model_preset (
            id,
            workspace_id,
            model_catalog_id,
            preset_name,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now(), now())
        returning
            id,
            workspace_id,
            model_catalog_id,
            preset_name,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            created_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(model_catalog_id)
    .bind(preset_name)
    .bind(system_prompt)
    .bind(temperature)
    .bind(top_p)
    .bind(max_output_tokens_override)
    .bind(extra_parameters_json)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_model_preset(
    postgres: &PgPool,
    preset_id: Uuid,
    preset_name: &str,
    system_prompt: Option<&str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    max_output_tokens_override: Option<i32>,
    extra_parameters_json: Value,
) -> Result<Option<AiModelPresetRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelPresetRow>(
        "update ai_model_preset
         set preset_name = $2,
             system_prompt = $3,
             temperature = $4,
             top_p = $5,
             max_output_tokens_override = $6,
             extra_parameters_json = $7,
             updated_at = now()
         where id = $1
         returning
            id,
            workspace_id,
            model_catalog_id,
            preset_name,
            system_prompt,
            temperature,
            top_p,
            max_output_tokens_override,
            extra_parameters_json,
            created_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(preset_id)
    .bind(preset_name)
    .bind(system_prompt)
    .bind(temperature)
    .bind(top_p)
    .bind(max_output_tokens_override)
    .bind(extra_parameters_json)
    .fetch_optional(postgres)
    .await
}

pub async fn list_library_bindings(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<AiLibraryModelBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiLibraryModelBindingRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
         from ai_library_model_binding
         where library_id = $1
         order by created_at desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_library_binding_by_id(
    postgres: &PgPool,
    binding_id: Uuid,
) -> Result<Option<AiLibraryModelBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiLibraryModelBindingRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
         from ai_library_model_binding
         where id = $1",
    )
    .bind(binding_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_active_library_binding_by_purpose(
    postgres: &PgPool,
    library_id: Uuid,
    binding_purpose: &str,
) -> Result<Option<AiLibraryModelBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiLibraryModelBindingRow>(
        "select
            id,
            workspace_id,
            library_id,
            binding_purpose::text as binding_purpose,
            provider_credential_id,
            model_preset_id,
            binding_state::text as binding_state,
            updated_by_principal_id,
            created_at,
            updated_at
         from ai_library_model_binding
         where library_id = $1
           and binding_purpose = $2::ai_binding_purpose
           and binding_state = 'active'
         order by updated_at desc, id desc
         limit 1",
    )
    .bind(library_id)
    .bind(binding_purpose)
    .fetch_optional(postgres)
    .await
}

pub async fn create_library_binding(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    binding_purpose: &str,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    updated_by_principal_id: Option<Uuid>,
) -> Result<AiLibraryModelBindingRow, sqlx::Error> {
    sqlx::query_as::<_, AiLibraryModelBindingRow>(
        "insert into ai_library_model_binding (
            id,
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
        values ($1, $2, $3, $4::ai_binding_purpose, $5, $6, 'active', $7, now(), now())
        returning
            id,
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
    .bind(workspace_id)
    .bind(library_id)
    .bind(binding_purpose)
    .bind(provider_credential_id)
    .bind(model_preset_id)
    .bind(updated_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_library_binding(
    postgres: &PgPool,
    binding_id: Uuid,
    provider_credential_id: Uuid,
    model_preset_id: Uuid,
    binding_state: &str,
    updated_by_principal_id: Option<Uuid>,
) -> Result<Option<AiLibraryModelBindingRow>, sqlx::Error> {
    sqlx::query_as::<_, AiLibraryModelBindingRow>(
        "update ai_library_model_binding
         set provider_credential_id = $2,
             model_preset_id = $3,
             binding_state = $4::ai_binding_state,
             updated_by_principal_id = $5,
             updated_at = now()
         where id = $1
         returning
            id,
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
