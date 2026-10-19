use super::*;

pub(super) async fn lock_publication_job(
    transaction: &mut Transaction<'_, Postgres>,
    attempt_id: Uuid,
    require_incomplete_job: bool,
    lock_context: &'static str,
) -> anyhow::Result<Option<PublicationJobAuthorityRow>> {
    sqlx::query_as::<_, PublicationJobAuthorityRow>(
        "select
            job.id,
            job.workspace_id,
            job.library_id,
            job.mutation_id,
            job.mutation_item_id,
            job.async_operation_id,
            job.knowledge_document_id,
            job.knowledge_revision_id,
            job.job_kind::text as job_kind
         from ingest_attempt as attempt
         join ingest_job as job on job.id = attempt.job_id
         where attempt.id = $1
           and job.queue_state = 'leased'
           and (not $2 or job.completed_at is null)
         for update of job",
    )
    .bind(attempt_id)
    .bind(require_incomplete_job)
    .fetch_optional(&mut **transaction)
    .await
    .context(lock_context)
}

pub(super) async fn lock_authoritative_publication_attempt(
    transaction: &mut Transaction<'_, Postgres>,
    attempt_id: Uuid,
    job_id: Uuid,
    stage_event_lock_context: &'static str,
    attempt_lock_context: &'static str,
) -> anyhow::Result<bool> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("ingest-stage-event:{attempt_id}"))
        .execute(&mut **transaction)
        .await
        .context(stage_event_lock_context)?;

    let authoritative_attempt_id = sqlx::query_scalar::<_, Uuid>(
        "select attempt.id
         from ingest_attempt as attempt
         where attempt.id = $1
           and attempt.job_id = $2
           and attempt.attempt_state = 'leased'
           and attempt.finished_at is null
           and not exists (
               select 1
               from ingest_attempt as newer
               where newer.job_id = attempt.job_id
                 and newer.attempt_number > attempt.attempt_number
           )
         for update of attempt",
    )
    .bind(attempt_id)
    .bind(job_id)
    .fetch_optional(&mut **transaction)
    .await
    .context(attempt_lock_context)?;
    Ok(authoritative_attempt_id == Some(attempt_id))
}

pub(super) struct PublicationCompletionLabels {
    pub(super) complete_context: &'static str,
    pub(super) lease_lost_message: &'static str,
    pub(super) event_mismatch_message: &'static str,
    pub(super) append_event_context: &'static str,
}

pub(super) struct PublicationAttemptCompletion<'a> {
    pub(super) attempt_id: Uuid,
    pub(super) job_id: Uuid,
    pub(super) completed_at: DateTime<Utc>,
    pub(super) graph_state: &'a str,
}

/// Completes a successful attempt while retaining a non-fatal graph outcome
/// on the attempt itself, independently of its stage-event details.
pub(super) async fn complete_publication_attempt_with_events(
    transaction: &mut Transaction<'_, Postgres>,
    completion: PublicationAttemptCompletion<'_>,
    terminal_stage_events: &[NewIngestStageEvent],
    labels: PublicationCompletionLabels,
) -> anyhow::Result<()> {
    let attempt = sqlx::query(
        "update ingest_attempt
         set attempt_state = 'succeeded',
             current_stage = 'finalizing',
             heartbeat_at = $2,
             finished_at = $2,
             failure_class = null,
             failure_code = case when $4 = 'graph_degraded' then $4 else null end,
             failure_message = null,
             progress_percent = 100,
             retryable = false
         where id = $1
           and job_id = $3
           and attempt_state = 'leased'",
    )
    .bind(completion.attempt_id)
    .bind(completion.completed_at)
    .bind(completion.job_id)
    .bind(completion.graph_state)
    .execute(&mut **transaction)
    .await
    .context(labels.complete_context)?;
    anyhow::ensure!(attempt.rows_affected() == 1, labels.lease_lost_message);

    for event in terminal_stage_events {
        anyhow::ensure!(event.attempt_id == completion.attempt_id, labels.event_mismatch_message);
        crate::infra::repositories::ingest_repository::append_ingest_stage_event_with_connection(
            &mut *transaction,
            event,
        )
        .await
        .context(labels.append_event_context)?;
    }
    Ok(())
}

