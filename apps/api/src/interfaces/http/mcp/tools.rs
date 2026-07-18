use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    app::state::AppState,
    domains::agent_runtime::RuntimeSurfaceKind,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_DOCUMENTS_WRITE, POLICY_LIBRARY_READ, POLICY_LIBRARY_WRITE,
            POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ, POLICY_USAGE_READ,
            POLICY_WORKSPACE_ADMIN,
        },
        router_support::ApiError,
    },
};

use super::{
    McpJsonRpcResponse, McpToolCallParams, McpToolDescriptor, McpToolResult, McpToolSurface,
    audit::record_canonical_mcp_audit, mcp_api_error_response, success_response, tool_error_result,
};
use document_image::VIEW_DOCUMENT_IMAGE_TOOL_NAME;
use documents::{READ_DOCUMENT_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME};

pub(crate) mod catalog;
pub(crate) mod document_image;
pub(crate) mod documents;
pub(crate) mod graph;
pub(crate) mod grounded;
pub(crate) mod runtime;
pub(crate) mod web_ingest;

#[derive(Clone, Copy)]
pub(crate) struct ToolCallContext<'a> {
    pub auth: &'a AuthContext,
    pub state: &'a AppState,
    pub request_id: &'a str,
    pub surface_kind: RuntimeSurfaceKind,
}

/// Capability inputs the listing predicate cannot derive purely from
/// the auth grants. Right now the only one is `agent_vision_available`,
/// computed by the async caller via
/// [`document_image::any_agent_binding_supports_vision`]. Per-call
/// enforcement still happens inside the tool handler regardless of the
/// listing-time flag.
#[derive(Clone, Copy, Default)]
pub(crate) struct ToolVisibilityCapabilities {
    pub agent_vision_available: bool,
}

/// Version of the deterministic framing used by [`tool_contract_hash_from_components`].
/// Exposed alongside the hash so clients can reject an unsupported future
/// canonicalization rather than comparing unlike digest formats.
pub(crate) const MCP_TOOL_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub(crate) struct McpVisibleToolContract {
    pub descriptors: Vec<McpToolDescriptor>,
    pub hash: String,
}

#[derive(Debug, thiserror::Error)]
#[error("MCP tool contract is incomplete: descriptor for '{tool_name}' is missing")]
pub(crate) struct McpToolContractError {
    tool_name: String,
}

/// Resolves the caller-visible tool set into one fail-closed contract.
///
/// Both `tools/list` and the capabilities endpoint use this path, so neither
/// transport can silently omit a name whose descriptor was accidentally
/// removed. The UI loop maps the same visible names through `descriptor_for`;
/// its parity test compares the resulting component hash byte-for-byte.
pub(crate) fn visible_tool_contract_with_capabilities(
    auth: &AuthContext,
    surface: McpToolSurface,
    capabilities: ToolVisibilityCapabilities,
) -> Result<McpVisibleToolContract, McpToolContractError> {
    let names = visible_tool_names_with_capabilities(auth, surface, capabilities);
    tool_contract_for_names(&names)
}

