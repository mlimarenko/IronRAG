// ---------------------------------------------------------------------------
// repair
// ---------------------------------------------------------------------------

use super::*;
use maintenance_repair as repair;

#[allow(
    clippy::print_stdout,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
pub(super) async fn run_repair(state: &AppState, command: RepairCommand) -> Result<()> {
    match command {
        RepairCommand::KnowledgeProjectionMetadata { library, dry_run, json } => {
            let report = knowledge_projection_metadata::knowledge_projection_metadata(
                state, library, dry_run,
            )
            .await?;
            println!("{}", format_projection_metadata_report(&report, dry_run, json)?);
            Ok(())
        }
        RepairCommand::OrphanKnowledgeDocuments { library, dry_run, json } => {
            let report =
                orphan_knowledge_documents::orphan_knowledge_documents(state, library, dry_run)
                    .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "repair.orphan-knowledge-documents  libraries={}  documents={}  chunk_vector_rows={}  graph_evidence_rows={}  graph_rebuilds={}  graph_rebuilds_skipped={}  dry_run={}",
                    report.libraries_repaired,
                    report.orphan_documents_removed,
                    report.chunk_vector_rows_removed,
                    report.graph_evidence_rows_removed,
                    report.graph_rebuilds_completed,
                    report.graph_rebuilds_skipped,
                    dry_run,
                );
            }
            anyhow::ensure!(
                report.graph_rebuilds_skipped == 0,
                "orphan purge completed but {} runtime-graph re-projection(s) failed; \
                 run `rebuild runtime-graph --library <id>` for the warned libraries",
                report.graph_rebuilds_skipped,
            );
            Ok(())
        }
        RepairCommand::NullHeads { library, json } => {
            repair_commands::null_heads(state, library, json).await
        }
        RepairCommand::NullHeadsAuto { library, json } => {
            repair_commands::null_heads_auto(state, library, json).await
        }
        RepairCommand::ClearRecoveryDeadLetter { document } => {
            repair_commands::clear_recovery_dead_letter(state, document).await
        }
        RepairCommand::WebhookOutboxDeadLetter { outbox, json } => {
            repair_commands::webhook_outbox_dead_letter(state, outbox, json).await
        }
        RepairCommand::WebhookOutboxDeadLetterResolve {
            outbox,
            reason_code,
            acknowledge_not_delivered,
            json,
        } => {
            repair_commands::webhook_outbox_dead_letter_resolve(
                state,
                outbox,
                reason_code,
                acknowledge_not_delivered,
                json,
            )
            .await
        }
        RepairCommand::WebhookDeliveryAbandon {
            subscription,
            acknowledge_duplicate_delivery_risk,
            json,
        } => {
            repair_commands::webhook_delivery_abandon(
                state,
                subscription,
                acknowledge_duplicate_delivery_risk,
                json,
            )
            .await
        }
        RepairCommand::OrphanVectorRows { dry_run, json } => {
            repair_commands::orphan_vector_rows(state, dry_run, json).await
        }
        RepairCommand::TerminalGraphStates { library, dry_run, json } => {
            repair_commands::terminal_graph_states(state, library, dry_run, json).await
        }
        RepairCommand::StrandedFanOutOperations { dry_run, json } => {
            repair_commands::stranded_fan_out_operations(state, dry_run, json).await
        }
        RepairCommand::ContradictedFanOutOutcomes { dry_run, json } => {
            repair_commands::contradicted_fan_out_outcomes(state, dry_run, json).await
        }
    }
}

fn format_projection_metadata_report(
    report: &knowledge_projection_metadata::KnowledgeProjectionMetadataReport,
    dry_run: bool,
    json: bool,
) -> Result<String> {
    if json {
        Ok(serde_json::to_string_pretty(report)?)
    } else {
        Ok(format!(
            "repair.knowledge-projection-metadata  libraries={}  document_rows={}  revision_rows={}  dry_run={}",
            report.libraries_reconciled,
            report.document_rows_updated,
            report.revision_rows_updated,
            dry_run,
        ))
    }
}

mod repair_commands {
    use super::*;

