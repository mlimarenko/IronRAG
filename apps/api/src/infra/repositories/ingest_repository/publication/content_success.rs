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
    /// The one non-fatal degradation code the attempt row may retain. Any other
    /// value (including an empty one) leaves `failure_code` null.
    pub(super) attempt_degradation_code: &'a str,
}

/// Completes a successful attempt while retaining a non-fatal enrichment
/// outcome on the attempt itself, independently of its stage-event details.
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
    .bind(completion.attempt_degradation_code)
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
    let request = PublicationRequest::from(input);
    // The guard already holds a library KEY SHARE lifetime lock. Acquiring
    // the vector advisory lock here would invert the ordinary
    // advisory -> library order and could deadlock with another publisher.
    // Upgrade the already-held parent row instead: every vector writer locks
    // this row before touching vector data, so FOR UPDATE both observes all
    // prior writers and excludes new ones through publication commit.
    let observed_source_truth_version =
        lock_guarded_publication_source(&mut transaction, &request).await?;
    let observed_embedding_profile_key =
        active_embedding_profile_key_with_transaction(&mut transaction, input.library_id).await?;
    publish_content_ingest_success_in_transaction(
        transaction,
        input,
        effects,
        RevisionPublicationProfileFence::Observed(observed_embedding_profile_key),
        observed_source_truth_version,
        ContentGraphPublication::Staged,
    )
    .await
}

async fn lock_guarded_publication_source(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "select source_truth_version
         from catalog_library
         where id = $1 and workspace_id = $2
         for update",
    )
    .bind(request.library_id)
    .bind(request.workspace_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("upgrade guarded content-ingest publication source lock")?
    .context("guarded content-ingest publication library disappeared")
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

/// Permitted base publication of one prepared revision.
///
/// It publishes only text/structure/lexical availability: the same readable head,
/// the exact mutation item and the ordinary aggregate. It deliberately carries no
/// embedding profile, vector coverage or graph outcome, because none of those
/// exists yet and none may gate the document's readability. A publication that
/// the visibility-intent decision withholds completes its work here and skips
/// enrichment entirely: there is nothing left to enrich for a revision that can
/// never be exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishContentIngestBase {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Uuid,
    pub attempt_id: Uuid,
    pub completed_at: DateTime<Utc>,
}

/// Optional enrichment outcome of one already-published revision.
///
/// It records vector readiness (proved by exact chunk-vector coverage under the
/// already-held library and job locks) and the graph layer's own terminal state,
/// then completes the attempt and job. It never writes the document head,
/// mutation item, aggregate or async operation: those belong to base publication,
/// and a revision that stopped being current must not rewrite them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeContentIngestEnrichment {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_source_truth_version: i64,
    pub embedding_profile_key: Option<String>,
    pub graph_state: String,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub completed_at: DateTime<Utc>,
}

impl FinalizeContentIngestEnrichment {
    fn ensure_valid_fence(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.expected_source_truth_version > 0,
            "content-ingest enrichment finalization requires a positive source-truth fence"
        );
        Ok(())
    }
}

