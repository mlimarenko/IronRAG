use super::tokens::{
    MintGrantScope, TokenResponseLookups, build_mint_grants, build_token_response,
};
use super::*;
use chrono::Utc;

#[test]
fn system_role_route_to_repo_round_trips() {
    for (route, repo) in [
        (SystemRole::Viewer, iam_repository::SystemRole::Viewer),
        (SystemRole::Operator, iam_repository::SystemRole::Operator),
        (SystemRole::Admin, iam_repository::SystemRole::Admin),
    ] {
        assert_eq!(map_route_system_role_to_repo(route), repo);
        assert_eq!(map_route_system_role_from_repo(repo), route);
    }
}

#[test]
fn repo_system_role_string_round_trips() {
    for repo in [
        iam_repository::SystemRole::Viewer,
        iam_repository::SystemRole::Operator,
        iam_repository::SystemRole::Admin,
    ] {
        assert_eq!(iam_repository::SystemRole::parse_wire(repo.as_str()), Some(repo));
    }
    assert_eq!(iam_repository::SystemRole::parse_wire("unknown"), None);
}

/// Declares, per resource kind, whether a grant for it can be created at all.
///
/// The `match` is exhaustive, so adding an `IamGrantResourceKind` variant does
/// not compile until this answer is written down — the same guarantee the
/// validator's own match gives, restated where the tests can read it.
const fn is_creatable_resource_kind(kind: &IamGrantResourceKind) -> bool {
    match kind {
        IamGrantResourceKind::Workspace | IamGrantResourceKind::Library => true,
        IamGrantResourceKind::System
        | IamGrantResourceKind::Document
        | IamGrantResourceKind::QuerySession
        | IamGrantResourceKind::AsyncOperation
        | IamGrantResourceKind::Connector
        | IamGrantResourceKind::ProviderCredential
        | IamGrantResourceKind::LibraryBinding => false,
    }
}

fn all_resource_kinds() -> [IamGrantResourceKind; 9] {
    [
        IamGrantResourceKind::System,
        IamGrantResourceKind::Workspace,
        IamGrantResourceKind::Library,
        IamGrantResourceKind::Document,
        IamGrantResourceKind::QuerySession,
        IamGrantResourceKind::AsyncOperation,
        IamGrantResourceKind::Connector,
        IamGrantResourceKind::ProviderCredential,
        IamGrantResourceKind::LibraryBinding,
    ]
}

fn all_permission_kinds() -> [IamPermissionKind; 13] {
    [
        IamPermissionKind::IamAdmin,
        IamPermissionKind::WorkspaceAdmin,
        IamPermissionKind::WorkspaceRead,
        IamPermissionKind::LibraryRead,
        IamPermissionKind::LibraryWrite,
        IamPermissionKind::DocumentRead,
        IamPermissionKind::DocumentWrite,
        IamPermissionKind::ConnectorAdmin,
        IamPermissionKind::CredentialAdmin,
        IamPermissionKind::BindingAdmin,
        IamPermissionKind::QueryRun,
        IamPermissionKind::OpsRead,
        IamPermissionKind::AuditRead,
    ]
}

#[test]
fn permission_kind_matches_expected_resource_kinds() {
    assert!(
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Workspace,
            IamPermissionKind::WorkspaceAdmin
        )
        .is_ok()
    );
    assert!(
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Library,
            IamPermissionKind::DocumentWrite
        )
        .is_ok()
    );
    // A permission outside the creatable kind's allow-list is still rejected.
    assert!(matches!(
        validate_permission_kind_for_resource(
            IamGrantResourceKind::Library,
            IamPermissionKind::WorkspaceAdmin
        ),
        Err(ApiError::BadRequest(_))
    ));
}

/// The permissive surface is exactly the creatable surface: a resource kind no
/// write path can construct accepts no permission at all.
#[test]
fn non_creatable_resource_kinds_accept_no_permission() {
    let mut denied = 0_usize;
    for resource_kind in all_resource_kinds() {
        if is_creatable_resource_kind(&resource_kind) {
            continue;
        }
        for permission_kind in all_permission_kinds() {
            assert!(
                matches!(
                    validate_permission_kind_for_resource(
                        resource_kind.clone(),
                        permission_kind.clone()
                    ),
                    Err(ApiError::BadRequest(_))
                ),
                "{} must not accept {}",
                resource_kind.as_str(),
                permission_kind.as_str()
            );
            denied += 1;
        }
    }
    assert_eq!(denied, 7 * 13, "the deny table must cover every non-creatable pair");
}