/// Publishes revision readiness, current document head/projection, exact mutation
/// item, aggregate-safe mutation state, attempt/job completion, and durable
/// webhook handoff in one transaction with one source-generation advance.
pub async fn publish_content_ingest_success(
    postgres: &PgPool,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    publish_content_ingest_success_with_effects(
        postgres,
        input,
        &ContentIngestPublicationEffects::default(),
    )
    .await
}

pub async fn publish_content_ingest_success_with_effects(
    postgres: &PgPool,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    publish_content_ingest_success_with_profile_source(postgres, input, effects, None).await
}

/// Revision-scoped publication used by product ingest pipelines. Unlike the
/// strict adapter/test entry point, unrelated library source advances are
/// accepted after the active embedding profile is checked under the lock.
pub async fn publish_content_ingest_success_revision_fenced(
    postgres: &PgPool,
    input: &PublishContentIngestSuccess,
    active_profile_source: &dyn ActiveEmbeddingProfileSource,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    publish_content_ingest_success_revision_fenced_with_effects(
        postgres,
        input,
        &ContentIngestPublicationEffects::default(),
        active_profile_source,
    )
    .await
}

pub async fn publish_content_ingest_success_revision_fenced_with_effects(
    postgres: &PgPool,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
    active_profile_source: &dyn ActiveEmbeddingProfileSource,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    publish_content_ingest_success_with_profile_source(
        postgres,
        input,
        effects,
        Some(active_profile_source),
    )
    .await
}

/// Publishes content ingest while consuming the transaction that staged graph
/// reconcile. Control-plane authority may move while reconcile runs; this path
/// locks and revalidates it, rolling back every staged write if it moved first.
pub async fn publish_content_ingest_success_revision_fenced_with_graph_guard(
    guard: IngestRevisionGraphPublicationGuard,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    anyhow::ensure!(
        guard.authority.workspace_id == input.workspace_id
            && guard.authority.library_id == input.library_id
            && guard.authority.revision_id == input.revision_id
            && guard.authority.attempt_id == input.attempt_id
            && guard.document_id == input.document_id,
        "content-ingest graph publication guard identity mismatch"
    );
    ensure_valid_content_publication_fence(input)?;
    let mut transaction = guard.into_transaction();
    // The guard already holds a library KEY SHARE lifetime lock. Acquiring
    // the vector advisory lock here would invert the ordinary
    // advisory -> library order and could deadlock with another publisher.
    // Upgrade the already-held parent row instead: every vector writer locks
    // this row before touching vector data, so FOR UPDATE both observes all
    // prior writers and excludes new ones through publication commit.
    let observed_source_truth_version = sqlx::query_scalar::<_, i64>(
        "select source_truth_version
         from catalog_library
         where id = $1 and workspace_id = $2
         for update",
    )
    .bind(input.library_id)
    .bind(input.workspace_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("upgrade guarded content-ingest publication source lock")?
    .context("guarded content-ingest publication library disappeared")?;
    let observed_embedding_profile_key =
        active_embedding_profile_key_with_transaction(&mut transaction, input.library_id).await?;
    publish_content_ingest_success_in_transaction(
        transaction,
        input,
        effects,
        RevisionPublicationProfileFence::Observed(observed_embedding_profile_key),
        observed_source_truth_version,
        true,
    )
    .await
}

fn ensure_valid_content_publication_fence(
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        input.expected_source_truth_version > 0,
        "content-ingest publication requires a positive source-truth fence"
    );
    Ok(())
}

