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

use utoipa::openapi::path::{Operation, PathItem};
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
    modifiers(&ContractDocAddon),
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
        crate::interfaces::http::ai::create_provider,
        crate::interfaces::http::ai::update_provider,
        crate::interfaces::http::ai::delete_provider,
        crate::interfaces::http::ai::list_models,
        crate::interfaces::http::ai::create_model,
        crate::interfaces::http::ai::update_model,
        crate::interfaces::http::ai::delete_model,
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
        crate::interfaces::http::iam::list_users,
        crate::interfaces::http::iam::create_user,
        crate::interfaces::http::iam::set_user_role,
        crate::interfaces::http::iam::get_user_access,
        crate::interfaces::http::iam::set_user_access,
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
        crate::interfaces::http::content::snapshot::export_workspace_snapshot,
        crate::interfaces::http::content::snapshot::import_workspace_snapshot,
        crate::interfaces::http::ai::update_credential,
        crate::interfaces::http::ai::delete_credential,
        crate::interfaces::http::ai::create_model_preset,
        crate::interfaces::http::ai::update_model_preset,
        crate::interfaces::http::ai::delete_model_preset,
        crate::interfaces::http::ai::create_price_override,
        crate::interfaces::http::ai::update_price_override,
        crate::interfaces::http::ai::delete_price_override,
        crate::interfaces::http::ai::delete_binding_assignment,
        crate::interfaces::http::catalog::update_library,
        crate::interfaces::http::catalog::update_library_recognition_policy,
        crate::interfaces::http::catalog::get_library_retrieval_config,
        crate::interfaces::http::catalog::update_library_retrieval_config,
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

struct ContractDocAddon;

impl Modify for ContractDocAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components =
            openapi.components.get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new().scheme(HttpAuthScheme::Bearer).bearer_format("JWT").build(),
            ),
        );
        describe_operations(openapi);
    }
}

fn describe_operations(openapi: &mut utoipa::openapi::OpenApi) {
    for (path, item) in &mut openapi.paths.paths {
        for (method, operation) in operations_mut(item) {
            if operation.summary.as_ref().is_none_or(|summary| summary.trim().is_empty()) {
                operation.summary = Some(operation_summary(operation));
            }
            if operation
                .description
                .as_ref()
                .is_none_or(|description| description.trim().is_empty())
            {
                operation.description = Some(operation_description(path, method, operation));
            }
        }
    }
}

fn operations_mut(item: &mut PathItem) -> Vec<(&'static str, &mut Operation)> {
    let mut operations = Vec::new();
    if let Some(operation) = item.get.as_mut() {
        operations.push(("GET", operation));
    }
    if let Some(operation) = item.put.as_mut() {
        operations.push(("PUT", operation));
    }
    if let Some(operation) = item.post.as_mut() {
        operations.push(("POST", operation));
    }
    if let Some(operation) = item.patch.as_mut() {
        operations.push(("PATCH", operation));
    }
    if let Some(operation) = item.delete.as_mut() {
        operations.push(("DELETE", operation));
    }
    if let Some(operation) = item.options.as_mut() {
        operations.push(("OPTIONS", operation));
    }
    if let Some(operation) = item.head.as_mut() {
        operations.push(("HEAD", operation));
    }
    if let Some(operation) = item.trace.as_mut() {
        operations.push(("TRACE", operation));
    }
    operations
}

fn operation_summary(operation: &Operation) -> String {
    let operation_id = operation.operation_id.as_deref().unwrap_or("apiOperation");
    match operation_id {
        "getHealth" => "Check backend liveness.".to_string(),
        "getReadiness" => "Check backend readiness.".to_string(),
        "getVersion" => "Get running service version.".to_string(),
        "getReleaseUpdate" => "Check for an available release update.".to_string(),
        "getOpenApiContract" => "Download the OpenAPI contract.".to_string(),
        _ => humanize_operation_id(operation_id),
    }
}