fn tool_contract_for_names(
    names: &[String],
) -> Result<McpVisibleToolContract, McpToolContractError> {
    let descriptors = names
        .iter()
        .map(|name| {
            descriptor_for(name).ok_or_else(|| McpToolContractError { tool_name: name.clone() })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let hash = tool_contract_hash_from_components(
        descriptors
            .iter()
            .map(|descriptor| (descriptor.name, descriptor.description, &descriptor.input_schema)),
    );
    Ok(McpVisibleToolContract { descriptors, hash })
}

/// Hashes the exact LLM-facing contract: ordered name, description, and
/// canonical JSON input schema for every visible tool.
///
/// Length-prefixed frames avoid concatenation ambiguity. Recursive object-key
/// sorting keeps the digest stable if an equivalent schema was constructed in
/// a different insertion order, while array order remains significant.
pub(crate) fn tool_contract_hash_from_components<'a>(
    components: impl IntoIterator<Item = (&'a str, &'a str, &'a Value)>,
) -> String {
    let mut hasher = Sha256::new();
    update_tool_contract_hash_frame(&mut hasher, b"ironrag-mcp-tool-contract");
    update_tool_contract_hash_frame(&mut hasher, &MCP_TOOL_CONTRACT_VERSION.to_be_bytes());
    for (name, description, input_schema) in components {
        update_tool_contract_hash_frame(&mut hasher, name.as_bytes());
        update_tool_contract_hash_frame(&mut hasher, description.as_bytes());
        update_tool_contract_hash_json(&mut hasher, input_schema);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn update_tool_contract_hash_frame(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(tool_contract_length(value.len()).to_be_bytes());
    hasher.update(value);
}

fn tool_contract_length(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn update_tool_contract_hash_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => update_tool_contract_hash_frame(hasher, b"null"),
        Value::Bool(boolean) => {
            update_tool_contract_hash_frame(hasher, b"boolean");
            let encoded: &[u8] = if *boolean { b"true" } else { b"false" };
            update_tool_contract_hash_frame(hasher, encoded);
        }
        Value::Number(number) => {
            update_tool_contract_hash_frame(hasher, b"number");
            update_tool_contract_hash_frame(hasher, number.to_string().as_bytes());
        }
        Value::String(string) => {
            update_tool_contract_hash_frame(hasher, b"string");
            update_tool_contract_hash_frame(hasher, string.as_bytes());
        }
        Value::Array(items) => {
            update_tool_contract_hash_frame(hasher, b"array");
            update_tool_contract_hash_frame(
                hasher,
                &tool_contract_length(items.len()).to_be_bytes(),
            );
            for item in items {
                update_tool_contract_hash_json(hasher, item);
            }
        }
        Value::Object(object) => {
            update_tool_contract_hash_frame(hasher, b"object");
            update_tool_contract_hash_frame(
                hasher,
                &tool_contract_length(object.len()).to_be_bytes(),
            );
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                update_tool_contract_hash_frame(hasher, key.as_bytes());
                if let Some(item) = object.get(key) {
                    update_tool_contract_hash_json(hasher, item);
                }
            }
        }
    }
}

/// Which of the two physical MCP mounts (`/v1/mcp` answer surface,
/// `/v1/mcp/diagnostics`) expose one [`ToolRegistryEntry`]. `BOTH` covers
/// reads; mutating tools are `DIAGNOSTICS_ONLY` (§6.3 rule: "чтения видимы
/// на обеих поверхностях по умолчанию; всё, что мутирует состояние —
/// только diagnostics").
#[derive(Debug, Clone, Copy)]
struct ToolSurfaces {
    answer: bool,
    diagnostics: bool,
}

impl ToolSurfaces {
    const BOTH: Self = Self { answer: true, diagnostics: true };
    const DIAGNOSTICS_ONLY: Self = Self { answer: false, diagnostics: true };

    const fn includes(self, surface: McpToolSurface) -> bool {
        match surface {
            McpToolSurface::Answer => self.answer,
            McpToolSurface::Diagnostics => self.diagnostics,
        }
    }
}

/// One row per MCP tool: its name, which surface(s) expose it, and the
/// single grant predicate that decides whether one caller sees it.
///
/// This table is the single canonical tool registry (plan §6.4): both
/// `tools/list` filters ([`visible_tool_names_with_capabilities`]) and the
/// exhaustive per-surface name lists ([`canonical_tool_names`], which back
/// the public `MCP_ANSWER_TOOL_NAMES`/`MCP_DIAGNOSTICS_TOOL_NAMES`
/// capability-advertisement statics) derive from it. It replaces the old
/// `visible_answer_tool_names`/`visible_diagnostics_tool_names` pair, which
/// re-implemented the same per-tool grant predicate twice and had to be
/// kept in sync by hand — the exact "5 copies per tool name" drift finding
/// this registry closes.
struct ToolRegistryEntry {
    name: &'static str,
    surfaces: ToolSurfaces,
    visible: fn(&AuthContext, ToolVisibilityCapabilities) -> bool,
}

const fn always_visible(_auth: &AuthContext, _capabilities: ToolVisibilityCapabilities) -> bool {
    true
}

fn can_read_library_memory(auth: &AuthContext, _capabilities: ToolVisibilityCapabilities) -> bool {
    auth.can_read_any_library_memory(POLICY_MCP_MEMORY_READ)
}

fn can_read_web_runs(auth: &AuthContext, _capabilities: ToolVisibilityCapabilities) -> bool {
    auth.can_read_any_library_memory(POLICY_LIBRARY_READ)
}

fn can_read_runtime(auth: &AuthContext, _capabilities: ToolVisibilityCapabilities) -> bool {
    auth.can_read_any_document_memory(POLICY_RUNTIME_READ)
}

const TOOL_REGISTRY: &[ToolRegistryEntry] = &[
    ToolRegistryEntry {
        name: "list_workspaces",
        surfaces: ToolSurfaces::BOTH,
        visible: always_visible,
    },
    ToolRegistryEntry {
        name: "list_libraries",
        surfaces: ToolSurfaces::BOTH,
        visible: always_visible,
    },
    ToolRegistryEntry {
        name: "grounded_answer",
        surfaces: ToolSurfaces::BOTH,
        visible: |auth, _capabilities| auth.can_read_any_library_memory(POLICY_QUERY_RUN),
    },
    ToolRegistryEntry {
        name: SEARCH_DOCUMENTS_TOOL_NAME,
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_library_memory,
    },
    ToolRegistryEntry {
        name: READ_DOCUMENT_TOOL_NAME,
        surfaces: ToolSurfaces::BOTH,
        visible: |auth, _capabilities| auth.can_read_any_document_memory(POLICY_MCP_MEMORY_READ),
    },
    ToolRegistryEntry {
        name: VIEW_DOCUMENT_IMAGE_TOOL_NAME,
        surfaces: ToolSurfaces::BOTH,
        visible: |auth, capabilities| {
            auth.can_read_any_document_memory(POLICY_MCP_MEMORY_READ)
                && capabilities.agent_vision_available
        },
    },
    ToolRegistryEntry {
        name: "list_documents",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_library_memory,
    },
    ToolRegistryEntry {
        name: "search_entities",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_library_memory,
    },
    ToolRegistryEntry {
        name: "get_graph_topology",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_library_memory,
    },
    ToolRegistryEntry {
        name: "list_relations",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_library_memory,
    },
    ToolRegistryEntry {
        name: "get_communities",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_library_memory,
    },
    ToolRegistryEntry {
        name: "get_runtime_execution",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_runtime,
    },
    ToolRegistryEntry {
        name: "get_runtime_execution_trace",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_runtime,
    },
    ToolRegistryEntry {
        name: "get_web_run",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_web_runs,
    },
    ToolRegistryEntry {
        name: "list_web_run_pages",
        surfaces: ToolSurfaces::BOTH,
        visible: can_read_web_runs,
    },
    ToolRegistryEntry {
        name: "create_workspace",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.is_system_admin,
    },
    ToolRegistryEntry {
        name: "create_library",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_admin_any_workspace(POLICY_WORKSPACE_ADMIN),
    },
    ToolRegistryEntry {
        name: "update_workspace",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.is_system_admin,
    },
    ToolRegistryEntry {
        name: "update_library",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_admin_any_workspace(POLICY_WORKSPACE_ADMIN),
    },
    ToolRegistryEntry {
        name: "create_documents",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE),
    },
    ToolRegistryEntry {
        name: "create_document_revision",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE),
    },
    ToolRegistryEntry {
        name: "delete_document",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_write_any_document_memory(POLICY_DOCUMENTS_WRITE),
    },
    ToolRegistryEntry {
        name: "get_operation",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_read_any_document_memory(POLICY_USAGE_READ),
    },
    ToolRegistryEntry {
        name: "submit_web_run",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_write_any_library_memory(POLICY_LIBRARY_WRITE),
    },
    ToolRegistryEntry {
        name: "cancel_web_run",
        surfaces: ToolSurfaces::DIAGNOSTICS_ONLY,
        visible: |auth, _capabilities| auth.can_write_any_library_memory(POLICY_LIBRARY_WRITE),
    },
];