async fn publish_content_ingest_success_with_profile_source(
    postgres: &PgPool,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
    active_profile_source: Option<&dyn ActiveEmbeddingProfileSource>,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    ensure_valid_content_publication_fence(input)?;

    let mut transaction = postgres.begin().await.context("begin content-ingest publication")?;
    let observed_source_truth_version =
        lock_exclusive_vector_mutation_source(&mut transaction, input.library_id).await?;
    publish_content_ingest_success_in_transaction(
        transaction,
        input,
        effects,
        active_profile_source.map_or(
            RevisionPublicationProfileFence::StrictSourceVersion,
            RevisionPublicationProfileFence::External,
        ),
        observed_source_truth_version,
        false,
    )
    .await
}

/// Close a transaction that turned out to have lost publication authority.
///
/// A guarded publication has already written graph rows in this transaction,
/// so losing authority has to roll them back; an unguarded one wrote nothing
/// and commits, which releases the row locks without leaving the caller to
/// guess whether a rollback is pending.
async fn finish_lost_authority_transaction(
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

/// Prove the locked job is the one this request claims to be publishing.
///
/// The lock was taken through the attempt identity, so every other identity on
/// the job is still unverified at that point: a mismatch means two publication
/// paths raced onto one attempt and one of them is about to write another
/// document's readiness.
fn ensure_publication_job_matches_request(
    job: &PublicationJobAuthorityRow,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<()> {
    anyhow::ensure!(job.workspace_id == input.workspace_id, "publication workspace mismatch");
    anyhow::ensure!(job.library_id == input.library_id, "publication library mismatch");
    anyhow::ensure!(job.mutation_id == Some(input.mutation_id), "publication mutation mismatch");
    anyhow::ensure!(
        job.mutation_item_id == Some(input.mutation_item_id),
        "publication mutation item mismatch"
    );
    anyhow::ensure!(
        job.knowledge_document_id == Some(input.document_id),
        "publication document mismatch"
    );
    anyhow::ensure!(
        job.knowledge_revision_id == Some(input.revision_id),
        "publication revision mismatch"
    );
    anyhow::ensure!(job.job_kind == "content_mutation", "publication job kind mismatch");
    Ok(())
}

async fn publish_content_ingest_success_in_transaction(
    mut transaction: Transaction<'static, Postgres>,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
    profile_fence: RevisionPublicationProfileFence<'_>,
    observed_source_truth_version: i64,
    graph_guarded: bool,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    // Required lock order is library -> job -> attempt. Lock the job through
    // the supplied attempt identity first, then lock and prove that exact
    // attempt is still the newest lease for the job.
    let job = match lock_content_publication_authority(&mut transaction, input).await? {
        ContentAuthority::Lost { rollback_context, commit_context } => {
            finish_lost_authority_transaction(
                transaction,
                graph_guarded,
                rollback_context,
                commit_context,
            )
            .await?;
            return Ok(PublishContentIngestSuccessOutcome::AuthorityLost {
                source_truth_version: observed_source_truth_version,
            });
        }
        ContentAuthority::Locked(job) => job,
    };

    validate_content_publication_inputs(
        &mut transaction,
        input,
        &profile_fence,
        observed_source_truth_version,
    )
    .await?;

    let document =
        crate::infra::repositories::content_document_repository::lock_document_by_id_with_executor(
            &mut *transaction,
            input.document_id,
        )
        .await?
        .context("publication document disappeared")?;
    anyhow::ensure!(
        document.workspace_id == input.workspace_id && document.library_id == input.library_id,
        "publication document scope mismatch"
    );
    content_document_head_repository::get_document_head_for_update_with_executor(
        &mut *transaction,
        input.document_id,
    )
    .await?
    .context("publication head disappeared")?;

    publish_content_revision_readiness(&mut transaction, input).await?;

    lock_content_mutation_scope(&mut transaction, input).await?;
    apply_content_mutation_item(&mut transaction, input).await?;
    let mutation_completed = complete_content_aggregate(&mut transaction, input).await?;

    let publicly_visible = content_document_head_repository::publication_item_is_visible(
        &mut transaction,
        input.document_id,
        input.revision_id,
        input.mutation_item_id,
    )
    .await?;
    if publicly_visible {
        publish_content_document_head(&mut transaction, input, effects).await?;
    }

    let source_truth_version = finalize_content_publication(
        &mut transaction,
        input,
        &job,
        effects,
        ContentPublicationDecision { mutation_completed, publicly_visible },
        observed_source_truth_version,
    )
    .await?;

    transaction.commit().await.context("commit content-ingest publication")?;

    Ok(PublishContentIngestSuccessOutcome::Applied { source_truth_version, mutation_completed })
}

/// Authority for one content-ingest publication. `Lost` carries the exact
/// rollback/commit contexts the caller must feed to
/// `finish_lost_authority_transaction`; `Locked` carries the revalidated job.
enum ContentAuthority {
    Lost { rollback_context: &'static str, commit_context: &'static str },
    Locked(PublicationJobAuthorityRow),
}

/// Locks the exact content-mutation job through the attempt identity and proves
/// the exact attempt is still the newest lease. Lock order is library -> job ->
/// attempt; the caller owns the transaction finish for a lost-authority no-op.
async fn lock_content_publication_authority(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<ContentAuthority> {
    let job = lock_publication_job(
        transaction,
        input.attempt_id,
        true,
        "lock content-ingest publication job",
    )
    .await?;
    let Some(job) = job else {
        return Ok(ContentAuthority::Lost {
            rollback_context: "roll back stale guarded content-ingest publication",
            commit_context: "commit stale content-ingest publication no-op",
        });
    };

    ensure_publication_job_matches_request(&job, input)?;

    // Shared lock order with ordinary stage writers is job -> stage-event
    // advisory -> attempt row. Taking the attempt row first would deadlock an
    // ordinary writer that already owns the advisory and is about to update
    // attempt progress.
    let authoritative_attempt = lock_authoritative_publication_attempt(
        transaction,
        input.attempt_id,
        job.id,
        "serialize content publication stage events",
        "lock latest content-ingest publication attempt",
    )
    .await?;
    if !authoritative_attempt {
        return Ok(ContentAuthority::Lost {
            rollback_context: "roll back superseded guarded content-ingest publication",
            commit_context: "commit superseded content-ingest publication no-op",
        });
    }
    Ok(ContentAuthority::Locked(job))
}

/// Revalidates the embedding-profile/source fence and the prepared structured
/// revision and vector coverage before any readiness write. All checks share the
/// already-held library, job, and attempt locks.
async fn validate_content_publication_inputs(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
    profile_fence: &RevisionPublicationProfileFence<'_>,
    observed_source_truth_version: i64,
) -> anyhow::Result<()> {
    validate_publication_profile_and_source_fence(
        profile_fence,
        input,
        observed_source_truth_version,
    )
    .await?;

    validate_prepared_structured_revision_for_publication(
        transaction,
        input.workspace_id,
        input.library_id,
        input.document_id,
        input.revision_id,
    )
    .await?;

    validate_ingest_revision_vector_coverage(
        transaction,
        input.library_id,
        input.revision_id,
        input.embedding_profile_key.as_deref(),
    )
    .await
}

/// Publishes revision readiness for the exact revision and library.
async fn publish_content_revision_readiness(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<()> {
    let readiness = sqlx::query(
        "update knowledge_revision
         set text_state = $3,
             vector_state = 'ready',
             graph_state = $4,
             text_readable_at = $5,
             vector_ready_at = coalesce(vector_ready_at, $7),
             graph_ready_at = $6
         where revision_id = $1
           and library_id = $2",
    )
    .bind(input.revision_id)
    .bind(input.library_id)
    .bind(&input.text_state)
    .bind(&input.graph_state)
    .bind(input.text_readable_at)
    .bind(input.graph_ready_at)
    .bind(input.completed_at)
    .execute(&mut **transaction)
    .await
    .context("publish content-ingest revision readiness")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "publication revision disappeared");
    Ok(())
}

/// Locks the aggregate mutation and the exact mutation item and proves both
/// belong to the input, then requires the item to be `pending` or `applied`.
async fn lock_content_mutation_scope(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<()> {
    let mutation = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id
         from content_mutation
         where id = $1
         for update",
    )
    .bind(input.mutation_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock content-ingest publication mutation")?
    .context("publication mutation disappeared")?;
    anyhow::ensure!(mutation.0 == input.workspace_id, "publication mutation workspace mismatch");
    anyhow::ensure!(mutation.1 == input.library_id, "publication mutation library mismatch");

    let item = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>, String)>(
        "select mutation_id, document_id, result_revision_id, item_state::text
         from content_mutation_item
         where id = $1
         for update",
    )
    .bind(input.mutation_item_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock exact content-ingest mutation item")?
    .context("publication mutation item disappeared")?;
    ensure_publication_item_matches_input(&item, input)
}

/// The locked item must belong to the publication input's mutation, document,
/// and revision, and may only be `pending` or `applied`.
fn ensure_publication_item_matches_input(
    item: &(Uuid, Option<Uuid>, Option<Uuid>, String),
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<()> {
    anyhow::ensure!(item.0 == input.mutation_id, "publication item mutation mismatch");
    anyhow::ensure!(item.1 == Some(input.document_id), "publication item document mismatch");
    anyhow::ensure!(item.2 == Some(input.revision_id), "publication item result revision mismatch");
    anyhow::ensure!(
        item.3 == "pending" || item.3 == "applied",
        "publication item is already terminal with incompatible state {}",
        item.3
    );
    Ok(())
}

/// Marks the exact mutation item applied.
async fn apply_content_mutation_item(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<()> {
    let item_update = sqlx::query(
        "update content_mutation_item
         set item_state = 'applied',
             message = 'mutation applied by ingest publication'
         where id = $1
           and mutation_id = $2
           and document_id = $3
           and result_revision_id = $4",
    )
    .bind(input.mutation_item_id)
    .bind(input.mutation_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .execute(&mut **transaction)
    .await
    .context("apply exact content-ingest mutation item")?;
    anyhow::ensure!(item_update.rows_affected() == 1, "publication item identity changed");
    Ok(())
}

/// Classifies web-run aggregate ownership and completes the ordinary aggregate
/// mutation when this attempt applied the last acceptable item. Returns whether
/// the mutation is completed after this call.
async fn complete_content_aggregate(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<bool> {
    // Web run orchestration owns its aggregate mutation because child page
    // ingests can finish independently. Ordinary mutations complete only once
    // every item is an acceptable success; no first-item shortcut is valid.
    let web_run_owns_aggregate = sqlx::query_scalar::<_, bool>(
        "select exists (
            select 1
            from content_web_ingest_run
            where mutation_id = $1
         )",
    )
    .bind(input.mutation_id)
    .fetch_one(&mut **transaction)
    .await
    .context("classify web-owned mutation aggregate")?;
    complete_aggregate_mutation_if_ready(transaction, input, web_run_owns_aggregate).await
}

/// Publishes the exact mutation's head and its own summary, including absent output.
async fn publish_content_document_head(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
) -> anyhow::Result<()> {
    let previous_head =
        content_document_head_repository::get_document_head_for_update_with_executor(
            &mut **transaction,
            input.document_id,
        )
        .await
        .context("lock content document head for ingest publication")?
        .context("content document head disappeared before ingest publication")?;
    content_document_head_repository::upsert_document_head_without_generation_with_executor(
        &mut **transaction,
        &NewContentDocumentHead {
            publishing_mutation_item_id: Some(input.mutation_item_id),
            document_id: input.document_id,
            active_revision_id: Some(input.revision_id),
            readable_revision_id: Some(input.revision_id),
            latest_mutation_id: previous_head.latest_mutation_id,
            latest_successful_attempt_id: Some(input.attempt_id),
        },
    )
    .await
    .context("publish content document head")?;
    let document_summary =
        effects.document_summary.as_deref().map(str::trim).filter(|summary| !summary.is_empty());
    let summary = sqlx::query(
        "update content_document_head set document_summary = $2
          where document_id = $1 and readable_revision_id = $3",
    )
    .bind(input.document_id)
    .bind(document_summary)
    .bind(input.revision_id)
    .execute(&mut **transaction)
    .await
    .context("publish content document summary")?;
    anyhow::ensure!(summary.rows_affected() == 1, "published content document head changed");
    Ok(())
}

/// Completion and visibility are independent: a successful superseded payload
/// completes its work but must not publish its source projection.
struct ContentPublicationDecision {
    mutation_completed: bool,
    publicly_visible: bool,
}

/// Refreshes graph provenance, materializes the knowledge projection, completes
/// the publication attempt and job, transitions the async operation, emits the
/// revision-ready lifecycle event, and advances the source-truth version.
/// Returns the advanced source-truth version.
async fn finalize_content_publication(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
    job: &PublicationJobAuthorityRow,
    effects: &ContentIngestPublicationEffects,
    decision: ContentPublicationDecision,
    observed_source_truth_version: i64,
) -> anyhow::Result<i64> {
    if decision.publicly_visible {
        super::refresh_runtime_graph_provenance_coverage_for_publication(
            transaction,
            input.library_id,
            input.document_id,
            input.revision_id,
        )
        .await?;
        let materialized =
        content_knowledge_projection_repository::materialize_knowledge_document_from_current_head_with_transaction(
            transaction,
            input.document_id,
        )
        .await
        .context("materialize content-ingest knowledge projection")?;
        anyhow::ensure!(
            materialized.outcome == MaterializeKnowledgeDocumentOutcome::Materialized,
            "content-ingest projection could not materialize: {:?}",
            materialized.outcome
        );
    }

    complete_publication_attempt_with_events(
        transaction,
        PublicationAttemptCompletion {
            attempt_id: input.attempt_id,
            job_id: job.id,
            completed_at: input.completed_at,
            graph_state: &input.graph_state,
        },
        &effects.terminal_stage_events,
        PublicationCompletionLabels {
            complete_context: "complete content-ingest attempt",
            lease_lost_message: "publication attempt lost its lease",
            event_mismatch_message: "content publication terminal stage event attempt mismatch",
            append_event_context: "append content publication terminal stage event",
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
           and workspace_id = $3
           and library_id = $4
           and mutation_id = $5
           and mutation_item_id = $6
           and knowledge_document_id = $7
           and knowledge_revision_id = $8",
    )
    .bind(job.id)
    .bind(input.completed_at)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .bind(input.mutation_item_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .execute(&mut **transaction)
    .await
    .context("complete content-ingest job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "publication job lost its lease");

    if decision.mutation_completed
        && let Some(async_operation_id) = job.async_operation_id
    {
        complete_content_mutation_operation(transaction, input, async_operation_id).await?;
    }

    let event = WebhookEvent {
        event_type: "revision.ready".to_string(),
        event_id: revision_ready_event_id(input.revision_id),
        occurred_at: input.completed_at,
        workspace_id: input.workspace_id,
        library_id: input.library_id,
        payload_json: serde_json::json!({
            "document_id": input.document_id,
            "revision_id": input.revision_id,
            "library_id": input.library_id,
        }),
    };
    webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(
        &mut **transaction,
        &event,
    )
    .await
    .context("enqueue deterministic revision-ready lifecycle event")?;

    advance_vector_source_truth_version(
        transaction,
        input.library_id,
        observed_source_truth_version,
    )
    .await
}

/// Completes the linked content-mutation async operation for a publication that
/// completed its aggregate mutation. The exact operation identity row must
/// exist and still be owned by the publication's mutation.
async fn complete_content_mutation_operation(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
    async_operation_id: Uuid,
) -> anyhow::Result<()> {
    let operation = sqlx::query(
        "update ops_async_operation
             set status = 'ready',
                 completed_at = $2,
                 failure_code = null
             where id = $1
               and workspace_id = $3
               and library_id = $4
               and subject_kind = 'content_mutation'
               and subject_id = $5",
    )
    .bind(async_operation_id)
    .bind(input.completed_at)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.mutation_id)
    .execute(&mut **transaction)
    .await
    .context("complete linked content-mutation async operation")?;
    anyhow::ensure!(
        operation.rows_affected() == 1,
        "linked content-mutation async operation identity changed"
    );
    Ok(())
}

/// Revalidates the embedding-profile and source-generation fence for a guarded
/// content-ingest publication. A strict fence requires the observed generation
/// to equal the expected one; a guarded fence requires it to be a monotonic
/// lower bound and, when the caller names an embedding profile, requires the
/// active profile to still match.
async fn validate_publication_profile_and_source_fence(
    profile_fence: &RevisionPublicationProfileFence<'_>,
    input: &PublishContentIngestSuccess,
    observed_source_truth_version: i64,
) -> anyhow::Result<()> {
    if matches!(profile_fence, RevisionPublicationProfileFence::StrictSourceVersion) {
        anyhow::ensure!(
            observed_source_truth_version == input.expected_source_truth_version,
            "library source or embedding profile changed before content-ingest publication"
        );
        return Ok(());
    }
    if let Some(expected_embedding_profile_key) = input.embedding_profile_key.as_deref() {
        let active_embedding_profile_key = match profile_fence {
            RevisionPublicationProfileFence::External(active_profile_source) => {
                active_profile_source.active_embedding_profile_key(input.library_id).await.context(
                    "resolve active embedding profile under content-ingest publication fence",
                )?
            }
            RevisionPublicationProfileFence::Observed(observed) => observed
                .clone()
                .context("active embedding profile disappeared before guarded publication")?,
            RevisionPublicationProfileFence::StrictSourceVersion => unreachable!(),
        };
        anyhow::ensure!(
            active_embedding_profile_key == expected_embedding_profile_key,
            "library embedding profile changed before content-ingest publication"
        );
    }
    // The locked job/attempt authority and document-head check below fence
    // this revision, so unrelated documents may advance the library-wide
    // cache generation while this exact revision is embedding.
    anyhow::ensure!(
        observed_source_truth_version >= input.expected_source_truth_version,
        "library source version moved backwards before content-ingest publication"
    );
    Ok(())
}

/// Completes the owning aggregate mutation when this attempt applied the last
/// acceptable item. A web-run-owned aggregate is never completed here: the run
/// orchestration closes it once every page settles. Returns whether the
/// aggregate mutation is completed after this call.
async fn complete_aggregate_mutation_if_ready(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
    web_run_owns_aggregate: bool,
) -> anyhow::Result<bool> {
    if web_run_owns_aggregate {
        return Ok(false);
    }
    let all_items_acceptable = sqlx::query_scalar::<_, bool>(
        "select not exists (
            select 1
            from content_mutation_item
            where mutation_id = $1
              and item_state not in ('applied', 'skipped')
         )",
    )
    .bind(input.mutation_id)
    .fetch_one(&mut **transaction)
    .await
    .context("check aggregate mutation completion")?;
    if all_items_acceptable {
        let aggregate = sqlx::query(
            "update content_mutation
             set mutation_state = 'applied',
                 completed_at = $2,
                 failure_code = null,
                 conflict_code = null
             where id = $1",
        )
        .bind(input.mutation_id)
        .bind(input.completed_at)
        .execute(&mut **transaction)
        .await
        .context("complete aggregate content mutation")?;
        anyhow::ensure!(aggregate.rows_affected() == 1, "publication mutation disappeared");
    }
    Ok(all_items_acceptable)
}

enum RevisionPublicationProfileFence<'a> {
    StrictSourceVersion,
    External(&'a dyn ActiveEmbeddingProfileSource),
    Observed(Option<String>),
}

async fn active_embedding_profile_key_with_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> anyhow::Result<Option<String>> {
    let row = crate::infra::repositories::ai_repository::get_effective_embedding_profile_with_transaction(
        transaction,
        library_id,
    )
    .await
    .context("resolve active embedding profile on guarded publication transaction")?;
    row.map(crate::services::ai_catalog_service::embedding_profile_key_from_effective_row)
        .transpose()
        .map_err(anyhow::Error::from)
}
