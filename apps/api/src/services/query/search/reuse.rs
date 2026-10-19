use super::{failures::fail_embed_chunks_after_cleanup, *};

#[derive(Debug, Default)]
struct RevisionVectorCoverage {
    chunk_ids: BTreeSet<Uuid>,
}

/// Inputs for [`resolve_reused_revision_chunk_ids`].
pub(super) struct ResolveReusedRevisionChunkIdsInput<'a> {
    pub service: &'a SearchService,
    pub state: &'a AppState,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub chunks: &'a [KnowledgeChunkRow],
    pub binding: &'a ResolvedRuntimeBinding,
    pub embedding_profile_key: &'a str,
    pub freshness_generation: i64,
    pub source_truth_version: i64,
    pub ingest_attempt: IngestVectorWriteFence,
    pub cleanup_owned_vector_ids: &'a mut BTreeSet<Uuid>,
}

pub(super) async fn resolve_reused_revision_chunk_ids(
    input: ResolveReusedRevisionChunkIdsInput<'_>,
) -> std::result::Result<(EmbeddingDimensionState, BTreeSet<Uuid>), QueryServiceError> {
    let ResolveReusedRevisionChunkIdsInput {
        service,
        state,
        library_id,
        revision_id,
        chunks,
        binding,
        embedding_profile_key,
        freshness_generation,
        source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
    } = input;
    let _vector_guard = service.vector_plane_read_guard(state, library_id).await?;
    let dimension_state =
        library_vector_index_dimension_state_for_binding(state, library_id, binding).await?;
    let EmbeddingDimensionState::Known(expected_dimensions) = dimension_state else {
        return Ok((dimension_state, BTreeSet::new()));
    };
    let coverage = load_current_revision_chunk_vector_ids(
        state,
        revision_id,
        chunks,
        embedding_profile_key,
        freshness_generation,
        expected_dimensions.get(),
    )
    .await;
    let mut reused_chunk_ids = match coverage {
        Ok(coverage) => coverage.chunk_ids,
        Err(error) => {
            return fail_embed_chunks_after_cleanup(
                state,
                revision_id,
                source_truth_version,
                ingest_attempt,
                cleanup_owned_vector_ids,
                error,
            )
            .await;
        }
    };
    if !reused_chunk_ids.is_empty() {
        tracing::info!(
            revision_id = %revision_id,
            reused = reused_chunk_ids.len(),
            total_chunks = chunks.len(),
            "embed_chunk resume: reusing current revision chunk vectors",
        );
    }
    let chunks_missing_current_vectors = chunks
        .iter()
        .filter(|chunk| !reused_chunk_ids.contains(&chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    let parent_reuse = reuse_chunk_vectors_from_parent_revision(ParentRevisionVectorReuseInput {
        state,
        revision_id,
        new_chunks: &chunks_missing_current_vectors,
        embedding_model_key: embedding_profile_key,
        freshness_generation,
        expected_dimensions: expected_dimensions.get(),
        expected_source_truth_version: source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
    })
    .await;
    match parent_reuse {
        Ok(parent_reused_chunk_ids) => reused_chunk_ids.extend(parent_reused_chunk_ids),
        Err(error) => {
            return fail_embed_chunks_after_cleanup(
                state,
                revision_id,
                source_truth_version,
                ingest_attempt,
                cleanup_owned_vector_ids,
                error,
            )
            .await;
        }
    }
    Ok((dimension_state, reused_chunk_ids))
}

/// Chunk ids that still need a vector on this attempt: every chunk whose id is
/// not already covered by a persisted/reusable vector. Drives the embed-only-the-
/// remainder resume path so a retry after a transient failure re-embeds just the
/// missing chunks instead of the whole revision.
pub(super) fn chunk_ids_missing_vectors(
    chunk_ids: &[Uuid],
    already_covered: &BTreeSet<Uuid>,
) -> Vec<Uuid> {
    chunk_ids.iter().copied().filter(|id| !already_covered.contains(id)).collect()
}

/// Count-gated readiness: a revision is only fully embedded when the chunks
/// embedded on this attempt plus the chunks reused from prior attempts/parents
/// exactly cover every chunk. A half-embedded revision (e.g. after a transient
/// failure that preserved partial vectors) fails this gate and never flips to
/// `vector_state = ready`.
pub(super) fn embed_coverage_is_complete(
    chunks_embedded: usize,
    chunks_reused: usize,
    total: usize,
) -> bool {
    chunks_embedded.saturating_add(chunks_reused) == total
}

async fn load_current_revision_chunk_vector_ids(
    state: &AppState,
    revision_id: Uuid,
    chunks: &[KnowledgeChunkRow],
    embedding_model_key: &str,
    freshness_generation: i64,
    expected_dimensions: u64,
) -> AnyhowResult<RevisionVectorCoverage> {
    let mut coverage = RevisionVectorCoverage::default();
    for chunk_batch in chunks.chunks(CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE) {
        let chunk_ids = chunk_batch.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        let vectors = state
            .search_store
            .list_chunk_vectors_by_chunks(
                &chunk_ids,
                embedding_model_key,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
            )
            .await
            .with_context(|| {
                format!("failed to load current chunk vectors for revision {revision_id}")
            })?;
        for vector in vectors {
            if vector.revision_id == revision_id
                && vector.freshness_generation == freshness_generation
                && vector.vector_kind == KNOWLEDGE_CHUNK_VECTOR_KIND
                && validate_embedding_vector_dimensions(
                    expected_dimensions,
                    &vector.vector,
                    format!("current chunk vector {}", vector.chunk_id),
                )
                .is_ok()
            {
                coverage.chunk_ids.insert(vector.chunk_id);
            }
        }
    }
    Ok(coverage)
}

/// Inputs for [`reuse_chunk_vectors_from_parent_revision`].
struct ParentRevisionVectorReuseInput<'a> {
    state: &'a AppState,
    revision_id: Uuid,
    new_chunks: &'a [KnowledgeChunkRow],
    embedding_model_key: &'a str,
    freshness_generation: i64,
    expected_dimensions: u64,
    expected_source_truth_version: i64,
    ingest_attempt: IngestVectorWriteFence,
    cleanup_owned_vector_ids: &'a mut BTreeSet<Uuid>,
}

async fn reuse_chunk_vectors_from_parent_revision(
    input: ParentRevisionVectorReuseInput<'_>,
) -> AnyhowResult<BTreeSet<Uuid>> {
    let ParentRevisionVectorReuseInput {
        state,
        revision_id,
        new_chunks,
        embedding_model_key,
        freshness_generation,
        expected_dimensions,
        expected_source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
    } = input;
    if new_chunks.is_empty() {
        return Ok(BTreeSet::new());
    }
    let Some(parent_revision_id) = load_parent_revision_id(state, revision_id).await? else {
        return Ok(BTreeSet::new());
    };
    let new_chunks_by_checksum = index_chunks_by_checksum(new_chunks);
    if new_chunks_by_checksum.is_empty() {
        return Ok(BTreeSet::new());
    }
    let parent_chunks =
        state.document_store.list_chunks_by_revision(parent_revision_id).await.with_context(
            || format!("failed to list parent chunks for vector reuse from {parent_revision_id}"),
        )?;
    let (parent_chunk_by_id, parent_ids_by_checksum) =
        index_matching_parent_chunks(&parent_chunks, &new_chunks_by_checksum);
    if parent_ids_by_checksum.is_empty() {
        return Ok(BTreeSet::new());
    }

    let mut reused_chunk_ids = BTreeSet::new();
    for parent_batch in parent_ids_by_checksum
        .values()
        .copied()
        .collect::<Vec<_>>()
        .chunks(CHUNK_VECTOR_REUSE_SOURCE_BATCH_SIZE)
    {
        let vectors =
            load_parent_vectors(state, parent_revision_id, parent_batch, embedding_model_key)
                .await?;
        let current_vectors = newest_parent_vectors(vectors);
        persist_parent_vector_matches(PersistParentVectorMatchesInput {
            state,
            current_vectors: &current_vectors,
            parent_chunk_by_id: &parent_chunk_by_id,
            new_chunks_by_checksum: &new_chunks_by_checksum,
            reused_chunk_ids: &mut reused_chunk_ids,
            embedding_model_key,
            freshness_generation,
            expected_dimensions,
            expected_source_truth_version,
            ingest_attempt,
            cleanup_owned_vector_ids,
        })
        .await?;
    }

    if !reused_chunk_ids.is_empty() {
        tracing::info!(
            revision_id = %revision_id,
            parent_revision_id = %parent_revision_id,
            reused = reused_chunk_ids.len(),
            total_chunks = new_chunks.len(),
            "diff-aware ingest: reusing chunk vectors for unchanged chunks",
        );
    }
    Ok(reused_chunk_ids)
}

async fn load_parent_revision_id(
    state: &AppState,
    revision_id: Uuid,
) -> AnyhowResult<Option<Uuid>> {
    Ok(content_revision_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
        .await
        .with_context(|| format!("failed to load content revision {revision_id} for vector reuse"))?
        .and_then(|revision| revision.parent_revision_id))
}

fn index_chunks_by_checksum(
    chunks: &[KnowledgeChunkRow],
) -> HashMap<String, Vec<&KnowledgeChunkRow>> {
    chunks.iter().filter_map(|chunk| chunk_text_reuse_key(chunk).map(|key| (key, chunk))).fold(
        HashMap::new(),
        |mut index, (key, chunk)| {
            index.entry(key).or_default().push(chunk);
            index
        },
    )
}

fn index_matching_parent_chunks<'a>(
    parent_chunks: &'a [KnowledgeChunkRow],
    new_chunks_by_checksum: &HashMap<String, Vec<&KnowledgeChunkRow>>,
) -> (HashMap<Uuid, &'a KnowledgeChunkRow>, HashMap<String, Uuid>) {
    parent_chunks
        .iter()
        .filter_map(|chunk| {
            let checksum = chunk_text_reuse_key(chunk)?;
            new_chunks_by_checksum.contains_key(&checksum).then_some((checksum, chunk))
        })
        .fold(
            (HashMap::new(), HashMap::new()),
            |(mut by_id, mut by_checksum), (checksum, chunk)| {
                by_checksum.entry(checksum).or_insert(chunk.chunk_id);
                by_id.entry(chunk.chunk_id).or_insert(chunk);
                (by_id, by_checksum)
            },
        )
}

