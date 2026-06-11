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

pub async fn get_provider_catalog_by_id(
    postgres: &PgPool,
    provider_catalog_id: Uuid,
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
         where id = $1",
    )
    .bind(provider_catalog_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_model_catalog_by_id(
    postgres: &PgPool,
    model_catalog_id: Uuid,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
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
         where id = $1",
    )
    .bind(model_catalog_id)
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

pub async fn create_provider_catalog(
    postgres: &PgPool,
    provider_kind: &str,
    display_name: &str,
    api_style: &str,
    lifecycle_state: &str,
    default_base_url: Option<&str>,
    capability_flags_json: Value,
) -> Result<AiProviderCatalogRow, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "insert into ai_provider_catalog (
            id,
            provider_kind,
            display_name,
            api_style,
            lifecycle_state,
            default_base_url,
            capability_flags_json
         )
         values (
            uuidv7(),
            $1,
            $2,
            $3::ai_provider_api_style,
            $4::ai_provider_lifecycle_state,
            $5,
            $6
         )
         returning
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json",
    )
    .bind(provider_kind)
    .bind(display_name)
    .bind(api_style)
    .bind(lifecycle_state)
    .bind(default_base_url)
    .bind(capability_flags_json)
    .fetch_one(postgres)
    .await
}

pub async fn update_provider_catalog(
    postgres: &PgPool,
    provider_id: Uuid,
    provider_kind: &str,
    display_name: &str,
    api_style: &str,
    lifecycle_state: &str,
    default_base_url: Option<&str>,
    capability_flags_json: Option<Value>,
) -> Result<Option<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "update ai_provider_catalog
         set provider_kind = $2,
             display_name = $3,
             api_style = $4::ai_provider_api_style,
             lifecycle_state = $5::ai_provider_lifecycle_state,
             default_base_url = $6,
             capability_flags_json = coalesce($7::jsonb, capability_flags_json)
         where id = $1
         returning
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json",
    )
    .bind(provider_id)
    .bind(provider_kind)
    .bind(display_name)
    .bind(api_style)
    .bind(lifecycle_state)
    .bind(default_base_url)
    .bind(capability_flags_json)
    .fetch_optional(postgres)
    .await
}

pub async fn disable_provider_catalog(
    postgres: &PgPool,
    provider_id: Uuid,
) -> Result<Option<AiProviderCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCatalogRow>(
        "update ai_provider_catalog
         set lifecycle_state = 'disabled'::ai_provider_lifecycle_state
         where id = $1
         returning
            id,
            provider_kind,
            display_name,
            api_style::text as api_style,
            lifecycle_state::text as lifecycle_state,
            default_base_url,
            capability_flags_json",
    )
    .bind(provider_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_model_catalog_by_provider_name_and_capability(
    postgres: &PgPool,
    provider_kind: &str,
    model_name: &str,
    capability_kind: &str,
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
           and amc.capability_kind = $3::ai_model_capability_kind
           and amc.lifecycle_state = 'active'
         order by
            amc.id asc
         limit 1",
    )
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability_kind)
    .fetch_optional(postgres)
    .await
}

pub async fn create_model_catalog(
    postgres: &PgPool,
    provider_catalog_id: Uuid,
    model_name: &str,
    capability_kind: &str,
    modality_kind: &str,
    context_window: Option<i32>,
    max_output_tokens: Option<i32>,
    lifecycle_state: &str,
    metadata_json: Value,
) -> Result<AiModelCatalogRow, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(
        "insert into ai_model_catalog (
            id,
            provider_catalog_id,
            model_name,
            capability_kind,
            modality_kind,
            context_window,
            max_output_tokens,
            lifecycle_state,
            metadata_json
         )
         values (
            uuidv7(),
            $1,
            $2,
            $3::ai_model_capability_kind,
            $4::ai_model_modality_kind,
            $5,
            $6,
            $7::ai_model_lifecycle_state,
            $8
         )
         returning
            id,
            provider_catalog_id,
            model_name,
            capability_kind::text as capability_kind,
            modality_kind::text as modality_kind,
            context_window,
            max_output_tokens,
            lifecycle_state::text as lifecycle_state,
            metadata_json",
    )
    .bind(provider_catalog_id)
    .bind(model_name)
    .bind(capability_kind)
    .bind(modality_kind)
    .bind(context_window)
    .bind(max_output_tokens)
    .bind(lifecycle_state)
    .bind(metadata_json)
    .fetch_one(postgres)
    .await
}