/// Publishes the prepared revision's allowed text as the document's readable
/// head, inside the canonical lifecycle and before any optional enrichment.
///
/// Every mutation, visibility-intent, revocation and supersession fence of the
/// one-transaction publication is reused unchanged; only the enrichment
/// requirements are deferred to [`finalize_content_ingest_enrichment`].
pub async fn publish_content_ingest_base(
    postgres: &PgPool,
    input: &PublishContentIngestBase,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    let request = PublicationRequest::from(input);
    let mut transaction =
        postgres.begin().await.context("begin content-ingest base publication")?;
    // Base publication writes no vectors, so it takes only the readable-head
    // library row lock the guarded publication already uses. It deliberately does
    // not take the exclusive vector-plane advisory lock: holding that lock would
    // queue text publication behind every in-flight embedding write in the
    // library, which is exactly the coupling this split removes.
    let observed_source_truth_version =
        lock_guarded_publication_source(&mut transaction, &request).await?;

    let job = match lock_content_publication_authority(&mut transaction, &request).await? {
        ContentAuthority::Lost { commit_context, .. } => {
            finish_lost_authority_transaction(
                transaction,
                false,
                "roll back stale content-ingest base publication",
                commit_context,
            )
            .await?;
            return Ok(PublishContentIngestSuccessOutcome::AuthorityLost {
                source_truth_version: observed_source_truth_version,
            });
        }
        ContentAuthority::Locked(job) => job,
    };

    // Base publication has no vector inventory to prove; the prepared structured
    // artifact is its whole contract.
    validate_prepared_structured_revision_for_publication(
        &mut transaction,
        request.workspace_id,
        request.library_id,
        request.document_id,
        request.revision_id,
    )
    .await?;

    let (decision, text_readable_at) =
        prepare_base_publication_decision(&mut transaction, input).await?;
    if decision.publicly_visible {
        publish_visible_base_revision(
            &mut transaction,
            &request,
            &job,
            input,
            text_readable_at,
            decision.mutation_completed,
        )
        .await?;
    }
    if !decision.publicly_visible {
        complete_withheld_base_revision(
            &mut transaction,
            &request,
            &job,
            input,
            text_readable_at,
            decision.mutation_completed,
        )
        .await?;
    }

    // Base publication is an answer-visible generation change: cached answers keyed
    // by the library generation must not outlive the revision that just became
    // readable, and a withheld revision still changed its own recorded text state.
    //
    // The advance is safe for unrelated in-flight work: ingest vector writes use
    // the revision-scoped monotonic fence (`observed >= expected`), so they are not
    // fenced out. Only whole-library compare-and-advance vector writers can see a
    // moved generation, which is their existing, documented retry contract - and
    // the row lock held since this transaction started prevents a torn read. The
    // enrichment finalization advances a second time for its own change.
    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        request.library_id,
        observed_source_truth_version,
    )
    .await?;
    transaction.commit().await.context("commit content-ingest base publication")?;

    Ok(PublishContentIngestSuccessOutcome::Applied {
        source_truth_version,
        mutation_completed: decision.mutation_completed,
        publicly_visible: decision.publicly_visible,
    })
}

/// Finalizes the optional enrichment of an already-published revision.
pub async fn finalize_content_ingest_enrichment(
    postgres: &PgPool,
    input: &FinalizeContentIngestEnrichment,
    effects: &ContentIngestPublicationEffects,
    active_profile_source: &dyn ActiveEmbeddingProfileSource,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    input.ensure_valid_fence()?;
    let mut transaction =
        postgres.begin().await.context("begin content-ingest enrichment finalization")?;
    let observed_source_truth_version =
        lock_exclusive_vector_mutation_source(&mut transaction, input.library_id).await?;
    finalize_content_ingest_enrichment_in_transaction(
        transaction,
        input,
        effects,
        RevisionPublicationProfileFence::External(active_profile_source),
        observed_source_truth_version,
        false,
    )
    .await
}

/// Finalizes enrichment while consuming the transaction that staged graph
/// reconcile. Control-plane authority may move while reconcile runs; this path
/// locks and revalidates it, rolling back every staged write if it moved first.
pub async fn finalize_content_ingest_enrichment_with_graph_guard(
    guard: IngestRevisionGraphPublicationGuard,
    input: &FinalizeContentIngestEnrichment,
    effects: &ContentIngestPublicationEffects,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    input.ensure_valid_fence()?;
    anyhow::ensure!(
        guard.authority.workspace_id == input.workspace_id
            && guard.authority.library_id == input.library_id
            && guard.authority.revision_id == input.revision_id
            && guard.authority.attempt_id == input.attempt_id
            && guard.document_id == input.document_id,
        "content-ingest graph publication guard identity mismatch"
    );
    let mut transaction = guard.into_transaction();
    let request = PublicationRequest::from(input);
    let observed_source_truth_version =
        lock_guarded_publication_source(&mut transaction, &request).await?;
    let observed_embedding_profile_key =
        active_embedding_profile_key_with_transaction(&mut transaction, request.library_id).await?;
    finalize_content_ingest_enrichment_in_transaction(
        transaction,
        input,
        effects,
        RevisionPublicationProfileFence::Observed(observed_embedding_profile_key),
        observed_source_truth_version,
        true,
    )
    .await
}

