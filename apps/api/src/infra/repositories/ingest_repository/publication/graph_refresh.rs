use super::*;

async fn lock_library_publication_source(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    anyhow::ensure!(
        catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut **transaction,
            workspace_id,
            library_id,
        )
        .await
        .context("lock graph-refresh publication library")?,
        "graph-refresh publication library disappeared"
    );
    sqlx::query_scalar::<_, i64>(
        "select source_truth_version
         from catalog_library
         where id = $1 and workspace_id = $2",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_one(&mut **transaction)
    .await
    .context("read graph-refresh publication source generation")
}

/// Publishes graph-only maintenance for the still-current readable revision.
///
/// No mutation is fabricated and no revision-ready event is emitted: the
/// document was already readable before this maintenance job started.
pub async fn publish_graph_refresh_success(
    postgres: &PgPool,
    input: &PublishGraphRefreshSuccess,
) -> anyhow::Result<PublishGraphRefreshSuccessOutcome> {
    publish_graph_refresh_success_with_effects(
        postgres,
        input,
        &GraphRefreshPublicationEffects::default(),
    )
    .await
}

pub async fn publish_graph_refresh_success_with_effects(
    postgres: &PgPool,
    input: &PublishGraphRefreshSuccess,
    effects: &GraphRefreshPublicationEffects,
) -> anyhow::Result<PublishGraphRefreshSuccessOutcome> {
    let mut transaction = postgres.begin().await.context("begin graph-refresh publication")?;
    let observed_source_truth_version =
        lock_library_publication_source(&mut transaction, input.workspace_id, input.library_id)
            .await?;
    publish_graph_refresh_success_in_transaction(
        transaction,
        input,
        effects,
        observed_source_truth_version,
        false,
    )
    .await
}

/// Publishes graph-only maintenance in the same authority transaction that
/// guarded graph reconcile.
pub async fn publish_graph_refresh_success_with_graph_guard(
    guard: IngestRevisionGraphPublicationGuard,
    input: &PublishGraphRefreshSuccess,
    effects: &GraphRefreshPublicationEffects,
) -> anyhow::Result<PublishGraphRefreshSuccessOutcome> {
    anyhow::ensure!(
        guard.authority.workspace_id == input.workspace_id
            && guard.authority.library_id == input.library_id
            && guard.authority.revision_id == input.revision_id
            && guard.authority.attempt_id == input.attempt_id
            && guard.document_id == input.document_id,
        "graph-refresh publication guard identity mismatch"
    );
    let mut transaction = guard.into_transaction();
    let observed_source_truth_version =
        lock_library_publication_source(&mut transaction, input.workspace_id, input.library_id)
            .await?;
    publish_graph_refresh_success_in_transaction(
        transaction,
        input,
        effects,
        observed_source_truth_version,
        true,
    )
    .await
}

async fn publish_graph_refresh_success_in_transaction(
    mut transaction: Transaction<'static, Postgres>,
    input: &PublishGraphRefreshSuccess,
    effects: &GraphRefreshPublicationEffects,
    observed_source_truth_version: i64,
    graph_guarded: bool,
) -> anyhow::Result<PublishGraphRefreshSuccessOutcome> {
    let job = super::content_success::lock_publication_job(
        &mut transaction,
        input.attempt_id,
        true,
        "lock graph-refresh publication job",
    )
    .await?;
    let Some(job) = job else {
        finish_graph_refresh_authority_lost(
            transaction,
            graph_guarded,
            "roll back stale guarded graph-refresh publication",
            "commit stale graph-refresh publication no-op",
        )
        .await?;
        return Ok(PublishGraphRefreshSuccessOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    };
    ensure_graph_refresh_job_matches_input(&job, input)?;

    // Uniform stage writer order: job -> stage-event advisory -> attempt.
    // Ordinary progress appenders acquire the same advisory before their
    // predicate-bound attempt update.
    let authoritative_attempt = super::content_success::lock_authoritative_publication_attempt(
        &mut transaction,
        input.attempt_id,
        job.id,
        "serialize graph-refresh publication stage events",
        "lock latest graph-refresh attempt",
    )
    .await?;
    if !authoritative_attempt {
        finish_graph_refresh_authority_lost(
            transaction,
            graph_guarded,
            "roll back superseded guarded graph-refresh publication",
            "commit superseded graph-refresh lease no-op",
        )
        .await?;
        return Ok(PublishGraphRefreshSuccessOutcome::AuthorityLost {
            source_truth_version: observed_source_truth_version,
        });
    }

    let head = content_document_head_repository::get_document_head_for_update_with_executor(
        &mut *transaction,
        input.document_id,
    )
    .await
    .context("lock graph-refresh document head")?
    .context("graph-refresh document head disappeared")?;
    if head.active_revision_id != Some(input.revision_id)
        || head.readable_revision_id != Some(input.revision_id)
    {
        if graph_guarded {
            // A guarded mismatch must never commit graph/evidence writes from
            // the long transaction. Exact-head acquisition makes this branch
            // defensive, but rolling back fails closed if an invariant is
            // ever weakened later.
            transaction
                .rollback()
                .await
                .context("roll back superseded guarded graph-refresh publication")?;
            return Ok(PublishGraphRefreshSuccessOutcome::AuthorityLost {
                source_truth_version: observed_source_truth_version,
            });
        }
        return abandon_superseded_graph_refresh(
            transaction,
            input,
            effects,
            job.id,
            observed_source_truth_version,
        )
        .await;
    }

    validate_prepared_structured_revision_for_publication(
        &mut transaction,
        input.workspace_id,
        input.library_id,
        input.document_id,
        input.revision_id,
    )
    .await?;

    publish_graph_refresh_readiness(&mut transaction, input).await?;
    super::refresh_runtime_graph_provenance_coverage_for_publication(
        &mut transaction,
        input.library_id,
        input.document_id,
        input.revision_id,
    )
    .await?;

    publish_graph_refresh_completion(&mut transaction, input, &job, effects).await?;

    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        input.library_id,
        observed_source_truth_version,
    )
    .await?;
    transaction.commit().await.context("commit graph-refresh publication")?;
    Ok(PublishGraphRefreshSuccessOutcome::Applied { source_truth_version })
}