fn humanize_operation_id(operation_id: &str) -> String {
    let mut words = Vec::new();
    let mut current = String::new();
    for char in operation_id.chars() {
        if char.is_uppercase() && !current.is_empty() {
            words.push(current);
            current = String::new();
        }
        current.push(char.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut summary = words.join(" ");
    if let Some(first) = summary.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    summary.push('.');
    summary
}

fn operation_description(path: &str, method: &str, operation: &Operation) -> String {
    let operation_id = operation.operation_id.as_deref().unwrap_or("apiOperation");
    let tag = operation.tags.as_ref().and_then(|tags| tags.first()).map(String::as_str);
    let purpose = operation_purpose(operation_id, tag);
    let usage = operation_usage(path, method, operation_id, tag);
    format!("{purpose} {usage}")
}

fn operation_purpose(operation_id: &str, tag: Option<&str>) -> &'static str {
    if operation_id.contains("AiCredential") || operation_id.contains("AiCredentials") {
        return "Manages stored AI provider credentials used by runtime bindings. Use these endpoints from the admin UI or automation when registering, rotating, or listing provider secrets without exposing secret material in responses.";
    }
    if operation_id.contains("AiLibraryBinding") || operation_id.contains("Binding") {
        return "Manages library-level AI runtime bindings. Bindings decide which configured provider/model handles embedding, query answering, graph extraction, and other AI purposes for one library.";
    }
    if operation_id.contains("AiModelPreset") {
        return "Manages reusable AI model presets. Presets group provider, model, capability, and tuning metadata so operators can assign consistent runtime behavior to libraries.";
    }
    if operation_id.contains("AiPrice") {
        return "Manages AI price catalog overrides. Billing and cost dashboards use these rows to attribute provider calls and estimate execution cost.";
    }
    if operation_id.contains("AiModel") || operation_id.contains("AiProvider") {
        return "Reads the AI provider catalog used by the admin configuration screens. Operators use this metadata to choose providers, models, capabilities, and binding targets.";
    }
    if operation_id.contains("Billing")
        || operation_id.contains("Cost")
        || operation_id.contains("Charges")
    {
        return "Reads billing and cost attribution data collected from runtime executions. Use these endpoints for dashboards, audits, and per-execution/provider-call cost inspection.";
    }
    if operation_id.contains("CatalogWorkspace") || operation_id.contains("Workspace") {
        return "Manages catalog workspaces. Workspaces group libraries, IAM scope, billing summaries, and administrative ownership boundaries.";
    }
    if operation_id.contains("CatalogLibrary") || operation_id.contains("Library") {
        return "Manages catalog libraries and their policies. Libraries own documents, knowledge graph data, assistant sessions, ingest settings, and query readiness.";
    }
    if operation_id.contains("ContentWebIngest") || operation_id.contains("WebIngest") {
        return "Manages web-ingest runs. These endpoints submit seed URLs, inspect crawl/materialization results, list candidate pages, and cancel active web ingestion.";
    }
    if operation_id.contains("ContentMutation") || operation_id.contains("Mutation") {
        return "Manages document mutation receipts. Use these endpoints to create or inspect append, replace, delete, and other asynchronous document lifecycle operations.";
    }
    if operation_id.contains("ContentDocument")
        || operation_id.contains("Document")
        || operation_id.contains("Chunks")
    {
        return "Manages document content and derived document views. These endpoints are used by document workspaces, upload flows, source viewers, revision history, and document-level diagnostics.";
    }
    if operation_id.contains("IamToken") || operation_id.contains("Token") {
        return "Manages API tokens and token lifecycle. Operators use these endpoints to mint, list, revoke, or delete bearer tokens for users, services, and MCP clients.";
    }
    if operation_id.contains("IamGrant") || operation_id.contains("Grant") {
        return "Manages IAM grants. Grants assign scoped permissions to principals so UI users, API tokens, and automation can access only the intended workspaces and libraries.";
    }
    if operation_id.contains("IamSession")
        || operation_id.contains("Bootstrap")
        || operation_id.contains("login")
        || operation_id.contains("logout")
    {
        return "Manages browser authentication and bootstrap state. The web shell uses these endpoints for login, logout, session restore, first-admin setup, and access-label resolution.";
    }
    if operation_id.contains("Ingest")
        || operation_id.contains("Stage")
        || operation_id.contains("Job")
    {
        return "Reads or controls ingest runtime state. Operators use these endpoints to inspect queued work, attempts, stage events, and document-processing failures.";
    }
    if operation_id.contains("Knowledge")
        || operation_id.contains("Graph")
        || operation_id.contains("Entity")
        || operation_id.contains("Relation")
        || operation_id.contains("ContextBundle")
    {
        return "Reads the knowledge model derived from ingested documents. These endpoints power graph workbench views, document memory search, context-bundle inspection, and entity/relation drill-downs.";
    }
    if operation_id.contains("Runtime") {
        return "Reads runtime execution traces. Use these endpoints to inspect lifecycle state, stages, actions, policy decisions, failures, and child work for asynchronous operations.";
    }
    if operation_id.contains("Audit") {
        return "Reads immutable audit events. Security and operations teams use this endpoint to inspect who performed sensitive actions and which resources were affected.";
    }
    if operation_id.contains("Webhook") {
        return "Manages outbound webhook subscriptions and delivery attempts. External systems use these subscriptions to receive document and revision lifecycle notifications.";
    }
    if operation_id.contains("AdminSurface") {
        return "Returns the admin shell aggregate. The admin UI uses this endpoint to load configuration, readiness, IAM, and model-catalog state with fewer round trips.";
    }
    if operation_id.contains("OpenApi") {
        return "Returns the generated OpenAPI 3.1 contract served by the running backend. Swagger UI and API clients use it as the HTTP contract source.";
    }
    if matches!(tag, Some("system")) {
        return "Reads service health, readiness, version, or release metadata. Monitoring systems and the UI shell use these endpoints before calling heavier APIs.";
    }
    "Executes an IronRAG HTTP API operation. The endpoint is bearer-authenticated unless explicitly documented otherwise and returns JSON shaped by the OpenAPI schema."
}

fn operation_usage(
    path: &str,
    method: &str,
    operation_id: &str,
    tag: Option<&str>,
) -> &'static str {
    if method == "GET" && path.contains("/source") {
        return "Call it when the UI or an integration needs the original stored source file rather than extracted text or derived metadata.";
    }
    if method == "GET" && path.contains("/snapshot") {
        return "Call it to export a portable library archive for backup, migration, or offline inspection.";
    }
    if method == "POST" && path.contains("/snapshot") {
        return "Call it to import a previously exported library archive into the selected library scope.";
    }
    if method == "POST" && path.contains("/upload") {
        return "Call it for multipart or direct document uploads; ingestion continues asynchronously after the document record is accepted.";
    }
    if method == "POST" && path.contains("batch") {
        return "Call it from bulk-action UI flows; the response describes accepted items and any per-item admission failures.";
    }
    if method == "GET" && path.contains("/dashboard") {
        return "Call it to hydrate dashboards with one server-computed view instead of issuing many smaller requests.";
    }
    if path.contains("/ready") {
        return "Call it from load balancers, deploy checks, and UI startup gates; it reports whether dependencies and required bootstrap state are usable.";
    }
    if path.contains("/health") {
        return "Call it for cheap liveness checks; it does not prove that downstream stores or AI bindings are ready.";
    }
    if operation_id.starts_with("list") {
        return "Call it for paginated or filtered table views. Prefer server-side filters and cursors over fetching broad result sets into the client.";
    }
    if operation_id.starts_with("search") {
        return "Call it when the caller has a query and needs ranked candidates before reading or drilling into a specific resource.";
    }
    if operation_id.starts_with("get") {
        return "Call it when the caller already has the resource identifier and needs the latest authorized server view.";
    }
    if operation_id.starts_with("create") || operation_id.starts_with("post") || method == "POST" {
        return "Call it to create work or submit a command. Some commands complete synchronously, while ingest, mutation, and runtime work can continue asynchronously.";
    }
    if operation_id.starts_with("update")
        || operation_id.starts_with("patch")
        || method == "PUT"
        || method == "PATCH"
    {
        return "Call it to replace or partially update server-owned configuration. The request body is validated before changes are persisted.";
    }
    if operation_id.starts_with("delete")
        || operation_id.starts_with("revoke")
        || method == "DELETE"
    {
        return "Call it to remove or revoke a resource. Destructive operations are authorized, audited, and may return an asynchronous operation when cleanup continues in the background.";
    }
    if matches!(tag, Some("automation")) {
        return "Call it from agents or automation clients after checking the token's visible capability set.";
    }
    "Use the documented parameters and request body schema to call it from the web UI, automation, or service integrations."
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

    #[test]
    fn every_operation_has_swagger_summary_and_description() {
        let doc = ApiDoc::openapi();
        let mut missing = Vec::new();
        for (path, item) in &doc.paths.paths {
            let operations = [
                ("GET", item.get.as_ref()),
                ("PUT", item.put.as_ref()),
                ("POST", item.post.as_ref()),
                ("PATCH", item.patch.as_ref()),
                ("DELETE", item.delete.as_ref()),
                ("OPTIONS", item.options.as_ref()),
                ("HEAD", item.head.as_ref()),
                ("TRACE", item.trace.as_ref()),
            ];
            for (method, operation) in operations {
                let Some(operation) = operation else {
                    continue;
                };
                if operation.summary.as_ref().is_none_or(|value| value.trim().is_empty())
                    || operation.description.as_ref().is_none_or(|value| value.trim().is_empty())
                {
                    missing.push(format!(
                        "{} {} ({})",
                        method,
                        path,
                        operation.operation_id.as_deref().unwrap_or("missing operationId")
                    ));
                }
            }
        }
        assert!(missing.is_empty(), "OpenAPI operations missing Swagger docs: {missing:?}");
    }
}