#[cfg(test)]
pub(crate) fn visible_tool_names(auth: &AuthContext, surface: McpToolSurface) -> Vec<String> {
    visible_tool_names_with_capabilities(auth, surface, ToolVisibilityCapabilities::default())
}

pub(crate) fn visible_tool_names_with_capabilities(
    auth: &AuthContext,
    surface: McpToolSurface,
    capabilities: ToolVisibilityCapabilities,
) -> Vec<String> {
    TOOL_REGISTRY
        .iter()
        .filter(|entry| entry.surfaces.includes(surface) && (entry.visible)(auth, capabilities))
        .map(|entry| entry.name.to_string())
        .collect()
}

/// Exhaustive tool names one surface *could* expose, independent of any
/// caller's grants. Backs the public `MCP_ANSWER_TOOL_NAMES`/
/// `MCP_DIAGNOSTICS_TOOL_NAMES` capability-advertisement statics in
/// `interfaces::http::mcp`.
pub(crate) fn canonical_tool_names(surface: McpToolSurface) -> Vec<&'static str> {
    TOOL_REGISTRY
        .iter()
        .filter(|entry| entry.surfaces.includes(surface))
        .map(|entry| entry.name)
        .collect()
}

pub(super) async fn handle_tools_list(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    let capabilities = ToolVisibilityCapabilities {
        agent_vision_available: document_image::any_agent_binding_supports_vision(auth, state)
            .await,
    };
    let contract = match visible_tool_contract_with_capabilities(auth, surface, capabilities) {
        Ok(contract) => contract,
        Err(error) => {
            record_canonical_mcp_audit(
                state,
                auth,
                request_id,
                "mcp.tools.list",
                "failed",
                Some("MCP tool contract preflight failed.".to_string()),
                Some(format!(
                    "principal {} could not list the MCP tool contract",
                    auth.principal_id
                )),
                Vec::new(),
            )
            .await;
            return mcp_api_error_response(
                id,
                ApiError::internal_with_log(error, "MCP tool contract preflight failed"),
            );
        }
    };

    record_canonical_mcp_audit(
        state,
        auth,
        request_id,
        "mcp.tools.list",
        "succeeded",
        Some("MCP tools list returned.".to_string()),
        Some(format!(
            "principal {} listed {} MCP tools",
            auth.principal_id,
            contract.descriptors.len()
        )),
        Vec::new(),
    )
    .await;

    success_response(
        id,
        json!({
            "tools": contract.descriptors,
            "_meta": {
                "ironrag/toolContractVersion": MCP_TOOL_CONTRACT_VERSION,
                "ironrag/toolContractHash": contract.hash,
            },
        }),
    )
}