async fn load_parent_vectors(
    state: &AppState,
    parent_revision_id: Uuid,
    parent_batch: &[Uuid],
    embedding_model_key: &str,
) -> AnyhowResult<Vec<KnowledgeChunkVectorRow>> {
    state
        .search_store
        .list_chunk_vectors_by_chunks(
            parent_batch,
            embedding_model_key,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
        )
        .await
        .with_context(|| {
            format!("failed to load parent chunk vectors for revision {parent_revision_id}")
        })
}

fn newest_parent_vectors(
    vectors: Vec<KnowledgeChunkVectorRow>,
) -> HashMap<Uuid, KnowledgeChunkVectorRow> {
    vectors.into_iter().fold(HashMap::new(), |mut current, vector| {
        match current.get(&vector.chunk_id) {
            Some(existing) if !chunk_vector_is_newer(&vector, existing) => {}
            _ => {
                current.insert(vector.chunk_id, vector);
            }
        }
        current
    })
}

/// Inputs for [`persist_parent_vector_matches`].
struct PersistParentVectorMatchesInput<'a> {
    state: &'a AppState,
    current_vectors: &'a HashMap<Uuid, KnowledgeChunkVectorRow>,
    parent_chunk_by_id: &'a HashMap<Uuid, &'a KnowledgeChunkRow>,
    new_chunks_by_checksum: &'a HashMap<String, Vec<&'a KnowledgeChunkRow>>,
    reused_chunk_ids: &'a mut BTreeSet<Uuid>,
    embedding_model_key: &'a str,
    freshness_generation: i64,
    expected_dimensions: u64,
    expected_source_truth_version: i64,
    ingest_attempt: IngestVectorWriteFence,
    cleanup_owned_vector_ids: &'a mut BTreeSet<Uuid>,
}

