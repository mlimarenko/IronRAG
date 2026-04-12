use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiModelPresetRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
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

pub async fn list_model_presets_exact(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiModelPresetRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelPresetRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
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

pub async fn list_visible_model_presets(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiModelPresetRow>, sqlx::Error> {
    match (workspace_id, library_id) {
        (Some(workspace_id), Some(library_id)) => {
            sqlx::query_as::<_, AiModelPresetRow>(
                "select
                    id,
                    scope_kind::text as scope_kind,
                    workspace_id,
                    library_id,
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
                 where scope_kind = 'instance'
                    or (scope_kind = 'workspace' and workspace_id = $1)
                    or (scope_kind = 'library' and library_id = $2)
                 order by created_at desc, id desc",
            )
            .bind(workspace_id)
            .bind(library_id)
            .fetch_all(postgres)
            .await
        }
        (Some(workspace_id), None) => {
            sqlx::query_as::<_, AiModelPresetRow>(
                "select
                    id,
                    scope_kind::text as scope_kind,
                    workspace_id,
                    library_id,
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
                 where scope_kind = 'instance'
                    or (scope_kind = 'workspace' and workspace_id = $1)
                 order by created_at desc, id desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, AiModelPresetRow>(
                "select
                    id,
                    scope_kind::text as scope_kind,
                    workspace_id,
                    library_id,
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
                 where scope_kind = 'instance'
                 order by created_at desc, id desc",
            )
            .fetch_all(postgres)
            .await
        }
        (None, Some(library_id)) => {
            sqlx::query_as::<_, AiModelPresetRow>(
                "select
                    preset.id,
                    preset.scope_kind::text as scope_kind,
                    preset.workspace_id,
                    preset.library_id,
                    preset.model_catalog_id,
                    preset.preset_name,
                    preset.system_prompt,
                    preset.temperature,
                    preset.top_p,
                    preset.max_output_tokens_override,
                    preset.extra_parameters_json,
                    preset.created_by_principal_id,
                    preset.created_at,
                    preset.updated_at
                 from ai_model_preset preset
                 join catalog_library library on library.id = $1
                 where preset.scope_kind = 'instance'
                    or (preset.scope_kind = 'workspace' and preset.workspace_id = library.workspace_id)
                    or (preset.scope_kind = 'library' and preset.library_id = library.id)
                 order by preset.created_at desc, preset.id desc",
            )
            .bind(library_id)
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn get_model_preset_by_id(
    postgres: &PgPool,
    preset_id: Uuid,
) -> Result<Option<AiModelPresetRow>, sqlx::Error> {
    sqlx::query_as::<_, AiModelPresetRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
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
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
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
            scope_kind,
            workspace_id,
            library_id,
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
        values ($1, $2::ai_scope_kind, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now(), now())
        returning
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
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
    .bind(scope_kind)
    .bind(workspace_id)
    .bind(library_id)
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
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
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