pub(super) async fn handle_tools_call(
    auth: &AuthContext,
    state: &AppState,
    request_id: &str,
    id: Option<Value>,
    params: Option<Value>,
    surface: McpToolSurface,
) -> McpJsonRpcResponse {
    let params_value = params.unwrap_or_else(|| json!({}));
    let parsed: McpToolCallParams = match serde_json::from_value(params_value) {
        Ok(parsed) => parsed,
        Err(error) => {
            return success_response(
                id,
                json!(tool_error_result(ApiError::invalid_mcp_tool_call(format!(
                    "invalid tools/call params: {error}"
                )))),
            );
        }
    };
    // Ordinary calls do not need an O(number of visible libraries) vision
    // capability scan. Only the optional image tool is visibility-gated by
    // model modality; every other answer/diagnostics tool has the same
    // membership when the flag is false.
    let capabilities = if parsed.name == VIEW_DOCUMENT_IMAGE_TOOL_NAME {
        ToolVisibilityCapabilities {
            agent_vision_available: document_image::any_agent_binding_supports_vision(auth, state)
                .await,
        }
    } else {
        ToolVisibilityCapabilities::default()
    };
    let contract = match visible_tool_contract_with_capabilities(auth, surface, capabilities) {
        Ok(contract) => contract,
        Err(error) => {
            return mcp_api_error_response(
                id,
                ApiError::internal_with_log(error, "MCP tool call contract preflight failed"),
            );
        }
    };
    if !contract.descriptors.iter().any(|descriptor| descriptor.name == parsed.name) {
        return success_response(
            id,
            json!(tool_error_result(ApiError::invalid_mcp_tool_call(format!(
                "tool '{}' is not available on the {} MCP surface",
                parsed.name,
                surface.label()
            )))),
        );
    }

    let context =
        ToolCallContext { auth, state, request_id, surface_kind: RuntimeSurfaceKind::Mcp };
    let dispatch_started_at = std::time::Instant::now();
    let dispatch_result =
        Box::pin(call_named_tool(parsed.name.as_str(), context, &parsed.arguments)).await;
    let duration_ms = dispatch_started_at.elapsed().as_millis();
    let result = match dispatch_result {
        Some(result) => result,
        None => tool_error_result(ApiError::invalid_mcp_tool_call(format!(
            "unsupported MCP tool '{}'",
            parsed.name
        ))),
    };

    // Mandatory audit chokepoint: every `tools/call` dispatch is audited
    // here exactly once, regardless of whether the tool handler itself
    // additionally audits. This makes it structurally impossible for any
    // tool — current or future — to go unaudited (plan §6.4), closing a
    // real gap where `get_runtime_execution`/`get_runtime_execution_trace`
    // previously wrote no durable audit record at all (their calls only
    // reached the confirmed-dead `record_success_audit`/`record_error_audit`
    // no-ops). `grounded_answer`'s richer query-specific audit event
    // remains an *additional* emission on top of this, not a replacement —
    // different job (per-call access log vs structured business event with
    // query/runtime correlation), see plan §1.4/§6.4 hedge-audit finding #2.
    let outcome = if result.is_error { "failed" } else { "succeeded" };
    record_canonical_mcp_audit(
        state,
        auth,
        request_id,
        "mcp.tools.call",
        outcome,
        Some(format!("MCP tool '{}' call {outcome}.", parsed.name)),
        Some(format!(
            "principal {} called MCP tool '{}' on {} surface in {duration_ms}ms with args {}",
            auth.principal_id,
            parsed.name,
            surface.label(),
            redacted_args_summary(&parsed.arguments),
        )),
        Vec::new(),
    )
    .await;

    success_response(id, json!(result))
}