async fn persist_parent_vector_matches(
    input: PersistParentVectorMatchesInput<'_>,
) -> AnyhowResult<()> {
    let PersistParentVectorMatchesInput {
        state,
        current_vectors,
        parent_chunk_by_id,
        new_chunks_by_checksum,
        reused_chunk_ids,
        embedding_model_key,
        freshness_generation,
        expected_dimensions,
        expected_source_truth_version,
        ingest_attempt,
        cleanup_owned_vector_ids,
    } = input;
    let mut rows = Vec::with_capacity(CHUNK_EMBEDDING_BATCH_SIZE);
    for (parent_chunk_id, parent_vector) in current_vectors {
        let Ok(parent_dimensions) = validate_embedding_vector_dimensions(
            expected_dimensions,
            &parent_vector.vector,
            format!("parent chunk vector {}", parent_vector.chunk_id),
        ) else {
            continue;
        };
        let Some(parent_chunk) = parent_chunk_by_id.get(parent_chunk_id) else {
            continue;
        };
        let Some(parent_checksum) = chunk_text_reuse_key(parent_chunk) else {
            continue;
        };
        let Some(new_matches) = new_chunks_by_checksum.get(&parent_checksum) else {
            continue;
        };
        for new_chunk in new_matches {
            if reused_chunk_ids.contains(&new_chunk.chunk_id) {
                continue;
            }
            rows.push(KnowledgeChunkVectorRow {
                vector_id: Uuid::now_v7(),
                workspace_id: new_chunk.workspace_id,
                library_id: new_chunk.library_id,
                chunk_id: new_chunk.chunk_id,
                revision_id: new_chunk.revision_id,
                embedding_model_key: embedding_model_key.to_string(),
                vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
                dimensions: parent_dimensions,
                vector: parent_vector.vector.clone(),
                freshness_generation,
                created_at: Utc::now(),
                occurred_at: new_chunk.occurred_at,
                occurred_until: new_chunk.occurred_until,
            });
            reused_chunk_ids.insert(new_chunk.chunk_id);
            if rows.len() == CHUNK_EMBEDDING_BATCH_SIZE {
                persist_reused_vector_rows(
                    state,
                    &rows,
                    expected_source_truth_version,
                    embedding_model_key,
                    ingest_attempt,
                    cleanup_owned_vector_ids,
                )
                .await?;
                rows.clear();
            }
        }
    }
    if !rows.is_empty() {
        persist_reused_vector_rows(
            state,
            &rows,
            expected_source_truth_version,
            embedding_model_key,
            ingest_attempt,
            cleanup_owned_vector_ids,
        )
        .await?;
    }
    Ok(())
}