async fn finalize_content_ingest_enrichment_in_transaction(
    mut transaction: Transaction<'static, Postgres>,
    input: &FinalizeContentIngestEnrichment,
    effects: &ContentIngestPublicationEffects,
    profile_fence: RevisionPublicationProfileFence<'_>,
    observed_source_truth_version: i64,
    graph_guarded: bool,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    let request = PublicationRequest::from(input);
    let job = match lock_content_publication_authority(&mut transaction, &request).await? {
        ContentAuthority::Lost { commit_context, .. } => {
            finish_lost_authority_transaction(
                transaction,
                graph_guarded,
                "roll back superseded content-ingest enrichment finalization",
                commit_context,
            )
            .await?;
            return Ok(PublishContentIngestSuccessOutcome::AuthorityLost {
                source_truth_version: observed_source_truth_version,
            });
        }
        ContentAuthority::Locked(job) => job,
    };

    validate_publication_profile_and_source_fence(
        &profile_fence,
        request.library_id,
        input.expected_source_truth_version,
        input.embedding_profile_key.as_deref(),
        observed_source_truth_version,
    )
    .await?;
    validate_ingest_revision_vector_coverage(
        &mut transaction,
        request.library_id,
        request.revision_id,
        input.embedding_profile_key.as_deref(),
    )
    .await?;

    publish_enrichment_revision_states(
        &mut transaction,
        &request,
        &input.graph_state,
        input.graph_ready_at,
        input.completed_at,
    )
    .await?;
    publish_head_summary_if_current(
        &mut transaction,
        &request,
        effects.document_summary.as_deref(),
    )
    .await?;

    // Base publication already published this head; refreshing coverage here only
    // records the graph layer that enrichment just produced.
    if !graph_guarded {
        super::refresh_runtime_graph_provenance_coverage_for_publication(
            &mut transaction,
            request.library_id,
            request.document_id,
            request.revision_id,
        )
        .await?;
    }

    complete_content_ingest_lifecycle(
        &mut transaction,
        ContentIngestLifecycleCompletion {
            request: &request,
            job: &job,
            completed_at: input.completed_at,
            revision_ready_event_at: None,
            mutation_completed: false,
            attempt_degradation_code: &input.graph_state,
            terminal_stage_events: &effects.terminal_stage_events,
        },
    )
    .await?;

    // Vector readiness is its own answer-visible change, so it advances the
    // generation once for itself. Both advances are deliberate: every cached
    // answer invalidated by "the text is readable" and by "the vectors are ready"
    // is tied to the step that made it true.
    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        request.library_id,
        observed_source_truth_version,
    )
    .await?;
    // The head was published by the base step; re-read it so deferred effects
    // (the graph summary refresh) only run for a revision that is still current.
    let publicly_visible =
        crate::infra::repositories::content_document_head_repository::get_document_head_with_executor(
            &mut *transaction,
            request.document_id,
        )
        .await?
        .is_some_and(|head| head.readable_revision_id == Some(request.revision_id));
    transaction.commit().await.context("commit content-ingest enrichment finalization")?;

    Ok(PublishContentIngestSuccessOutcome::Applied {
        source_truth_version,
        mutation_completed: false,
        publicly_visible,
    })
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
        ContentGraphPublication::None,
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

/// Identity fields every content-ingest publication step is fenced to.
///
/// Base publication, enrichment finalization and the one-transaction
/// publication all address the same job/attempt/document/revision/mutation
/// item; carrying one small value keeps each step fenced to the exact same
/// identities instead of re-reading whatever shape the caller happens to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PublicationRequest {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    mutation_id: Uuid,
    mutation_item_id: Uuid,
    attempt_id: Uuid,
    completed_at: DateTime<Utc>,
}

impl From<&PublishContentIngestSuccess> for PublicationRequest {
    fn from(input: &PublishContentIngestSuccess) -> Self {
        Self {
            workspace_id: input.workspace_id,
            library_id: input.library_id,
            document_id: input.document_id,
            revision_id: input.revision_id,
            mutation_id: input.mutation_id,
            mutation_item_id: input.mutation_item_id,
            attempt_id: input.attempt_id,
            completed_at: input.completed_at,
        }
    }
}

impl From<&PublishContentIngestBase> for PublicationRequest {
    fn from(input: &PublishContentIngestBase) -> Self {
        Self {
            workspace_id: input.workspace_id,
            library_id: input.library_id,
            document_id: input.document_id,
            revision_id: input.revision_id,
            mutation_id: input.mutation_id,
            mutation_item_id: input.mutation_item_id,
            attempt_id: input.attempt_id,
            completed_at: input.completed_at,
        }
    }
}

impl From<&FinalizeContentIngestEnrichment> for PublicationRequest {
    fn from(input: &FinalizeContentIngestEnrichment) -> Self {
        Self {
            workspace_id: input.workspace_id,
            library_id: input.library_id,
            document_id: input.document_id,
            revision_id: input.revision_id,
            mutation_id: input.mutation_id,
            mutation_item_id: input.mutation_item_id,
            attempt_id: input.attempt_id,
            completed_at: input.completed_at,
        }
    }
}