/// Redacted `argsSummary` for the mandatory audit chokepoint: top-level
/// argument *keys* only, never values — tool arguments routinely carry
/// document bodies, base64 file payloads, and free-text queries that must
/// never land in an audit log.
fn redacted_args_summary(arguments: &Value) -> String {
    match arguments.as_object() {
        Some(map) if map.is_empty() => "{}".to_string(),
        Some(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort_unstable();
            format!("{{{}}}", keys.join(", "))
        }
        None => "(non-object arguments)".to_string(),
    }
}

pub(crate) fn descriptor_for(name: &str) -> Option<McpToolDescriptor> {
    catalog::descriptor(name)
        .or_else(|| documents::descriptor(name))
        .or_else(|| document_image::descriptor(name))
        .or_else(|| grounded::descriptor(name))
        .or_else(|| runtime::descriptor(name))
        .or_else(|| web_ingest::descriptor(name))
        .or_else(|| graph::descriptor(name))
}

pub(crate) async fn call_named_tool<'a>(
    name: &'a str,
    context: ToolCallContext<'a>,
    arguments: &'a Value,
) -> Option<McpToolResult> {
    if let Some(result) = catalog::call_tool(name, context, arguments).await {
        Some(result)
    } else if let Some(result) = documents::call_tool(name, context, arguments).await {
        Some(result)
    } else if let Some(result) = document_image::call_tool(name, context, arguments).await {
        Some(result)
    } else if let Some(result) = grounded::call_tool(name, context, arguments).await {
        Some(result)
    } else if let Some(result) = runtime::call_tool(name, context, arguments).await {
        Some(result)
    } else if let Some(result) = web_ingest::call_tool(name, context, arguments).await {
        Some(result)
    } else {
        graph::call_tool(name, context, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uuid::Uuid;

    use crate::{
        domains::iam::PrincipalKind,
        interfaces::http::{
            auth::{AuthContext, AuthGrant, AuthTokenKind},
            authorization::{
                POLICY_LIBRARY_READ, POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ,
            },
            mcp::{
                McpToolSurface,
                tools::{READ_DOCUMENT_TOOL_NAME, documents, grounded, visible_tool_names},
            },
        },
    };

    fn auth_with_query_and_memory_access() -> AuthContext {
        AuthContext {
            token_id: Uuid::nil(),
            principal_id: Uuid::nil(),
            parent_principal_id: None,
            workspace_id: None,
            token_kind: AuthTokenKind::Principal(PrincipalKind::ApiToken),
            scopes: Vec::new(),
            grants: vec![
                AuthGrant {
                    id: Uuid::from_u128(1),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_QUERY_RUN[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(2),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_MCP_MEMORY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(3),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_RUNTIME_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
                AuthGrant {
                    id: Uuid::from_u128(4),
                    resource_kind: "library".to_string(),
                    resource_id: Uuid::from_u128(11),
                    permission_kind: POLICY_LIBRARY_READ[0].to_string(),
                    workspace_id: Some(Uuid::from_u128(101)),
                    library_id: Some(Uuid::from_u128(11)),
                    document_id: None,
                },
            ],
            workspace_memberships: Vec::new(),
            visible_workspace_ids: BTreeSet::new(),
            is_system_admin: false,
            system_role: None,
        }
    }

    #[test]
    fn visible_tools_prioritize_grounded_answer_before_raw_search_tools() {
        let tools =
            visible_tool_names(&auth_with_query_and_memory_access(), McpToolSurface::Diagnostics);
        let grounded_index =
            tools.iter().position(|name| name == "grounded_answer").expect("grounded_answer");
        let search_index =
            tools.iter().position(|name| name == "search_documents").expect("search_documents");
        let read_index =
            tools.iter().position(|name| name == READ_DOCUMENT_TOOL_NAME).expect("read_document");

        assert!(grounded_index < search_index);
        assert!(grounded_index < read_index);
    }

    #[test]
    fn answer_surface_exposes_read_only_agent_tools() {
        let tools = super::visible_tool_names_with_capabilities(
            &auth_with_query_and_memory_access(),
            McpToolSurface::Answer,
            super::ToolVisibilityCapabilities { agent_vision_available: true },
        );
        let canonical = crate::interfaces::http::mcp::MCP_ANSWER_TOOL_NAMES.as_slice();

        for expected in [
            "grounded_answer",
            "list_workspaces",
            "list_libraries",
            "list_documents",
            "search_documents",
            READ_DOCUMENT_TOOL_NAME,
            "search_entities",
            "get_graph_topology",
            "list_relations",
            "get_communities",
        ] {
            assert!(tools.iter().any(|name| name == expected), "missing {expected}");
        }
        for forbidden in [
            "create_workspace",
            "create_library",
            "update_workspace",
            "update_library",
            "create_documents",
            "create_document_revision",
            "delete_document",
            "get_operation",
            "submit_web_run",
            "cancel_web_run",
        ] {
            assert!(!tools.iter().any(|name| name == forbidden), "forbidden {forbidden}");
        }
        assert!(canonical.contains(&"list_documents"));
        assert!(canonical.contains(&"view_document_image"));
        assert_eq!(canonical.len(), tools.len());
    }

    #[test]
    fn view_document_image_only_visible_when_agent_vision_available() {
        let auth = auth_with_query_and_memory_access();
        let no_vision = super::visible_tool_names_with_capabilities(
            &auth,
            McpToolSurface::Answer,
            super::ToolVisibilityCapabilities { agent_vision_available: false },
        );
        assert!(
            !no_vision.iter().any(|name| name == "view_document_image"),
            "view_document_image must be hidden when the Agent binding is not multimodal"
        );

        let with_vision = super::visible_tool_names_with_capabilities(
            &auth,
            McpToolSurface::Answer,
            super::ToolVisibilityCapabilities { agent_vision_available: true },
        );
        assert!(
            with_vision.iter().any(|name| name == "view_document_image"),
            "view_document_image must surface when the Agent binding is multimodal"
        );

        // Diagnostics surface mirrors the gating.
        let diag_with_vision = super::visible_tool_names_with_capabilities(
            &auth,
            McpToolSurface::Diagnostics,
            super::ToolVisibilityCapabilities { agent_vision_available: true },
        );
        assert!(
            diag_with_vision.iter().any(|name| name == "view_document_image"),
            "diagnostics surface must also expose view_document_image when vision is available"
        );
    }

    #[test]
    fn list_documents_descriptor_keeps_change_summaries_on_grounded_answer() {
        let descriptor = documents::descriptor("list_documents").expect("list_documents");

        assert!(descriptor.description.contains("versioned change-summary questions"));
        assert!(descriptor.description.contains("use `grounded_answer`"));
        assert!(descriptor.description.contains("not as the final absence check"));
    }

    #[test]
    fn grounded_answer_descriptor_guides_content_and_setup_probes() {
        let descriptor = grounded::descriptor("grounded_answer").expect("grounded_answer");

        assert!(descriptor.description.contains("exact current user question"));
        assert!(descriptor.description.contains("built-in UI dispatches it"));
        assert!(descriptor.description.contains("responseProfile=compact"));
        assert!(descriptor.description.contains("maxReferences<=8"));
        assert!(descriptor.description.contains("finalAnswerReady=true"));
        assert!(descriptor.description.contains("one exact-query repair"));
        assert!(descriptor.description.contains("clarification.required=true"));
        assert!(descriptor.description.contains("mustPreserveSpans"));
        assert!(
            descriptor
                .description
                .contains(crate::services::mcp::agent_policy::AGENT_POLICY_VERSION)
        );
    }

    #[test]
    fn document_descriptors_do_not_make_search_a_content_answer() {
        let search = documents::descriptor("search_documents").expect("search_documents");
        let read = documents::descriptor(READ_DOCUMENT_TOOL_NAME).expect("read_document");

        assert!(search.description.contains("grounded_answer"));
        assert!(search.description.contains("search response alone is NOT enough"));
        assert!(search.description.contains("Follow relevant hits with `read_document`"));
        assert!(read.description.contains("after a `grounded_answer` result"));
        assert!(read.description.contains("package/module, path, and parameter/default/example"));
    }

    #[test]
    fn ui_and_external_mcp_share_the_same_tool_contract_hash() {
        let auth = auth_with_query_and_memory_access();

        for agent_vision_available in [false, true] {
            let capabilities = super::ToolVisibilityCapabilities { agent_vision_available };
            let mcp_contract = super::visible_tool_contract_with_capabilities(
                &auth,
                McpToolSurface::Answer,
                capabilities,
            )
            .expect("canonical MCP descriptors must be complete");
            let ui_tools =
                crate::services::query::agent_loop::answer_surface_tool_defs(&auth, capabilities)
                    .expect("complete UI answer tool contract");
            let ui_hash = super::tool_contract_hash_from_components(
                ui_tools
                    .iter()
                    .map(|tool| (tool.name.as_str(), tool.description.as_str(), &tool.parameters)),
            );

            assert_eq!(ui_hash, mcp_contract.hash);
            assert_eq!(ui_tools.len(), mcp_contract.descriptors.len());
        }
    }

    #[test]
    fn tool_contract_hash_is_deterministic_and_schema_sensitive() {
        let names = vec!["grounded_answer".to_string(), "list_libraries".to_string()];
        let first = super::tool_contract_for_names(&names).expect("known descriptors");
        let second = super::tool_contract_for_names(&names).expect("known descriptors");

        assert_eq!(first.hash, second.hash);
        assert!(first.hash.starts_with("sha256:"));

        let changed_schema = serde_json::json!({ "type": "object", "properties": {} });
        let changed_hash =
            super::tool_contract_hash_from_components(first.descriptors.iter().map(|descriptor| {
                let schema = if descriptor.name == "grounded_answer" {
                    &changed_schema
                } else {
                    &descriptor.input_schema
                };
                (descriptor.name, descriptor.description, schema)
            }));
        assert_ne!(changed_hash, first.hash);
    }

    #[test]
    fn missing_requested_descriptor_fails_the_contract_preflight() {
        let error = super::tool_contract_for_names(&[
            "grounded_answer".to_string(),
            "missing_tool".to_string(),
        ])
        .expect_err("unknown requested tools must fail loudly");

        assert!(error.to_string().contains("missing_tool"));
    }
}
