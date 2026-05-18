//! OpenAPI document construction (utoipa code-first).
//!
//! Single source of truth for the IronRAG HTTP contract. Sub-sprint 1a
//! scaffolds the [`ApiDoc`] struct with shared metadata and security schemes.
//! Paths and components are registered incrementally in sub-sprints
//! 1b (DTOs via `#[derive(ToSchema)]`) and 1c (handlers via
//! `#[utoipa::path(...)]` + `OpenApiRouter::routes`).
//!
//! Once 1c is complete, [`ApiDoc::openapi`] yields the same surface that
//! today's hand-maintained `apps/api/contracts/ironrag.openapi.yaml` carries,
//! and sub-sprint 1d emits it to `apps/api/contracts/openapi.gen.yaml`.

use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

const API_TITLE: &str = "IronRAG API";
const API_VERSION: &str = env!("CARGO_PKG_VERSION");
const API_DESCRIPTION: &str = concat!(
    "Public HTTP API for the ArangoDB-backed IronRAG service ",
    "(`apps/api/src/interfaces/http`). Operation paths include the `/v1` ",
    "prefix. `servers.url` is the API origin without the `/v1` suffix.",
);

#[derive(OpenApi)]
#[openapi(
    info(
        title = API_TITLE,
        version = API_VERSION,
        description = API_DESCRIPTION,
    ),
    servers(
        (url = "/", description = "Same origin (paths include /v1)"),
    ),
    components(schemas(
        crate::interfaces::http::content::types::DocumentListSortKey,
        crate::interfaces::http::content::types::DocumentListSortOrder,
    )),
    modifiers(&SecurityAddon),
    security(("bearerAuth" = [])),
    tags(
        (name = "system"),
        (name = "catalog"),
        (name = "iam"),
        (name = "ai"),
        (name = "knowledge"),
        (name = "ingest"),
        (name = "search"),
        (name = "query"),
        (name = "runtime"),
        (name = "billing"),
        (name = "ops"),
        (name = "audit"),
        (name = "automation"),
        (name = "admin"),
        (name = "content"),
        (name = "webhooks"),
    ),
    paths(
        crate::interfaces::http::health::health,
        crate::interfaces::http::health::readiness,
        crate::interfaces::http::health::version,
        crate::interfaces::http::health::release_update,
        crate::interfaces::http::admin::get_admin_surface,
        crate::interfaces::http::audit::list_audit_events,
        crate::interfaces::http::ops::get_async_operation,
        crate::interfaces::http::ops::get_library_state,
        crate::interfaces::http::ops::list_ingest_queue,
        crate::interfaces::http::ops::move_ingest_queue_job,
        crate::interfaces::http::ops::pause_ingest_queue_job,
        crate::interfaces::http::ops::resume_ingest_queue_job,
        crate::interfaces::http::ops::cancel_ingest_queue_job,
        crate::interfaces::http::runtime::get_runtime_execution,
        crate::interfaces::http::runtime::get_runtime_execution_trace,
        crate::interfaces::http::ingestion::list_jobs,
        crate::interfaces::http::ingestion::get_job,
        crate::interfaces::http::ingestion::get_attempt,
        crate::interfaces::http::ingestion::list_stage_events,
        crate::interfaces::http::billing::list_provider_calls,
        crate::interfaces::http::billing::list_charges,
        crate::interfaces::http::billing::get_execution_cost,
        crate::interfaces::http::billing::get_library_cost_summary,
        crate::interfaces::http::billing::get_workspace_cost_summary,
        crate::interfaces::http::mcp::get_answer_capabilities,
        crate::interfaces::http::mcp::handle_answer_jsonrpc,
        crate::interfaces::http::mcp::get_diagnostics_capabilities,
        crate::interfaces::http::mcp::handle_diagnostics_jsonrpc,
        crate::interfaces::http::query::list_sessions,
        crate::interfaces::http::query::create_session,
        crate::interfaces::http::query::get_session,
        crate::interfaces::http::query::create_session_turn,
        crate::interfaces::http::query::get_execution,
        crate::interfaces::http::catalog::list_workspaces,
        crate::interfaces::http::catalog::get_workspace,
        crate::interfaces::http::catalog::create_workspace,
        crate::interfaces::http::catalog::delete_workspace,
        crate::interfaces::http::catalog::list_libraries,
        crate::interfaces::http::catalog::create_library,
        crate::interfaces::http::catalog::delete_library,
        crate::interfaces::http::catalog::get_library,
        crate::interfaces::http::catalog::update_library_web_ingest_policy,
        crate::interfaces::http::ai::list_providers,
        crate::interfaces::http::ai::list_models,
        crate::interfaces::http::ai::list_prices,
        crate::interfaces::http::ai::list_model_presets,
        crate::interfaces::http::ai::list_credentials,
        crate::interfaces::http::ai::create_credential,
        crate::interfaces::http::ai::list_binding_assignments,
        crate::interfaces::http::ai::create_binding_assignment,
        crate::interfaces::http::ai::update_binding_assignment,
        crate::interfaces::http::ai::validate_binding_assignment,
        crate::interfaces::http::iam::session::get_bootstrap_status,
        crate::interfaces::http::iam::session::setup_bootstrap_admin,
        crate::interfaces::http::iam::session::login_session,
        crate::interfaces::http::iam::session::get_session,
        crate::interfaces::http::iam::session::logout_session,
        crate::interfaces::http::iam::get_me,
        crate::interfaces::http::iam::list_tokens,
        crate::interfaces::http::iam::mint_token,
        crate::interfaces::http::iam::delete_token,
        crate::interfaces::http::iam::revoke_token,
        crate::interfaces::http::iam::list_grants,
        crate::interfaces::http::iam::create_grant,
        crate::interfaces::http::iam::revoke_grant,
        crate::interfaces::http::knowledge::library::list_context_bundles,
        crate::interfaces::http::knowledge::library::list_documents,
        crate::interfaces::http::knowledge::library::get_library_summary,
        crate::interfaces::http::knowledge::library::get_document,
        crate::interfaces::http::knowledge::library::get_context_bundle,
        crate::interfaces::http::knowledge::library::list_library_generations,
        crate::interfaces::http::knowledge::get_graph,
        crate::interfaces::http::knowledge::get_entity,
        crate::interfaces::http::knowledge::get_relation,
        crate::interfaces::http::knowledge::search::search_documents,
        crate::interfaces::http::knowledge::search::search_documents_by_library_query,
        crate::interfaces::http::content::list_documents,
        crate::interfaces::http::content::list_chunks,
        crate::interfaces::http::content::create_document,
        crate::interfaces::http::content::upload_document,
        crate::interfaces::http::content::get_document,
        crate::interfaces::http::content::patch_document_metadata,
        crate::interfaces::http::content::get_document_prepared_segments,
        crate::interfaces::http::content::get_document_technical_facts,
        crate::interfaces::http::content::delete_document,
        crate::interfaces::http::content::append_document,
        crate::interfaces::http::content::edit_document,
        crate::interfaces::http::content::replace_document,
        crate::interfaces::http::content::list_revisions,
        crate::interfaces::http::content::create_mutation,
        crate::interfaces::http::content::list_mutations,
        crate::interfaces::http::content::get_mutation,
        crate::interfaces::http::content::reprocess_document,
        crate::interfaces::http::content::batch::batch_delete_documents,
        crate::interfaces::http::content::batch::batch_cancel_documents,
        crate::interfaces::http::content::batch::batch_reprocess_documents,
        crate::interfaces::http::content::web_runs::create_web_ingest_run,
        crate::interfaces::http::content::web_runs::list_web_ingest_runs,
        crate::interfaces::http::content::web_runs::get_web_ingest_run,
        crate::interfaces::http::content::web_runs::list_web_ingest_run_pages,
        crate::interfaces::http::content::web_runs::cancel_web_ingest_run,
        crate::interfaces::http::content::source_download::download_document_source,
        crate::interfaces::http::content::get_document_head,
        crate::interfaces::http::content::snapshot::export_library_snapshot,
        crate::interfaces::http::content::snapshot::import_library_snapshot,
        crate::interfaces::http::ai::update_credential,
        crate::interfaces::http::ai::create_model_preset,
        crate::interfaces::http::ai::update_model_preset,
        crate::interfaces::http::ai::create_price_override,
        crate::interfaces::http::ai::update_price_override,
        crate::interfaces::http::ai::delete_binding_assignment,
        crate::interfaces::http::catalog::update_library,
        crate::interfaces::http::catalog::update_library_recognition_policy,
        crate::interfaces::http::billing::list_library_document_costs,
        crate::interfaces::http::ops::get_library_dashboard,
        crate::interfaces::http::iam::session::resolve_session,
        crate::interfaces::http::query::get_assistant_system_prompt,
        crate::interfaces::http::query::get_execution_llm_context,
        crate::interfaces::http::openapi::get_openapi_spec,
        crate::interfaces::http::webhook::create_subscription,
        crate::interfaces::http::webhook::list_subscriptions,
        crate::interfaces::http::webhook::get_subscription,
        crate::interfaces::http::webhook::update_subscription,
        crate::interfaces::http::webhook::delete_subscription,
        crate::interfaces::http::webhook::list_delivery_attempts,
    ),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components =
            openapi.components.get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new().scheme(HttpAuthScheme::Bearer).bearer_format("JWT").build(),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::ApiDoc;
    use utoipa::OpenApi;

    #[test]
    fn openapi_skeleton_is_valid_3_1() {
        let doc = ApiDoc::openapi();
        assert_eq!(doc.info.title, super::API_TITLE);
        assert_eq!(doc.info.version, super::API_VERSION);
        assert!(
            doc.components.as_ref().is_some_and(|c| c.security_schemes.contains_key("bearerAuth")),
            "bearerAuth security scheme must be registered",
        );
        let yaml = doc.to_yaml().expect("OpenAPI document must serialize to yaml");
        assert!(yaml.starts_with("openapi:"));
    }

    #[test]
    fn registered_paths_match_sprint_1c_progress() {
        let doc = ApiDoc::openapi();
        let paths = &doc.paths.paths;
        for expected in [
            // system
            "/v1/health",
            "/v1/ready",
            "/v1/version",
            "/v1/version/update",
            // admin
            "/v1/admin/surface",
            // audit
            "/v1/audit/events",
        ] {
            assert!(
                paths.contains_key(expected),
                "missing path {expected}; have: {:?}",
                paths.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn registered_responses_emit_referenced_schemas() {
        let doc = ApiDoc::openapi();
        let schemas = &doc.components.as_ref().expect("components present").schemas;
        for expected in [
            "HealthResponse",
            "VersionResponse",
            "ReleaseUpdateResponse",
            "DeploymentReadinessSnapshot",
            "AdminSurface",
            "AuditEventPageResponse",
        ] {
            assert!(
                schemas.contains_key(expected),
                "schema {expected} must be exported via response refs",
            );
        }
    }
}