/// Commits or rolls back a lost-authority graph-refresh transaction according to
/// the guard: a guarded caller rolls back so no graph/evidence write can commit,
/// an unguarded caller commits the read-only no-op.
async fn finish_graph_refresh_authority_lost(
    transaction: Transaction<'static, Postgres>,
    graph_guarded: bool,
    rollback_context: &'static str,
    commit_context: &'static str,
) -> anyhow::Result<()> {
    if graph_guarded {
        transaction.rollback().await.context(rollback_context)?;
    } else {
        transaction.commit().await.context(commit_context)?;
    }
    Ok(())
}

/// The locked job must be the exact graph-refresh maintenance job named by the
/// input: same workspace/library/kind, no mutation, item, or async operation, and
/// the exact document and revision.
fn ensure_graph_refresh_job_matches_input(
    job: &PublicationJobAuthorityRow,
    input: &PublishGraphRefreshSuccess,
) -> anyhow::Result<()> {
    ensure_graph_refresh_job_is_maintenance(job, input)?;
    ensure_graph_refresh_job_targets(job, input)
}

/// Proves the job carries no content-mutation ownership fields.
fn ensure_graph_refresh_job_is_maintenance(
    job: &PublicationJobAuthorityRow,
    input: &PublishGraphRefreshSuccess,
) -> anyhow::Result<()> {
    anyhow::ensure!(job.workspace_id == input.workspace_id, "graph-refresh workspace mismatch");
    anyhow::ensure!(job.library_id == input.library_id, "graph-refresh library mismatch");
    anyhow::ensure!(job.mutation_id.is_none(), "graph-refresh job unexpectedly owns a mutation");
    anyhow::ensure!(
        job.mutation_item_id.is_none(),
        "graph-refresh job unexpectedly owns a mutation item"
    );
    anyhow::ensure!(
        job.async_operation_id.is_none(),
        "graph-refresh maintenance job unexpectedly owns an async operation"
    );
    anyhow::ensure!(job.job_kind == "graph_refresh", "graph-refresh job kind mismatch");
    Ok(())
}

/// Proves the job points at the exact document and revision under publication.
fn ensure_graph_refresh_job_targets(
    job: &PublicationJobAuthorityRow,
    input: &PublishGraphRefreshSuccess,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        job.knowledge_document_id == Some(input.document_id),
        "graph-refresh document mismatch"
    );
    anyhow::ensure!(
        job.knowledge_revision_id == Some(input.revision_id),
        "graph-refresh revision mismatch"
    );
    Ok(())
}

/// Publishes the graph-ready readiness for the exact revision, document, and
/// library.
async fn publish_graph_refresh_readiness(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishGraphRefreshSuccess,
) -> anyhow::Result<()> {
    let readiness = sqlx::query(
        "update knowledge_revision
         set graph_state = $4,
             graph_ready_at = $5
         where revision_id = $1
           and document_id = $2
           and library_id = $3",
    )
    .bind(input.revision_id)
    .bind(input.document_id)
    .bind(input.library_id)
    .bind(&input.graph_state)
    .bind(input.graph_ready_at)
    .execute(&mut **transaction)
    .await
    .context("publish graph-refresh readiness")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "graph-refresh revision disappeared");
    Ok(())
}