async fn persist_reused_vector_rows(
    state: &AppState,
    rows: &[KnowledgeChunkVectorRow],
    expected_source_truth_version: i64,
    embedding_model_key: &str,
    ingest_attempt: IngestVectorWriteFence,
    cleanup_owned_vector_ids: &mut BTreeSet<Uuid>,
) -> AnyhowResult<()> {
    let library_id =
        rows.first().context("reused vector write batch must not be empty")?.library_id;
    ensure_active_embedding_profile_key(state, library_id, embedding_model_key).await?;
    state
        .search_store
        .upsert_ingest_chunk_vectors_bulk_fenced(
            rows,
            &VectorWriteFence {
                expected_source_truth_version,
                embedding_profile_key: embedding_model_key.to_string(),
                ingest_attempt: Some(ingest_attempt),
                advance_source_truth_version: false,
            },
            state,
        )
        .await
        .context("failed to authority/profile-fenced persist reused chunk vectors")?;
    cleanup_owned_vector_ids.extend(rows.iter().map(|row| row.vector_id));
    Ok(())
}

fn chunk_vector_is_newer(
    candidate: &KnowledgeChunkVectorRow,
    existing: &KnowledgeChunkVectorRow,
) -> bool {
    candidate
        .freshness_generation
        .cmp(&existing.freshness_generation)
        .then_with(|| candidate.created_at.cmp(&existing.created_at))
        .then_with(|| candidate.vector_id.cmp(&existing.vector_id))
        .is_gt()
}

pub(super) fn chunk_text_reuse_key(chunk: &KnowledgeChunkRow) -> Option<String> {
    (!chunk.normalized_text.trim().is_empty()).then(|| {
        let mut hasher = Sha256::new();
        hasher.update(chunk.normalized_text.as_bytes());
        hex::encode(hasher.finalize())
    })
}