pub async fn update_model_catalog(
    postgres: &PgPool,
    model_id: Uuid,
    provider_catalog_id: Uuid,
    model_name: &str,
    capability_kind: &str,
    modality_kind: &str,
    context_window: Option<i32>,
    max_output_tokens: Option<i32>,
    lifecycle_state: &str,
    metadata_json: Value,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(
        "update ai_model_catalog
         set provider_catalog_id = $2,
             model_name = $3,
             capability_kind = $4::ai_model_capability_kind,
             modality_kind = $5::ai_model_modality_kind,
             context_window = $6,
             max_output_tokens = $7,
             lifecycle_state = $8::ai_model_lifecycle_state,
             metadata_json = $9
         where id = $1
         returning
            id,
            provider_catalog_id,
            model_name,
            capability_kind::text as capability_kind,
            modality_kind::text as modality_kind,
            context_window,
            max_output_tokens,
            lifecycle_state::text as lifecycle_state,
            metadata_json",
    )
    .bind(model_id)
    .bind(provider_catalog_id)
    .bind(model_name)
    .bind(capability_kind)
    .bind(modality_kind)
    .bind(context_window)
    .bind(max_output_tokens)
    .bind(lifecycle_state)
    .bind(metadata_json)
    .fetch_optional(postgres)
    .await
}

pub async fn disable_model_catalog(
    postgres: &PgPool,
    model_id: Uuid,
) -> Result<Option<AiModelCatalogRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(
        "update ai_model_catalog
         set lifecycle_state = 'disabled'::ai_model_lifecycle_state
         where id = $1
         returning
            id,
            provider_catalog_id,
            model_name,
            capability_kind::text as capability_kind,
            modality_kind::text as modality_kind,
            context_window,
            max_output_tokens,
            lifecycle_state::text as lifecycle_state,
            metadata_json",
    )
    .bind(model_id)
    .fetch_optional(postgres)
    .await
}

pub async fn upsert_model_catalog(
    postgres: &PgPool,
    provider_catalog_id: Uuid,
    model_name: &str,
    capability_kind: &str,
    modality_kind: &str,
    metadata_json: Value,
) -> Result<AiModelCatalogRow, sqlx::Error> {
    sqlx::query_as::<_, AiModelCatalogRow>(
        "insert into ai_model_catalog (
            id,
            provider_catalog_id,
            model_name,
            capability_kind,
            modality_kind,
            context_window,
            max_output_tokens,
            lifecycle_state,
            metadata_json
         )
         values (
            uuidv7(),
            $1,
            $2,
            $3::ai_model_capability_kind,
            $4::ai_model_modality_kind,
            null,
            null,
            'active'::ai_model_lifecycle_state,
            $5
         )
         -- Seed migration is the canonical source of `modality_kind` and
         -- `metadata_json.defaultRoles` (incl. `vision` role for multimodal
         -- chat models). Runtime provider-discovery cannot classify per
         -- model — its signature is keyed on capability_kind only — so we
         -- must NOT let it overwrite the seed. On conflict only refresh
         -- lifecycle so re-discovered models bounce back to `active`.
         on conflict (provider_catalog_id, model_name, capability_kind) do update
         set
            lifecycle_state = 'active'::ai_model_lifecycle_state
         returning
            id,
            provider_catalog_id,
            model_name,
            capability_kind::text as capability_kind,
            modality_kind::text as modality_kind,
            context_window,
            max_output_tokens,
            lifecycle_state::text as lifecycle_state,
            metadata_json",
    )
    .bind(provider_catalog_id)
    .bind(model_name)
    .bind(capability_kind)
    .bind(modality_kind)
    .bind(metadata_json)
    .fetch_one(postgres)
    .await
}
