// ---------------------------------------------------------------------------
// migrate
// ---------------------------------------------------------------------------

use super::*;

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
pub(super) async fn run_migrate(state: &AppState, command: MigrateCommand) -> Result<()> {
    match command {
        MigrateCommand::ContentVisibilityIntentSchema {
            import_admission_manifest,
            import_admission_sha256,
            accepted_recovery_snapshot_id,
            source_baseline_sha384,
            admitted_by,
            apply,
            json,
        } => {
            let admission = if let Some(path) = import_admission_manifest {
                Some(
                    content_visibility_intent_schema::CertifiedImportAdmission::from_file(
                        &path,
                        import_admission_sha256
                            .as_deref()
                            .context("missing certificate SHA-256 pin")?,
                        accepted_recovery_snapshot_id
                            .as_deref()
                            .context("missing accepted recovery snapshot pin")?,
                        source_baseline_sha384.as_deref().context("missing source baseline pin")?,
                        admitted_by.as_deref().context("missing admitting operator")?,
                    )
                    .await?,
                )
            } else {
                None
            };
            let report =
                content_visibility_intent_schema::migrate_content_visibility_intent_schema(
                    &state.persistence.postgres,
                    apply,
                    &state.content_storage,
                    admission.as_ref(),
                )
                .await?;
            emit_schema_migration("migrate.content-visibility-intent-schema", json, &report)
        }
        MigrateCommand::IntegrationProvisioningSchema { apply, json } => {
            migrate_integration_provisioning_schema(state, apply, json).await
        }
        MigrateCommand::OpsFailureDispositionSchema { apply, json } => {
            migrate_ops_failure_disposition_schema(state, apply, json).await
        }
        MigrateCommand::QueryRerankBindingPurpose { apply, json } => {
            migrate_query_rerank_binding_purpose(state, apply, json).await
        }
        MigrateCommand::DecisionCatalogSchema { apply, json } => {
            migrate_decision_catalog_schema(state, apply, json).await
        }
        MigrateCommand::AdmissionVisibilityPolicySchema { apply, json } => {
            migrate_admission_visibility_policy_schema(state, apply, json).await
        }
        MigrateCommand::IntegrationProvisioningOwnership {
            authority_principal,
            workspace,
            library,
            apply,
            json,
        } => {
            migrate_integration_provisioning_ownership(
                state,
                apply,
                authority_principal,
                workspace,
                library,
                json,
            )
            .await
        }
        MigrateCommand::CredentialSecrets { apply, batch_size, json } => {
            migrate_credential_secrets(state, apply, batch_size, json).await
        }
    }
}

/// The counters every additive schema migration reports, so the shared
/// `emit_schema_migration` printer owns the one textual shape.
trait SchemaMigrationReport {
    fn apply(&self) -> bool;
    fn already_current(&self) -> bool;
    fn schema_applied(&self) -> bool;
}

impl SchemaMigrationReport
    for content_visibility_intent_schema::ContentVisibilityIntentSchemaMigrationReport
{
    fn apply(&self) -> bool {
        self.apply
    }
    fn already_current(&self) -> bool {
        self.already_current
    }
    fn schema_applied(&self) -> bool {
        self.schema_applied
    }
}

impl SchemaMigrationReport
    for service_identity_schema::IntegrationProvisioningSchemaMigrationReport
{
    fn apply(&self) -> bool {
        self.apply
    }

    fn already_current(&self) -> bool {
        self.already_current
    }

    fn schema_applied(&self) -> bool {
        self.schema_applied
    }
}

impl SchemaMigrationReport
    for ops_failure_disposition_schema::OpsFailureDispositionSchemaMigrationReport
{
    fn apply(&self) -> bool {
        self.apply
    }

    fn already_current(&self) -> bool {
        self.already_current
    }

    fn schema_applied(&self) -> bool {
        self.schema_applied
    }
}

impl SchemaMigrationReport for ai_binding_purpose_schema::QueryRerankBindingPurposeMigrationReport {
    fn apply(&self) -> bool {
        self.apply
    }

    fn already_current(&self) -> bool {
        self.already_current
    }

    fn schema_applied(&self) -> bool {
        self.schema_applied
    }
}

impl SchemaMigrationReport for decision_catalog_schema::DecisionCatalogSchemaMigrationReport {
    fn apply(&self) -> bool {
        self.apply
    }

    fn already_current(&self) -> bool {
        self.already_current
    }

    fn schema_applied(&self) -> bool {
        self.schema_applied
    }
}