/// Prove the locked job is the one this request claims to be publishing.
///
/// The lock was taken through the attempt identity, so every other identity on
/// the job is still unverified at that point: a mismatch means two publication
/// paths raced onto one attempt and one of them is about to write another
/// document's readiness.
fn ensure_publication_job_matches_request(
    job: &PublicationJobAuthorityRow,
    request: &PublicationRequest,
) -> anyhow::Result<()> {
    anyhow::ensure!(job.workspace_id == request.workspace_id, "publication workspace mismatch");
    anyhow::ensure!(job.library_id == request.library_id, "publication library mismatch");
    anyhow::ensure!(job.mutation_id == Some(request.mutation_id), "publication mutation mismatch");
    anyhow::ensure!(
        job.mutation_item_id == Some(request.mutation_item_id),
        "publication mutation item mismatch"
    );
    anyhow::ensure!(
        job.knowledge_document_id == Some(request.document_id),
        "publication document mismatch"
    );
    anyhow::ensure!(
        job.knowledge_revision_id == Some(request.revision_id),
        "publication revision mismatch"
    );
    anyhow::ensure!(job.job_kind == "content_mutation", "publication job kind mismatch");
    Ok(())
}

/// Publishes the readable head of a revision whose visibility decision allows
/// it, closing the linked operation exactly once.
async fn publish_visible_base_revision(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    job: &PublicationJobAuthorityRow,
    input: &PublishContentIngestBase,
    text_readable_at: DateTime<Utc>,
    mutation_completed: bool,
) -> anyhow::Result<()> {
    publish_content_document_head(transaction, request, None).await?;
    crate::infra::repositories::invalidate_unpublished_runtime_graph_snapshot(
        transaction,
        request.library_id,
        false,
    )
    .await?;
    publish_readable_head_projection(transaction, request).await?;
    // Completing the aggregate is the user-visible request finishing, so this
    // step closes the linked async operation exactly once. The attempt and job
    // stay open: their completion belongs to the enrichment finalization.
    if mutation_completed && let Some(async_operation_id) = job.async_operation_id {
        complete_content_mutation_operation(
            transaction,
            request,
            input.completed_at,
            async_operation_id,
        )
        .await?;
    }
    // The revision became answer-visible here, so its lifecycle event is
    // emitted once by this step; the enrichment finalization never re-emits it.
    enqueue_revision_ready_event(transaction, request, text_readable_at).await
}

/// Completes the lifecycle of a withheld revision: nothing else will ever run
/// for it, and vector and graph states were never claimed.
async fn complete_withheld_base_revision(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    job: &PublicationJobAuthorityRow,
    input: &PublishContentIngestBase,
    text_readable_at: DateTime<Utc>,
    mutation_completed: bool,
) -> anyhow::Result<()> {
    complete_content_ingest_lifecycle(
        transaction,
        ContentIngestLifecycleCompletion {
            request,
            job,
            completed_at: input.completed_at,
            revision_ready_event_at: Some(text_readable_at),
            mutation_completed,
            attempt_degradation_code: "",
            terminal_stage_events: &[],
        },
    )
    .await
}

/// Publishes the readable head of an enriched revision together with its
/// summary, then projects the readable head.
async fn publish_visible_success_revision(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    effects: &ContentIngestPublicationEffects,
) -> anyhow::Result<()> {
    publish_content_document_head(transaction, request, effects.document_summary.as_deref())
        .await?;
    crate::infra::repositories::invalidate_unpublished_runtime_graph_snapshot(
        transaction,
        request.library_id,
        false,
    )
    .await?;
    publish_readable_head_projection(transaction, request).await
}