    macro_rules! terminal_println {
        ($($argument:tt)*) => {{
            use std::io::Write as _;
            writeln!(std::io::stdout().lock(), $($argument)*)?;
        }};
    }

    pub(super) async fn null_heads(
        state: &AppState,
        library: Option<Uuid>,
        json: bool,
    ) -> Result<()> {
        let report = repair::promote_null_heads(state, library).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.null-heads  libraries_scanned={}  promoted={}  skipped_no_chunks={}",
                report.libraries_scanned,
                report.promoted,
                report.skipped_no_chunks,
            );
        }
        Ok(())
    }

    pub(super) async fn null_heads_auto(
        state: &AppState,
        library: Option<Uuid>,
        json: bool,
    ) -> Result<()> {
        let report = repair::promote_null_heads_auto(state, library).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.null-heads-auto  libraries_scanned={}  candidates={}  promoted={}  failed={}  dead_lettered={}  cooldown_skipped={}",
                report.libraries_scanned,
                report.candidates_considered,
                report.promoted,
                report.failed,
                report.dead_lettered,
                report.cooldown_skipped,
            );
        }
        Ok(())
    }

    pub(super) async fn orphan_vector_rows(
        state: &AppState,
        dry_run: bool,
        json: bool,
    ) -> Result<()> {
        let report = vector_orphan_reconcile::orphaned_vector_rows(state, dry_run).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.orphan-vector-rows  relations={}  lanes={}  rows={}  dry_run={}",
                report.relations_with_orphans,
                report.lanes,
                report.rows,
                dry_run,
            );
            for relation in &report.details {
                for lane in &relation.lanes {
                    terminal_println!(
                        "  relation={}  library_id={}  embedding_model_key={}  vector_kind={}  rows={}",
                        relation.relation_name,
                        lane.library_id,
                        lane.embedding_model_key,
                        lane.vector_kind,
                        lane.row_count,
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) async fn terminal_graph_states(
        state: &AppState,
        library: Option<Uuid>,
        dry_run: bool,
        json: bool,
    ) -> Result<()> {
        let report = repair::reconcile_terminal_graph_states(state, library, dry_run).await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.terminal-graph-states  terminal_state={}  candidates={}  updated={}  dry_run={}",
                report.terminal_state,
                report.candidates,
                report.updated,
                report.dry_run,
            );
            for row in &report.by_library {
                terminal_println!("  library_id={}  revisions={}", row.library_id, row.revisions);
            }
        }
        Ok(())
    }

    pub(super) async fn stranded_fan_out_operations(
        state: &AppState,
        dry_run: bool,
        json: bool,
    ) -> Result<()> {
        let report = repair::reconcile_stranded_fan_out_operations(state, dry_run).await?;
        print_fan_out_repair("stranded-fan-out-operations", report, json)
    }

    pub(super) async fn contradicted_fan_out_outcomes(
        state: &AppState,
        dry_run: bool,
        json: bool,
    ) -> Result<()> {
        let report = repair::reconcile_contradicted_fan_out_outcomes(state, dry_run).await?;
        print_fan_out_repair("contradicted-fan-out-outcomes", report, json)
    }

    /// One printer for both fan-out repair cohorts: the report shape is shared,
    /// only the command label differs.
    fn print_fan_out_repair(
        label: &str,
        report: repair::FanOutRepairReport,
        json: bool,
    ) -> Result<()> {
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            terminal_println!(
                "repair.{label}  candidates={}  settled={}  dry_run={}",
                report.candidates,
                report.settled,
                report.dry_run,
            );
            for row in &report.by_kind {
                terminal_println!(
                    "  operation_kind={}  operations={}",
                    row.operation_kind,
                    row.operations
                );
            }
        }
        Ok(())
    }

    pub(super) async fn clear_recovery_dead_letter(state: &AppState, document: Uuid) -> Result<()> {
        let cleared =
            repair::clear_recovery_dead_letter(&state.persistence.postgres, document).await?;
        if cleared {
            terminal_println!("cleared dead_letter_at for document {document}");
            Ok(())
        } else {
            bail!("no dead_letter_at mark to clear for document {document}");
        }
    }

    pub(super) async fn webhook_outbox_dead_letter(
        state: &AppState,
        outbox: Uuid,
        json: bool,
    ) -> Result<()> {
        let report = webhook_outbox_ops::requeue_dead_letter_webhook_lifecycle_outbox(
            &state.persistence.postgres,
            outbox,
        )
        .await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        }
        if !report.requeued {
            bail!(
                "webhook lifecycle outbox row {outbox} was not requeued: it does not exist or is no longer dead_letter"
            );
        }
        if !json {
            terminal_println!(
                "repair.webhook-outbox-dead-letter  outbox_id={}  transition=dead_letter->pending  delivery=not-triggered",
                report.outbox_id,
            );
        }
        Ok(())
    }

    pub(super) async fn webhook_outbox_dead_letter_resolve(
        state: &AppState,
        outbox: Uuid,
        reason_code: String,
        acknowledge_not_delivered: bool,
        json: bool,
    ) -> Result<()> {
        if !acknowledge_not_delivered {
            bail!("refusing to resolve an undelivered webhook without --acknowledge-not-delivered");
        }
        let report = webhook_outbox_ops::resolve_dead_letter_webhook_lifecycle_outbox(
            &state.persistence.postgres,
            outbox,
            &reason_code,
        )
        .await?;
        if json {
            terminal_println!("{}", serde_json::to_string_pretty(&report)?);
        }
        if !report.resolved {
            bail!(
                "webhook lifecycle outbox row {outbox} was not resolved: it does not exist or is no longer dead_letter"
            );
        }
        if !json {
            terminal_println!(
                "repair.webhook-outbox-dead-letter-resolve  outbox_id={}  transition=dead_letter->resolved  reason_code={}  delivered=false",
                report.outbox_id,
                report.reason_code,
            );
        }
        Ok(())
    }

    pub(super) async fn webhook_delivery_abandon(
        state: &AppState,
        subscription: Uuid,
        acknowledge_duplicate_delivery_risk: bool,
        json: bool,
    ) -> Result<()> {
        if !acknowledge_duplicate_delivery_risk {
            bail!(
                "refusing to abandon delivery ownership without --acknowledge-duplicate-delivery-risk"
            );
        }
        let abandoned = webhook_repository::force_abandon_draining_webhook_deliveries(
            &state.persistence.postgres,
            subscription,
        )
        .await?;
        if abandoned == 0 {
            bail!(
                "subscription {subscription} is not a tombstoned drain with an active delivery owner"
            );
        }
        let delete_outcome = webhook_repository::delete_webhook_subscription(
            &state.persistence.postgres,
            subscription,
        )
        .await?;
        if delete_outcome != webhook_repository::DeleteWebhookSubscriptionOutcome::Deleted {
            bail!("subscription {subscription} remained draining after explicit abandon");
        }
        if json {
            terminal_println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "subscription_id": subscription,
                    "abandoned_deliveries": abandoned,
                    "deleted": true,
                }))?
            );
        } else {
            terminal_println!(
                "repair.webhook-delivery-abandon  subscription_id={}  abandoned={}  deleted=true  duplicate_delivery_risk=acknowledged",
                subscription,
                abandoned,
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_metadata_report_preserves_text_and_json_fields() -> Result<()> {
        let report = knowledge_projection_metadata::KnowledgeProjectionMetadataReport {
            libraries_reconciled: 2,
            document_rows_updated: 3,
            revision_rows_updated: 5,
        };
        assert_eq!(
            format_projection_metadata_report(&report, true, false)?,
            "repair.knowledge-projection-metadata  libraries=2  document_rows=3  revision_rows=5  dry_run=true",
        );
        assert_eq!(
            format_projection_metadata_report(&report, false, false)?,
            "repair.knowledge-projection-metadata  libraries=2  document_rows=3  revision_rows=5  dry_run=false",
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&format_projection_metadata_report(
                &report, true, true,
            )?)?,
            serde_json::json!({
                "libraries_reconciled": 2,
                "document_rows_updated": 3,
                "revision_rows_updated": 5,
            }),
        );
        Ok(())
    }
}