/// Each creatable kind accepts at least one permission, so the test above
/// cannot pass by the validator having become a blanket `false`.
#[test]
fn creatable_resource_kinds_accept_at_least_one_permission() {
    for resource_kind in all_resource_kinds() {
        if !is_creatable_resource_kind(&resource_kind) {
            continue;
        }
        assert!(
            all_permission_kinds().into_iter().any(|permission_kind| {
                validate_permission_kind_for_resource(resource_kind.clone(), permission_kind)
                    .is_ok()
            }),
            "{} must accept at least one permission",
            resource_kind.as_str()
        );
    }
}

/// Source-level contract behind the validator's deny arm.
///
/// A behavioural test cannot express "nobody made these creatable" — it keeps
/// passing the moment a new write path starts minting `connector` grants. This
/// one reads the whole `interfaces::http::iam` module (by listing the directory,
/// not by naming files, so a new module is covered the day it is added) and
/// asserts that every argument handed to `validate_permission_kind_for_resource`
/// is a literal `Workspace` or `Library`.
///
/// It fails on three distinct regressions: a call site passing another kind, a
/// call site passing a runtime value instead of a literal, and the call sites
/// disappearing altogether.
#[test]
fn grant_write_paths_create_only_creatable_resource_kinds() {
    const VALIDATOR: &str = "validate_permission_kind_for_resource";

    let module_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("interfaces")
        .join("http")
        .join("iam");
    let module_root = module_dir.with_extension("rs");

    let mut sources: Vec<(String, String)> = Vec::new();
    sources.push((
        "iam.rs".to_string(),
        std::fs::read_to_string(&module_root)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", module_root.display())),
    ));
    let entries = std::fs::read_dir(&module_dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", module_dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry must be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("module file name must be UTF-8")
            .to_string();
        // The test module is not a write path; scanning it would read this
        // test's own literals back as call sites.
        if !name.ends_with(".rs") || name == "tests.rs" {
            continue;
        }
        sources.push((
            name,
            std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display())),
        ));
    }
    assert!(
        sources.len() >= 6,
        "expected the whole iam module to be scanned, got {} files",
        sources.len()
    );

    let mut call_sites = 0_usize;
    for (name, source) in &sources {
        // Drop comment lines so the validator's own doc comment is not parsed
        // as a call.
        let code = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut cursor = 0_usize;
        while let Some(offset) = code[cursor..].find(VALIDATOR) {
            let start = cursor + offset;
            cursor = start + VALIDATOR.len();
            let tail = &code[cursor..];
            // Skip the definition itself, and any longer identifier that merely
            // contains this name.
            if !tail.starts_with('(') || code[..start].trim_end().ends_with("fn") {
                continue;
            }

            let arguments = balanced_argument_list(tail)
                .unwrap_or_else(|| panic!("unbalanced call to {VALIDATOR} in {name}"));
            let first_argument = arguments.split(',').next().unwrap_or_default().trim();
            let variant =
                first_argument.strip_prefix("IamGrantResourceKind::").unwrap_or_else(|| {
                    panic!(
                        "{name}: {VALIDATOR} must be called with a literal \
                         IamGrantResourceKind variant, got `{first_argument}`"
                    )
                });
            assert!(
                matches!(variant, "Workspace" | "Library"),
                "{name}: {VALIDATOR} is called with the non-creatable resource kind \
                 `{variant}`; making a resource kind creatable means adding a write \
                 path, an allow-list arm in the validator and a permission model — \
                 not just a call site"
            );
            call_sites += 1;
        }
    }

    assert!(call_sites > 0, "found no call site of {VALIDATOR}; the scan matched nothing");

    // The scan is only complete because the validator is module-private: a
    // wider visibility would let a module outside `iam/` call it, and this
    // directory listing would never see that call site.
    let module_root_source = sources
        .iter()
        .find(|(name, _)| name == "iam.rs")
        .map(|(_, source)| source.as_str())
        .expect("iam.rs must be among the scanned sources");
    assert!(
        module_root_source.contains(&format!("\nfn {VALIDATOR}(")),
        "{VALIDATOR} must stay module-private; widening its visibility puts call \
         sites outside the iam module, where this scan cannot reach them"
    );
}

