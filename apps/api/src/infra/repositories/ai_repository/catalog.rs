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
         on conflict (provider_catalog_id, model_name, capability_kind) do update
         set
            modality_kind = excluded.modality_kind,
            lifecycle_state = 'active'::ai_model_lifecycle_state,
            metadata_json = excluded.metadata_json
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