/// Discards a staged graph projection that predates the current savepoint and
/// re-runs the publication with the re-read source fence and profile.
async fn discard_unpublished_graph_and_retry(
    transaction: Transaction<'static, Postgres>,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
    request: &PublicationRequest,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    let mut transaction = transaction;
    sqlx::query("rollback to savepoint ingest_graph_projection").execute(&mut *transaction).await?;
    let source_version = lock_guarded_publication_source(&mut transaction, request).await?;
    let profile =
        active_embedding_profile_key_with_transaction(&mut transaction, request.library_id).await?;
    Box::pin(publish_content_ingest_success_in_transaction(
        transaction,
        input,
        effects,
        RevisionPublicationProfileFence::Observed(profile),
        source_version,
        ContentGraphPublication::Discarded,
    ))
    .await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContentGraphPublication {
    None,
    Staged,
    Discarded,
}

async fn publish_content_ingest_success_in_transaction(
    mut transaction: Transaction<'static, Postgres>,
    input: &PublishContentIngestSuccess,
    effects: &ContentIngestPublicationEffects,
    profile_fence: RevisionPublicationProfileFence<'_>,
    observed_source_truth_version: i64,
    graph_publication: ContentGraphPublication,
) -> anyhow::Result<PublishContentIngestSuccessOutcome> {
    let request = PublicationRequest::from(input);
    // Required lock order is library -> job -> attempt. Lock the job through
    // the supplied attempt identity first, then lock and prove that exact
    // attempt is still the newest lease for the job.
    let job = match lock_content_publication_authority(&mut transaction, &request).await? {
        ContentAuthority::Lost { rollback_context, commit_context } => {
            finish_lost_authority_transaction(
                transaction,
                graph_publication == ContentGraphPublication::Staged,
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

    let decision = prepare_content_publication_decision(&mut transaction, input).await?;
    if !decision.publicly_visible && graph_publication == ContentGraphPublication::Staged {
        // Retained model/extraction results predate this savepoint. Discard the
        // derived graph and the tentative lifecycle decision together, then
        // revalidate authority and complete only the unpublished revision. The
        // savepoint rollback releases later row-lock upgrades, so the source
        // fence and profile must be read again before completing the lifecycle.
        return discard_unpublished_graph_and_retry(transaction, input, effects, &request).await;
    }
    anyhow::ensure!(
        graph_publication != ContentGraphPublication::Discarded || !decision.publicly_visible,
        "publication eligibility changed after discarding unpublished graph"
    );
    if decision.publicly_visible {
        publish_visible_success_revision(&mut transaction, &request, effects).await?;
    }

    let mutation_completed = decision.mutation_completed;
    let publicly_visible = decision.publicly_visible;
    complete_content_ingest_lifecycle(
        &mut transaction,
        ContentIngestLifecycleCompletion {
            request: &request,
            job: &job,
            completed_at: input.completed_at,
            revision_ready_event_at: Some(input.completed_at),
            mutation_completed,
            attempt_degradation_code: &input.graph_state,
            terminal_stage_events: &effects.terminal_stage_events,
        },
    )
    .await?;
    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        request.library_id,
        observed_source_truth_version,
    )
    .await?;

    transaction.commit().await.context("commit content-ingest publication")?;

    Ok(PublishContentIngestSuccessOutcome::Applied {
        source_truth_version,
        mutation_completed,
        publicly_visible,
    })
}

/// Tentative lifecycle writes precede the canonical membership decision. A
/// staged, denied graph rolls these writes back and revalidates this same path.
async fn prepare_content_publication_decision(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestSuccess,
) -> anyhow::Result<ContentPublicationDecision> {
    let request = PublicationRequest::from(input);
    lock_publication_document_and_head(transaction, &request).await?;
    publish_content_revision_readiness(transaction, input).await?;
    apply_publication_item_and_aggregate(transaction, &request).await
}

/// Base publication decision: the document/head are locked, only the prepared
/// text becomes readable, and the exact mutation item and ordinary aggregate
/// are applied. Vector and graph channel states are left untouched, so a reader
/// can never mistake deferred enrichment for completed coverage.
async fn prepare_base_publication_decision(
    transaction: &mut Transaction<'_, Postgres>,
    input: &PublishContentIngestBase,
) -> anyhow::Result<(ContentPublicationDecision, DateTime<Utc>)> {
    let request = PublicationRequest::from(input);
    lock_publication_document_and_head(transaction, &request).await?;
    let text_readable_at =
        publish_base_revision_readiness(transaction, &request, input.completed_at).await?;
    let decision = apply_publication_item_and_aggregate(transaction, &request).await?;
    Ok((decision, text_readable_at))
}

/// Locks the exact document and its head. Every publication path starts here so
/// a head write, a mutation claim and a visibility decision share one lock order.
async fn lock_publication_document_and_head(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
) -> anyhow::Result<()> {
    let document =
        crate::infra::repositories::content_document_repository::lock_document_by_id_with_executor(
            &mut **transaction,
            request.document_id,
        )
        .await?
        .context("publication document disappeared")?;
    anyhow::ensure!(
        document.workspace_id == request.workspace_id && document.library_id == request.library_id,
        "publication document scope mismatch"
    );
    content_document_head_repository::get_document_head_for_update_with_executor(
        &mut **transaction,
        request.document_id,
    )
    .await?
    .context("publication head disappeared")?;
    Ok(())
}

/// Applies the exact mutation item, completes the ordinary aggregate when it is
/// the last acceptable item, and reports whether this revision is visible.
async fn apply_publication_item_and_aggregate(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
) -> anyhow::Result<ContentPublicationDecision> {
    lock_content_mutation_scope(transaction, request).await?;
    apply_content_mutation_item(transaction, request).await?;
    let mutation_completed = complete_content_aggregate(transaction, request).await?;
    let publicly_visible = content_document_head_repository::publication_item_is_visible(
        transaction,
        request.document_id,
        request.revision_id,
        request.mutation_item_id,
    )
    .await?;
    Ok(ContentPublicationDecision { mutation_completed, publicly_visible })
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
    request: &PublicationRequest,
) -> anyhow::Result<ContentAuthority> {
    let job = lock_publication_job(
        transaction,
        request.attempt_id,
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

    ensure_publication_job_matches_request(&job, request)?;

    // Shared lock order with ordinary stage writers is job -> stage-event
    // advisory -> attempt row. Taking the attempt row first would deadlock an
    // ordinary writer that already owns the advisory and is about to update
    // attempt progress.
    let authoritative_attempt = lock_authoritative_publication_attempt(
        transaction,
        request.attempt_id,
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
        input.library_id,
        input.expected_source_truth_version,
        input.embedding_profile_key.as_deref(),
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

/// Publishes only the permitted text of the exact revision and returns when it
/// first became readable.
///
/// `text_readable_at` is preserved across retries so a resumed attempt re-emits
/// the identical readable timestamp, which keeps the revision-ready lifecycle
/// event idempotent. Vector and graph states are deliberately absent: they stay
/// `accepted`/`processing` until the enrichment finalizer records a real proof or
/// outcome. Nothing here can mark an unembedded revision vector-ready.
async fn publish_base_revision_readiness(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    readable_at: DateTime<Utc>,
) -> anyhow::Result<DateTime<Utc>> {
    let text_readable_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        "update knowledge_revision
         set text_state = 'text_readable',
             text_readable_at = coalesce(text_readable_at, $3)
         where revision_id = $1
           and library_id = $2
         returning text_readable_at",
    )
    .bind(request.revision_id)
    .bind(request.library_id)
    .bind(readable_at)
    .fetch_optional(&mut **transaction)
    .await
    .context("publish content-ingest base text readiness")?
    .context("base publication revision disappeared")?;
    Ok(text_readable_at)
}

/// Records the enrichment outcome on the same revision: chunk vectors are
/// complete and validated, and the graph layer keeps its own terminal state.
/// Text readability is owned by base publication and is never rewritten here.
async fn publish_enrichment_revision_states(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    graph_state: &str,
    graph_ready_at: Option<DateTime<Utc>>,
    completed_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let readiness = sqlx::query(
        "update knowledge_revision
         set vector_state = 'ready',
             graph_state = $3,
             vector_ready_at = coalesce(vector_ready_at, $5),
             graph_ready_at = $4
         where revision_id = $1
           and library_id = $2",
    )
    .bind(request.revision_id)
    .bind(request.library_id)
    .bind(graph_state)
    .bind(graph_ready_at)
    .bind(completed_at)
    .execute(&mut **transaction)
    .await
    .context("publish content-ingest enrichment states")?;
    anyhow::ensure!(readiness.rows_affected() == 1, "enrichment revision disappeared");
    Ok(())
}

/// Locks the aggregate mutation and the exact mutation item and proves both
/// belong to the request, then requires the item to be `pending` or `applied`.
async fn lock_content_mutation_scope(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
) -> anyhow::Result<()> {
    let mutation = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id
         from content_mutation
         where id = $1
         for update",
    )
    .bind(request.mutation_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock content-ingest publication mutation")?
    .context("publication mutation disappeared")?;
    anyhow::ensure!(mutation.0 == request.workspace_id, "publication mutation workspace mismatch");
    anyhow::ensure!(mutation.1 == request.library_id, "publication mutation library mismatch");

    let item = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>, String)>(
        "select mutation_id, document_id, result_revision_id, item_state::text
         from content_mutation_item
         where id = $1
         for update",
    )
    .bind(request.mutation_item_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock exact content-ingest mutation item")?
    .context("publication mutation item disappeared")?;
    ensure_publication_item_matches_request(&item, request)
}

/// The locked item must belong to the publication request's mutation, document,
/// and revision, and may only be `pending` or `applied`.
fn ensure_publication_item_matches_request(
    item: &(Uuid, Option<Uuid>, Option<Uuid>, String),
    request: &PublicationRequest,
) -> anyhow::Result<()> {
    anyhow::ensure!(item.0 == request.mutation_id, "publication item mutation mismatch");
    anyhow::ensure!(item.1 == Some(request.document_id), "publication item document mismatch");
    anyhow::ensure!(
        item.2 == Some(request.revision_id),
        "publication item result revision mismatch"
    );
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
    request: &PublicationRequest,
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
    .bind(request.mutation_item_id)
    .bind(request.mutation_id)
    .bind(request.document_id)
    .bind(request.revision_id)
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
    request: &PublicationRequest,
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
    .bind(request.mutation_id)
    .fetch_one(&mut **transaction)
    .await
    .context("classify web-owned mutation aggregate")?;
    complete_aggregate_mutation_if_ready(transaction, request, web_run_owns_aggregate).await
}

/// Publishes the exact mutation's head and its own summary. An absent summary
/// publishes an absent document summary: a freshly readable revision has no
/// summary until enrichment produces one.
async fn publish_content_document_head(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    document_summary: Option<&str>,
) -> anyhow::Result<()> {
    let previous_head =
        content_document_head_repository::get_document_head_for_update_with_executor(
            &mut **transaction,
            request.document_id,
        )
        .await
        .context("lock content document head for ingest publication")?
        .context("content document head disappeared before ingest publication")?;
    content_document_head_repository::upsert_document_head_without_generation_with_executor(
        &mut **transaction,
        &NewContentDocumentHead {
            publishing_mutation_item_id: Some(request.mutation_item_id),
            document_id: request.document_id,
            active_revision_id: Some(request.revision_id),
            readable_revision_id: Some(request.revision_id),
            latest_mutation_id: previous_head.latest_mutation_id,
            latest_successful_attempt_id: Some(request.attempt_id),
        },
    )
    .await
    .context("publish content document head")?;
    let document_summary = document_summary.map(str::trim).filter(|summary| !summary.is_empty());
    let summary = sqlx::query(
        "update content_document_head set document_summary = $2
          where document_id = $1 and readable_revision_id = $3",
    )
    .bind(request.document_id)
    .bind(document_summary)
    .bind(request.revision_id)
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

/// Publishes the enrichment-generated summary onto the head that still points at
/// this exact revision. A superseded revision never rewrites the current document
/// summary, so an absent target row is a normal no-op rather than an error.
async fn publish_head_summary_if_current(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    document_summary: Option<&str>,
) -> anyhow::Result<()> {
    let Some(document_summary) =
        document_summary.map(str::trim).filter(|summary| !summary.is_empty())
    else {
        return Ok(());
    };
    sqlx::query(
        "update content_document_head set document_summary = $2
          where document_id = $1 and readable_revision_id = $3",
    )
    .bind(request.document_id)
    .bind(document_summary)
    .bind(request.revision_id)
    .execute(&mut **transaction)
    .await
    .context("publish enriched content document summary")?;
    Ok(())
}

/// Refreshes the library's graph provenance coverage and materializes the
/// document projection of the readable head. Both are answer-visible surfaces, so
/// they belong to the step that publishes the head, not to enrichment.
async fn publish_readable_head_projection(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
) -> anyhow::Result<()> {
    super::refresh_runtime_graph_provenance_coverage_for_publication(
        transaction,
        request.library_id,
        request.document_id,
        request.revision_id,
    )
    .await?;
    let materialized =
        content_knowledge_projection_repository::materialize_knowledge_document_from_current_head_with_transaction(
            transaction,
            request.document_id,
        )
        .await
        .context("materialize content-ingest knowledge projection")?;
    anyhow::ensure!(
        materialized.outcome == MaterializeKnowledgeDocumentOutcome::Materialized,
        "content-ingest projection could not materialize: {:?}",
        materialized.outcome
    );
    Ok(())
}

/// Everything one lifecycle completion needs beyond the transaction: the fenced
/// identities, the exact timestamps, and the terminal evidence to append.
struct ContentIngestLifecycleCompletion<'a> {
    request: &'a PublicationRequest,
    job: &'a PublicationJobAuthorityRow,
    completed_at: DateTime<Utc>,
    /// Timestamp of the revision-ready lifecycle event, or `None` when the event
    /// was already emitted by the step that published the revision. It is the
    /// revision's readable timestamp so a resumed attempt re-emits an identical
    /// event instead of a conflicting one.
    revision_ready_event_at: Option<DateTime<Utc>>,
    mutation_completed: bool,
    attempt_degradation_code: &'a str,
    terminal_stage_events: &'a [NewIngestStageEvent],
}

/// Completes the publication attempt and the leased job, transitions the linked
/// async operation, and emits the revision-ready lifecycle event. It never writes
/// the document head: the head belongs to publication, not to completion.
async fn complete_content_ingest_lifecycle(
    transaction: &mut Transaction<'_, Postgres>,
    completion: ContentIngestLifecycleCompletion<'_>,
) -> anyhow::Result<()> {
    let ContentIngestLifecycleCompletion {
        request,
        job,
        completed_at,
        revision_ready_event_at,
        mutation_completed,
        attempt_degradation_code,
        terminal_stage_events,
    } = completion;

    complete_publication_attempt_with_events(
        transaction,
        PublicationAttemptCompletion {
            attempt_id: request.attempt_id,
            job_id: job.id,
            completed_at,
            attempt_degradation_code,
        },
        terminal_stage_events,
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
    .bind(completed_at)
    .bind(request.workspace_id)
    .bind(request.library_id)
    .bind(request.mutation_id)
    .bind(request.mutation_item_id)
    .bind(request.document_id)
    .bind(request.revision_id)
    .execute(&mut **transaction)
    .await
    .context("complete content-ingest job")?;
    anyhow::ensure!(job_update.rows_affected() == 1, "publication job lost its lease");

    if mutation_completed && let Some(async_operation_id) = job.async_operation_id {
        complete_content_mutation_operation(transaction, request, completed_at, async_operation_id)
            .await?;
    }

    if let Some(event_occurred_at) = revision_ready_event_at {
        enqueue_revision_ready_event(transaction, request, event_occurred_at).await?;
    }

    Ok(())
}

/// Emits the deterministic revision-ready lifecycle event for the request's exact
/// revision. The event identity is the revision; the outbox keeps the first
/// persisted transition time, so a retry of the same publication step reuses the
/// row instead of colliding with a newer `occurred_at`.
async fn enqueue_revision_ready_event(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    occurred_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let event = WebhookEvent {
        event_type: "revision.ready".to_string(),
        event_id: revision_ready_event_id(request.revision_id),
        occurred_at,
        workspace_id: request.workspace_id,
        library_id: request.library_id,
        payload_json: serde_json::json!({
            "document_id": request.document_id,
            "revision_id": request.revision_id,
            "library_id": request.library_id,
        }),
    };
    webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(
        &mut **transaction,
        &event,
    )
    .await
    .context("enqueue deterministic revision-ready lifecycle event")?;
    Ok(())
}

/// Completes the linked content-mutation async operation for a publication that
/// completed its aggregate mutation. The exact operation identity row must
/// exist and still be owned by the publication's mutation.
///
/// The write is an idempotent ready transition, not a one-shot claim: repeating
/// it sets the same identity row back to `ready`, so a resumed base publication
/// never fails because the operation is already complete.
async fn complete_content_mutation_operation(
    transaction: &mut Transaction<'_, Postgres>,
    request: &PublicationRequest,
    completed_at: DateTime<Utc>,
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
    .bind(completed_at)
    .bind(request.workspace_id)
    .bind(request.library_id)
    .bind(request.mutation_id)
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
    library_id: Uuid,
    expected_source_truth_version: i64,
    expected_embedding_profile_key: Option<&str>,
    observed_source_truth_version: i64,
) -> anyhow::Result<()> {
    if matches!(profile_fence, RevisionPublicationProfileFence::StrictSourceVersion) {
        anyhow::ensure!(
            observed_source_truth_version == expected_source_truth_version,
            "library source or embedding profile changed before content-ingest publication"
        );
        return Ok(());
    }
    if let Some(expected_embedding_profile_key) = expected_embedding_profile_key {
        let active_embedding_profile_key = match profile_fence {
            RevisionPublicationProfileFence::External(active_profile_source) => {
                active_profile_source.active_embedding_profile_key(library_id).await.context(
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
        observed_source_truth_version >= expected_source_truth_version,
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
    request: &PublicationRequest,
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
    .bind(request.mutation_id)
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
        .bind(request.mutation_id)
        .bind(request.completed_at)
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