/// Returns the text between the leading `(` of `input` and its matching `)`.
fn balanced_argument_list(input: &str) -> Option<&str> {
    let mut depth = 0_usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&input[1..index]);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn grant_resource_and_permission_strings_are_supported() {
    assert_eq!(IamGrantResourceKind::System.as_str(), "system");
    assert_eq!(IamPermissionKind::IamAdmin.as_str(), "iam_admin");
    assert_eq!(IamGrantResourceKind::ProviderCredential.as_str(), "provider_credential");
    assert_eq!(IamPermissionKind::WorkspaceAdmin.as_str(), "workspace_admin");
}

#[test]
fn system_administration_grant_maps_to_route_and_contract_models() {
    assert_eq!(
        map_grant_resource_kind("system").expect("system resource should map"),
        IamGrantResourceKind::System
    );
    assert_eq!(
        map_permission_kind("iam_admin").expect("IAM admin permission should map"),
        IamPermissionKind::IamAdmin
    );
    assert_eq!(
        map_permission_kind_contract("iam_admin")
            .expect("IAM admin contract permission should map"),
        ironrag_contracts::auth::PermissionKind::IamAdmin
    );
    assert_eq!(
        map_domain_grant_resource_kind_contract(GrantResourceKind::System),
        ironrag_contracts::auth::GrantResourceKind::System
    );
}

#[test]
fn build_mint_grants_supports_workspace_scope() {
    let workspace_id = Uuid::now_v7();
    let grants = build_mint_grants(
        MintGrantScope::Workspace(workspace_id),
        &[IamPermissionKind::WorkspaceRead, IamPermissionKind::LibraryRead],
        None,
    )
    .expect("workspace grants should build");

    assert_eq!(grants.len(), 2);
    assert!(grants.iter().all(|grant| grant.resource_kind == GrantResourceKind::Workspace));
    assert!(grants.iter().all(|grant| grant.resource_id == workspace_id));
}

#[test]
fn build_mint_grants_supports_multiple_libraries() {
    let library_a = Uuid::now_v7();
    let library_b = Uuid::now_v7();
    let grants = build_mint_grants(
        MintGrantScope::Libraries(vec![library_a, library_b]),
        &[IamPermissionKind::LibraryRead, IamPermissionKind::DocumentRead],
        None,
    )
    .expect("library grants should build");

    assert_eq!(grants.len(), 4);
    assert!(grants.iter().all(|grant| grant.resource_kind == GrantResourceKind::Library));
    assert!(grants.iter().any(|grant| grant.resource_id == library_a));
    assert!(grants.iter().any(|grant| grant.resource_id == library_b));
}

#[test]
fn build_token_response_includes_resolved_scope_and_permissions() {
    let principal_id = Uuid::now_v7();
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let issuer_id = Uuid::now_v7();
    let token = build_token_response(
        iam_repository::IamApiTokenRow {
            principal_id,
            workspace_id: Some(workspace_id),
            label: "Library token".to_string(),
            token_prefix: "inrg_demo".to_string(),
            status: "active".to_string(),
            expires_at: None,
            revoked_at: None,
            issued_by_principal_id: Some(issuer_id),
            last_used_at: None,
        },
        vec![iam_repository::ResolvedIamGrantScopeRow {
            id: Uuid::now_v7(),
            principal_id,
            resource_kind: "library".to_string(),
            resource_id: library_id,
            permission_kind: "library_read".to_string(),
            granted_at: Utc::now(),
            granted_by_principal_id: Some(issuer_id),
            expires_at: None,
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            document_id: None,
        }],
        &TokenResponseLookups {
            workspaces: [(
                workspace_id,
                TokenWorkspaceSummaryResponse {
                    id: workspace_id,
                    display_name: "Workspace One".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            libraries: [(
                library_id,
                TokenLibrarySummaryResponse {
                    id: library_id,
                    workspace_id,
                    display_name: "Library One".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            issuers: [(
                issuer_id,
                TokenIssuerResponse { principal_id: issuer_id, display_label: "Admin".to_string() },
            )]
            .into_iter()
            .collect(),
        },
    )
    .expect("token response should build");

    assert_eq!(token.scope.kind, TokenScopeKind::Library);
    assert_eq!(
        token.scope.workspace.as_ref().map(|workspace| workspace.display_name.as_str()),
        Some("Workspace One")
    );
    assert_eq!(token.scope.libraries[0].display_name, "Library One");
    assert_eq!(token.grants[0].permission_kind, IamPermissionKind::LibraryRead);
    assert_eq!(token.issuer.as_ref().map(|issuer| issuer.display_label.as_str()), Some("Admin"));
}
