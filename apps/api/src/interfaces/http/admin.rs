use axum::{Json, Router, extract::State, routing::get};
use tracing::warn;

use ironrag_contracts::{
    admin::{
        AdminCapabilityState, AdminSection, AdminSectionSummary, AdminSurface, AdminViewerSummary,
        CapabilityGate,
    },
    auth::{IamMe, PermissionKind},
};

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{
            POLICY_IAM_ADMIN, POLICY_MCP_AUDIT_REVIEW, POLICY_PROVIDERS_ADMIN, POLICY_USAGE_READ,
        },
        iam::load_contract_me,
        router_support::ApiError,
    },
};

pub fn router() -> Router<AppState> {
    Router::new().route("/admin/surface", get(get_admin_surface))
}

#[tracing::instrument(level = "info", name = "http.get_admin_surface", skip_all)]
async fn get_admin_surface(
    auth: AuthContext,
    State(state): State<AppState>,
) -> Result<Json<AdminSurface>, ApiError> {
    auth.require_any_scope(POLICY_IAM_ADMIN)?;

    let me = load_contract_me(&state, &auth).await?;
    let capabilities = AdminCapabilityState {
        admin_enabled: true,
        can_manage_tokens: auth.has_any_scope(POLICY_IAM_ADMIN),
        can_read_audit: auth.has_any_scope(POLICY_MCP_AUDIT_REVIEW),
        can_read_operations: auth.has_any_scope(POLICY_USAGE_READ),
        can_manage_ai: auth.has_any_scope(POLICY_PROVIDERS_ADMIN),
    };

    Ok(Json(AdminSurface {
        viewer: map_admin_viewer_summary(&me, &capabilities),
        capabilities: capabilities.clone(),
        sections: build_section_summaries(&capabilities),
        warnings: Vec::new(),
    }))
}

fn map_admin_viewer_summary(me: &IamMe, capabilities: &AdminCapabilityState) -> AdminViewerSummary {
    let display_name = me
        .user
        .as_ref()
        .and_then(|user| user.display_name.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| me.principal.display_label.clone());

    let access_label = if me
        .effective_grants
        .iter()
        .any(|grant| grant.permission_kind == PermissionKind::IamAdmin)
    {
        "IAM administrator"
    } else if me
        .effective_grants
        .iter()
        .any(|grant| grant.permission_kind == PermissionKind::WorkspaceAdmin)
    {
        "Workspace administrator"
    } else if capabilities.admin_enabled {
        warn!(
            principal_id = %me.principal.id,
            "admin surface resolved without explicit admin grant; falling back to generic admin label"
        );
        "Administrator"
    } else {
        "Operator"
    };

    AdminViewerSummary {
        principal_id: me.principal.id,
        display_name,
        access_label: access_label.to_string(),
        is_admin: capabilities.admin_enabled,
    }
}

fn build_section_summaries(capabilities: &AdminCapabilityState) -> Vec<AdminSectionSummary> {
    [
        AdminSection::Access,
        AdminSection::Mcp,
        AdminSection::Operations,
        AdminSection::Ai,
        AdminSection::Pricing,
        AdminSection::Settings,
    ]
    .into_iter()
    .map(|section| map_section_summary(section, capabilities))
    .collect()
}

fn map_section_summary(
    section: AdminSection,
    capabilities: &AdminCapabilityState,
) -> AdminSectionSummary {
    let gate = section_gate(section, capabilities);
    let summary = if gate.allowed {
        match section {
            AdminSection::Access => {
                "Token administration is available for the authenticated administrator."
            }
            AdminSection::Mcp => {
                "MCP administration is available through the canonical admin console."
            }
            AdminSection::Operations => {
                "Operations diagnostics are available for the authenticated administrator."
            }
            AdminSection::Ai => {
                "AI catalog and binding administration is available for the authenticated administrator."
            }
            AdminSection::Pricing => {
                "Pricing administration is available through the AI administration capability."
            }
            AdminSection::Settings => {
                "Administrative settings are available for the authenticated administrator."
            }
        }
    } else {
        match section {
            AdminSection::Access => "IAM administration permission is required.",
            AdminSection::Mcp => "Administrator access is required for MCP administration.",
            AdminSection::Operations => "Operations-read capability is required.",
            AdminSection::Ai => "Provider and binding administration capability is required.",
            AdminSection::Pricing => {
                "AI administration capability is required for pricing controls."
            }
            AdminSection::Settings => "Administrator access is required for settings.",
        }
    };

    AdminSectionSummary {
        section,
        title: format!("{} administration", section.label()),
        summary: summary.to_string(),
        item_count: None,
        gate,
    }
}

fn section_gate(section: AdminSection, capabilities: &AdminCapabilityState) -> CapabilityGate {
    let (allowed, reason) = match section {
        AdminSection::Access => (
            capabilities.can_manage_tokens,
            Some("IAM administration permission is required.".to_string()),
        ),
        AdminSection::Mcp => (
            capabilities.admin_enabled,
            Some("Administrator access is required for MCP administration.".to_string()),
        ),
        AdminSection::Operations => (
            capabilities.can_read_operations,
            Some("Operations-read capability is required.".to_string()),
        ),
        AdminSection::Ai => (
            capabilities.can_manage_ai,
            Some("Provider and binding administration capability is required.".to_string()),
        ),
        AdminSection::Pricing => (
            capabilities.can_manage_ai,
            Some("AI administration capability is required for pricing controls.".to_string()),
        ),
        AdminSection::Settings => (
            capabilities.admin_enabled,
            Some("Administrator access is required for settings.".to_string()),
        ),
    };

    CapabilityGate { section, allowed, reason: if allowed { None } else { reason } }
}