impl SchemaMigrationReport
    for admission_visibility_policy_schema::AdmissionVisibilityPolicySchemaMigrationReport
{
    fn apply(&self) -> bool {
        self.apply
    }

    fn already_current(&self) -> bool {
        self.already_current
    }

    fn schema_applied(&self) -> bool {
        self.schema_applied
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
fn emit_schema_migration<R: serde::Serialize + SchemaMigrationReport>(
    label: &str,
    json: bool,
    report: &R,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!(
            "{label}  apply={}  already_current={}  schema_applied={}",
            report.apply(),
            report.already_current(),
            report.schema_applied()
        );
    }
    Ok(())
}

async fn migrate_integration_provisioning_schema(
    state: &AppState,
    apply: bool,
    json: bool,
) -> Result<()> {
    let report = service_identity_schema::migrate_integration_provisioning_schema(
        &state.persistence.postgres,
        apply,
    )
    .await?;
    emit_schema_migration("migrate.integration-provisioning-schema", json, &report)
}

async fn migrate_ops_failure_disposition_schema(
    state: &AppState,
    apply: bool,
    json: bool,
) -> Result<()> {
    let report = ops_failure_disposition_schema::migrate_ops_failure_disposition_schema(
        &state.persistence.postgres,
        apply,
    )
    .await?;
    emit_schema_migration("migrate.ops-failure-disposition-schema", json, &report)
}

async fn migrate_query_rerank_binding_purpose(
    state: &AppState,
    apply: bool,
    json: bool,
) -> Result<()> {
    let report = ai_binding_purpose_schema::migrate_query_rerank_binding_purpose(
        &state.persistence.postgres,
        apply,
    )
    .await?;
    emit_schema_migration("migrate.query-rerank-binding-purpose", json, &report)
}

async fn migrate_decision_catalog_schema(state: &AppState, apply: bool, json: bool) -> Result<()> {
    let report = decision_catalog_schema::migrate_decision_catalog_schema(
        &state.persistence.postgres,
        apply,
    )
    .await?;
    emit_schema_migration("migrate.decision-catalog-schema", json, &report)
}

async fn migrate_admission_visibility_policy_schema(
    state: &AppState,
    apply: bool,
    json: bool,
) -> Result<()> {
    let report = admission_visibility_policy_schema::migrate_admission_visibility_policy_schema(
        &state.persistence.postgres,
        apply,
    )
    .await?;
    emit_schema_migration("migrate.admission-visibility-policy-schema", json, &report)
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
async fn migrate_integration_provisioning_ownership(
    state: &AppState,
    apply: bool,
    authority_principal: Uuid,
    workspace: Uuid,
    library: Vec<Uuid>,
    json: bool,
) -> Result<()> {
    let report = service_identity_schema::assign_integration_provisioning_ownership(
        &state.persistence.postgres,
        apply,
        authority_principal,
        workspace,
        library,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "migrate.integration-provisioning-ownership  apply={}  authority={}  workspace={}  libraries={}  assigned_workspaces={}  assigned_libraries={}",
            report.apply,
            report.authority_principal_id,
            report.workspace_id,
            report.library_ids.len(),
            report.assigned_workspace_count,
            report.assigned_library_count,
        );
    }
    Ok(())
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
async fn migrate_credential_secrets(
    state: &AppState,
    apply: bool,
    batch_size: usize,
    json: bool,
) -> Result<()> {
    let report = credential_secrets::migrate_credential_secrets(
        &state.persistence.postgres,
        &state.credential_cipher,
        CredentialSecretMigrationOptions { apply, batch_size },
    )
    .await?;
    let invalid_values = report.invalid_values();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "migrate.credential-secrets  apply={}  ai_scanned={}  ai_rewrap_candidates={}  ai_migrated={}  ai_concurrent={}  ai_invalid={}  service_identity_scanned={}  service_identity_rewrap_candidates={}  service_identity_migrated={}  service_identity_concurrent={}  service_identity_invalid={}  webhook_scanned={}  webhook_rewrap_candidates={}  webhook_migrated={}  webhook_concurrent={}  webhook_invalid={}  webhook_headers_scanned={}  webhook_header_rewrap_candidates={}  webhook_headers_migrated={}  webhook_header_concurrent={}  webhook_header_invalid={}  invalid_samples={}",
            report.apply,
            report.ai_rows_scanned,
            report.ai_rewrap_candidates,
            report.ai_migrated,
            report.ai_concurrent_changes,
            report.ai_invalid,
            report.service_identity_rows_scanned,
            report.service_identity_rewrap_candidates,
            report.service_identity_migrated,
            report.service_identity_concurrent_changes,
            report.service_identity_invalid,
            report.webhook_rows_scanned,
            report.webhook_rewrap_candidates,
            report.webhook_migrated,
            report.webhook_concurrent_changes,
            report.webhook_invalid,
            report.webhook_header_rows_scanned,
            report.webhook_header_rewrap_candidates,
            report.webhook_headers_migrated,
            report.webhook_header_concurrent_changes,
            report.webhook_header_invalid,
            report.invalid_samples.len(),
        );
    }
    if invalid_values > 0 {
        anyhow::bail!(
            "credential audit found {invalid_values} invalid stored value(s); inspect the redacted report and repair or rotate those credentials"
        );
    }
    Ok(())
}