/// Completes the graph-refresh attempt with its terminal stage events and closes
/// the leased job. The job must still be `leased` with no mutation ownership.
async fn publish_graph_refresh_completion(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishGraphRefreshSuccess,
    job: &PublicationJobAuthorityRow,
    effects: &GraphRefreshPublicationEffects,
) -> anyhow::Result<()> {
    super::content_success::complete_publication_attempt_with_events(
        transaction,
        super::content_success::PublicationAttemptCompletion {
            attempt_id: input.attempt_id,
            job_id: job.id,
            completed_at: input.completed_at,
            graph_state: &input.graph_state,
        },
        &effects.terminal_stage_events,
        super::content_success::PublicationCompletionLabels {
            complete_context: "complete graph-refresh attempt",
            lease_lost_message: "graph-refresh attempt lost its lease",
            event_mismatch_message: "graph-refresh terminal stage event attempt mismatch",
            append_event_context: "append graph-refresh terminal stage event",
        },
    )
    .await?;
    let job_update = sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = $2,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'
           and mutation_id is null
           and mutation_item_id is null
           and knowledge_document_id = $3
           and knowledge_revision_id = $4",
    )
    .bind(job.id)
    .bind(input.completed_at)
    .bind(input.document_id)
    .bind(input.revision_id)
    .execute(&mut **transaction)
    .await
    .context("complete graph-refresh job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "graph-refresh job lost its lease");
    Ok(())
}

/// Terminalizes a graph-refresh attempt whose revision is no longer the active
/// readable head: abandons the still-leased attempt, appends its terminal stage
/// events, completes the leased job, and commits. The guarded caller rolls back
/// before this point, so the commit here is the unguarded superseded outcome.
async fn abandon_superseded_graph_refresh(
    mut transaction: Transaction<'static, Postgres>,
    input: &PublishGraphRefreshSuccess,
    effects: &GraphRefreshPublicationEffects,
    job_id: Uuid,
    observed_source_truth_version: i64,
) -> anyhow::Result<PublishGraphRefreshSuccessOutcome> {
    let attempt = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'abandoned',
             current_stage = 'finalizing',
             heartbeat_at = $2,
             finished_at = $2,
             failure_class = 'lifecycle',
             failure_code = 'revision_superseded',
             failure_message = 'graph refresh revision is no longer the active readable head',
             retryable = false
         where id = $1
           and job_id = $3
           and attempt_state = 'leased'",
    )
    .bind(input.attempt_id)
    .bind(input.completed_at)
    .bind(job_id)
    .execute(&mut *transaction)
    .await
    .context("abandon superseded graph-refresh attempt")?;
    anyhow::ensure!(attempt.rows_affected() == 1, "graph-refresh attempt lost its lease");
    for event in &effects.terminal_stage_events {
        anyhow::ensure!(
            event.attempt_id == input.attempt_id,
            "superseded graph-refresh terminal stage event attempt mismatch"
        );
        crate::infra::repositories::ingest_repository::append_ingest_stage_event_with_connection(
            &mut transaction,
            event,
        )
        .await
        .context("append superseded graph-refresh terminal stage event")?;
    }
    let job_update = sqlx::query(
        "update ingest_job
         set queue_state = 'completed',
             completed_at = $2,
             queue_leased_at = null,
             queue_lease_token = null,
             queue_lease_owner = null
         where id = $1
           and queue_state = 'leased'",
    )
    .bind(job_id)
    .bind(input.completed_at)
    .execute(&mut *transaction)
    .await
    .context("complete superseded graph-refresh job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "graph-refresh job lost its lease");
    transaction.commit().await.context("commit superseded graph-refresh publication")?;
    Ok(PublishGraphRefreshSuccessOutcome::Superseded {
        source_truth_version: observed_source_truth_version,
    })
}

#[cfg(test)]
mod graph_refresh_job_validation_tests {
    use super::*;

    fn input() -> PublishGraphRefreshSuccess {
        PublishGraphRefreshSuccess {
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            document_id: Uuid::from_u128(3),
            revision_id: Uuid::from_u128(4),
            attempt_id: Uuid::from_u128(5),
            graph_state: "ready".to_string(),
            graph_ready_at: None,
            completed_at: chrono::Utc::now(),
        }
    }

    fn job() -> PublicationJobAuthorityRow {
        PublicationJobAuthorityRow {
            id: Uuid::from_u128(5),
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            mutation_id: None,
            mutation_item_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(Uuid::from_u128(3)),
            knowledge_revision_id: Some(Uuid::from_u128(4)),
            job_kind: "graph_refresh".to_string(),
        }
    }

    #[test]
    fn maintenance_job_for_the_exact_target_is_accepted() {
        assert!(ensure_graph_refresh_job_matches_input(&job(), &input()).is_ok());
    }

    #[test]
    fn maintenance_job_owning_mutation_state_is_rejected() {
        let mut with_mutation = job();
        with_mutation.mutation_id = Some(Uuid::from_u128(9));
        assert!(ensure_graph_refresh_job_matches_input(&with_mutation, &input()).is_err());
    }

    #[test]
    fn maintenance_job_pointing_at_a_foreign_document_is_rejected() {
        let mut foreign = job();
        foreign.knowledge_document_id = Some(Uuid::from_u128(9));
        assert!(ensure_graph_refresh_job_matches_input(&foreign, &input()).is_err());
    }

    #[test]
    fn non_graph_refresh_job_kind_is_rejected() {
        let mut wrong_kind = job();
        wrong_kind.job_kind = "content_mutation".to_string();
        assert!(ensure_graph_refresh_job_matches_input(&wrong_kind, &input()).is_err());
    }
}
