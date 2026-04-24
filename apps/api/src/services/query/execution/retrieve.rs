use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        provider_profiles::EffectiveProviderProfile, query::RuntimeQueryMode,
        query_ir::QueryIR,
    },
    infra::{
        arangodb::document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
        },
        repositories::{self, ai_repository},
    },
    services::{
        knowledge::runtime_read::load_active_runtime_graph_projection,
        query::{
            latest_versions::{
                LATEST_VERSION_CHUNKS_PER_DOCUMENT, compare_version_desc,
                extract_semver_like_version, latest_version_chunk_score,
                latest_version_context_top_k, latest_version_family_key,
                latest_version_scope_terms, question_requests_latest_versions,
                requested_latest_version_count, text_has_release_version_marker,
            },
            planner::RuntimeQueryPlan,
        },
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::technical_literals::technical_literal_focus_keyword_segments;
use super::tuning::DOCUMENT_IDENTITY_SCORE_FLOOR;
use super::types::*;
use super::{
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, merge_canonical_table_aggregation_chunks,
    question_asks_table_aggregation, requested_initial_table_row_count,
};

const DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT: usize = 32;
const DIRECT_TABLE_AGGREGATION_ROW_LIMIT: usize = 24;
const DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT: usize = 32;

pub(crate) async fn load_graph_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<QueryGraphIndex> {
    let projection = load_active_runtime_graph_projection(state, library_id)
        .await
        .context("failed to load active runtime graph projection for query")?;
    let mut all_node_positions = HashMap::with_capacity(projection.nodes.len());
    for (position, node) in projection.nodes.iter().enumerate() {
        all_node_positions.insert(node.id, position);
    }

    let mut edge_positions = HashMap::with_capacity(projection.edges.len());
    let mut connected_node_ids = HashSet::with_capacity(projection.edges.len().saturating_mul(2));
    for (position, edge) in projection.edges.iter().enumerate() {
        let Some(&from_position) = all_node_positions.get(&edge.from_node_id) else {
            continue;
        };
        let Some(&to_position) = all_node_positions.get(&edge.to_node_id) else {
            continue;
        };
        let from_node_key = projection.nodes[from_position].canonical_key.as_str();
        let to_node_key = projection.nodes[to_position].canonical_key.as_str();
        if !state.bulk_ingest_hardening_services.graph_quality_guard.allows_relation(
            from_node_key,
            to_node_key,
            &edge.relation_type,
        ) {
            continue;
        }
        edge_positions.insert(edge.id, position);
        connected_node_ids.insert(edge.from_node_id);
        connected_node_ids.insert(edge.to_node_id);
    }
    let node_positions = projection
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(position, node)| {
            (node.node_type == "document" || connected_node_ids.contains(&node.id))
                .then_some((node.id, position))
        })
        .collect();

    Ok(QueryGraphIndex::new(projection, node_positions, edge_positions))
}

pub(crate) async fn load_latest_library_generation(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<KnowledgeLibraryGenerationRow>> {
    state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map(|rows| rows.into_iter().next())
        .map_err(|error| {
            anyhow::anyhow!("failed to derive library generations for runtime query: {error}")
        })
}

pub(crate) fn query_graph_status(
    generation: Option<&KnowledgeLibraryGenerationRow>,
) -> &'static str {
    match generation {
        Some(row) if row.active_graph_generation > 0 && row.degraded_state == "ready" => "current",
        Some(row) if row.active_graph_generation > 0 => "partial",
        _ => "empty",
    }
}

pub(crate) async fn load_document_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<HashMap<Uuid, KnowledgeDocumentRow>> {
    let library = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .context("failed to load library for runtime query document index")?;
    state
        .arango_document_store
        .list_documents_by_library(library.workspace_id, library_id, false)
        .await
        .map(|rows| rows.into_iter().map(|row| (row.document_id, row)).collect())
        .context("failed to load runtime query document index")
}

/// Ceiling on chunks pulled by the entity-bio fallback. Bounded so
/// the concat with vector + lexical hits does not drown the context
/// window on entities that appear across dozens of documents.
const ENTITY_BIO_CHUNK_CAP: usize = 24;

/// Synthetic score for entity-bio hits. Chunks reached via graph
/// evidence for a named target entity ARE the answer for a
/// biographical query, so they must outrank any vector / BM25 hit —
/// otherwise per-document diversification and top-K truncation drop
/// them in corpora where the entity label collides with unrelated
/// noise.
const ENTITY_BIO_CHUNK_SCORE: f32 = 1000.0;

/// For "who is X" / "что такое X" questions (`QueryAct::Describe` with
/// at least one target entity) the vector + lexical lanes often miss
/// the full picture — a rare surname in a technical procedure has low
/// cosine similarity to "кто такой X" and BM25 ranks a single random
/// mention. This helper fans out over the graph instead: match the
/// entity label against the admitted runtime graph, then load every
/// chunk of evidence attached to that node (capped at
/// `ENTITY_BIO_CHUNK_CAP`). The caller merges the result into the main
/// retrieval set so the answer model sees ALL mentions of the entity,
/// not just the top-scored one.
async fn load_entity_bio_chunks(
    state: &AppState,
    library_id: Uuid,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(ir) = query_ir else {
        return Ok(Vec::new());
    };
    if ir.target_entities.is_empty() {
        return Ok(Vec::new());
    }

    let snapshot = repositories::get_runtime_graph_snapshot(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load graph projection snapshot for entity-bio retrieval")?;
    let Some(snapshot) = snapshot else {
        return Ok(Vec::new());
    };

    // Entity-bio is a proper-name fan-out. An upstream reformulator
    // sometimes stuffs common concept nouns ("сотрудник", "контакт",
    // "фамилия") into target_entities; those match thousands of
    // unrelated graph entities and flood the chunk budget, pushing the
    // real rare-surname mentions out. When the IR contains at least
    // one capitalized mention, restrict to capitalized ones — the
    // extractor treats capitalization as the proper-noun signal. When
    // everything is lowercase (e.g. user typed "золотов" as-is), keep
    // the whole set so single-word surname queries still work.
    let has_capitalized = ir.target_entities.iter().any(|m| {
        m.label
            .trim()
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_uppercase)
    });
    let proper_name_mentions: Vec<&_> = ir
        .target_entities
        .iter()
        .filter(|m| {
            if !has_capitalized {
                return true;
            }
            m.label
                .trim()
                .chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(char::is_uppercase)
        })
        .collect();
    if proper_name_mentions.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen_nodes: HashSet<Uuid> = HashSet::new();
    let mut all_evidence_chunk_ids: Vec<Uuid> = Vec::new();
    for mention in &proper_name_mentions {
        if mention.label.trim().is_empty() {
            continue;
        }
        let nodes = repositories::search_admitted_runtime_graph_entities_by_query_text(
            &state.persistence.postgres,
            library_id,
            snapshot.projection_version,
            &mention.label,
            4,
        )
        .await
        .context("failed to search graph entities by label for entity-bio retrieval")?;
        for node in nodes {
            if !seen_nodes.insert(node.id) {
                continue;
            }
            let evidence = repositories::list_runtime_graph_evidence_by_target(
                &state.persistence.postgres,
                library_id,
                "node",
                node.id,
            )
            .await
            .context("failed to list graph evidence for entity-bio retrieval")?;
            for row in evidence {
                if let Some(chunk_id) = row.chunk_id {
                    if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                        break;
                    }
                    if !all_evidence_chunk_ids.contains(&chunk_id) {
                        all_evidence_chunk_ids.push(chunk_id);
                    }
                }
            }
            if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
        }
        if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
            break;
        }
    }

    // Graph-evidence is bounded by what the `extract_graph` stage
    // captured — low-confidence or oblique-case mentions often miss
    // that pass. Complement the graph lookup with a dedicated lexical
    // search over the entity label itself so every chunk where the
    // label appears as plain text contributes, not just the ones that
    // became evidence rows.
    let mut lexical_chunk_ids: Vec<Uuid> = Vec::new();
    for mention in &proper_name_mentions {
        if mention.label.trim().is_empty() {
            continue;
        }
        let remaining = ENTITY_BIO_CHUNK_CAP.saturating_sub(
            all_evidence_chunk_ids.len() + lexical_chunk_ids.len(),
        );
        if remaining == 0 {
            break;
        }
        let hits = state
            .arango_search_store
            .search_chunks(library_id, mention.label.trim(), remaining.max(4))
            .await
            .context("failed to run lexical entity-label search for entity-bio retrieval")?;
        for hit in hits {
            if lexical_chunk_ids.len() + all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
            if all_evidence_chunk_ids.contains(&hit.chunk_id)
                || lexical_chunk_ids.contains(&hit.chunk_id)
            {
                continue;
            }
            lexical_chunk_ids.push(hit.chunk_id);
        }
    }

    if all_evidence_chunk_ids.is_empty() && lexical_chunk_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut all_ids = all_evidence_chunk_ids;
    all_ids.extend(lexical_chunk_ids.iter().copied());
    let candidate_total = all_ids.len();
    let hits: Vec<(Uuid, f32)> =
        all_ids.into_iter().map(|id| (id, ENTITY_BIO_CHUNK_SCORE)).collect();
    let empty_targets: BTreeSet<Uuid> = BTreeSet::new();
    let candidates =
        batch_hydrate_hits(state, hits, document_index, plan_keywords, &empty_targets).await?;
    // Post-filter: ArangoSearch BM25 stems tokens, so a surname like
    // "Foster" can retrieve chunks mentioning "forest" that share a
    // stem but have nothing to do with the target person. Similarly,
    // a graph entity whose label contains the mention as substring may
    // attach evidence chunks that do not carry the name as plain text.
    // Keep only chunks whose raw text actually contains one of the
    // mention labels as a case-insensitive substring — this is the
    // literal grounding the answer model needs.
    let label_tokens: Vec<String> = proper_name_mentions
        .iter()
        .map(|m| m.label.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let chunks: Vec<RuntimeMatchedChunk> = candidates
        .into_iter()
        .filter(|c| {
            let haystack = c.source_text.to_lowercase();
            label_tokens.iter().any(|t| haystack.contains(t))
        })
        .collect();
    tracing::info!(
        stage = "retrieval.entity_bio",
        entity_label_count = ir.target_entities.len(),
        evidence_node_count = seen_nodes.len(),
        lexical_extra_count = lexical_chunk_ids.len(),
        candidate_chunk_count = candidate_total,
        evidence_chunk_count = chunks.len(),
        "entity-bio fan-out loaded extra chunks for Describe-intent query",
    );
    Ok(chunks)
}

pub(crate) async fn retrieve_document_chunks(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    question: &str,
    forced_target_document_ids: Option<&BTreeSet<Uuid>>,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let targeted_document_ids = forced_target_document_ids
        .filter(|ids| !ids.is_empty())
        .cloned()
        .unwrap_or_else(|| explicit_target_document_ids(question, document_index));
    let initial_table_row_count = requested_initial_table_row_count(question);
    let targeted_table_aggregation =
        question_asks_table_aggregation(question) && !targeted_document_ids.is_empty();
    let lexical_queries = build_lexical_queries(question, plan);
    let lexical_limit = limit.saturating_mul(2).max(24);
    let plan_keywords = &plan.keywords;
    let targeted_document_ids_ref = &targeted_document_ids;

    let vector_future = async {
        let started = std::time::Instant::now();
        if question_embedding.is_empty() {
            tracing::info!(
                stage = "retrieval.vector_skip",
                reason = "question_embedding_empty",
                "vector retrieve skipped: no question embedding"
            );
            return Ok::<(Vec<RuntimeMatchedChunk>, u128), anyhow::Error>((Vec::new(), 0));
        }
        let context =
            resolve_runtime_vector_search_context(state, library_id, provider_profile).await?;
        let Some(context) = context else {
            tracing::info!(
                stage = "retrieval.vector_skip",
                reason = "no_vector_search_context",
                "vector retrieve skipped: resolve_runtime_vector_search_context returned None (missing EmbedChunk binding or no active vector generation)"
            );
            return Ok::<(Vec<RuntimeMatchedChunk>, u128), anyhow::Error>((Vec::new(), 0));
        };
        let raw_hits = state
            .arango_search_store
            .search_chunk_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                question_embedding,
                limit.max(1),
                Some(16),
            )
            .await
            .context("failed to search canonical chunk vectors for runtime query")?;
        tracing::info!(
            stage = "retrieval.vector_search",
            raw_hit_count = raw_hits.len(),
            embedding_dims = question_embedding.len(),
            limit = limit.max(1),
            "vector search returned raw hits"
        );
        // Previously this fanned out one `get_chunk` call per hit via
        // `join_all`, costing an Arango round-trip for every vector
        // match (8–16 per query). On warm-cache grounded_answer that
        // N+1 cost ≈ 300–500 ms of retrieval time even when the
        // vector ranking was already cached. One batched
        // `list_chunks_by_ids` folds it into a single coordinator
        // query; the hit→chunk join happens in-process.
        let hits = batch_hydrate_hits(
            state,
            raw_hits.iter().map(|hit| (hit.chunk_id, hit.score as f32)).collect(),
            document_index,
            plan_keywords,
            targeted_document_ids_ref,
        )
        .await?;
        Ok((hits, started.elapsed().as_millis()))
    };

    // Lexical queries used to run in a tight `for` loop that awaited
    // each Arango `search_chunks` sequentially — on a 5–10 query
    // plan that stacked up to 2–5 s of pure round-trip time per
    // request. Running them concurrently via `join_all` gives the
    // Arango coordinator a chance to fan them out over the coordinator
    // thread pool; the merge step below still uses RRF so the
    // output order is unchanged.
    let lexical_future = async {
        let started = std::time::Instant::now();
        let lexical_query_count = lexical_queries.len();
        // Fan the AQL searches out in parallel — same as before — but
        // hydrate each query's hits through `batch_hydrate_hits` to
        // replace the per-hit `get_chunk` N+1 with a single
        // `list_chunks_by_ids` round-trip. With 4 lexical queries × ~20
        // hits each the old path fired ~80 serial chunk loads per
        // request; now it's at most 4 batched reads.
        let per_query_futures = lexical_queries.into_iter().map(|lexical_query| async move {
            let hits = state
                .arango_search_store
                .search_chunks(library_id, &lexical_query, lexical_limit)
                .await
                .with_context(|| {
                    format!(
                        "failed to run lexical Arango chunk search for runtime query: {lexical_query}"
                    )
                })?;
            batch_hydrate_hits(
                state,
                hits.into_iter().map(|hit| (hit.chunk_id, hit.score as f32)).collect(),
                document_index,
                plan_keywords,
                targeted_document_ids_ref,
            )
            .await
        });
        let per_query_results: Vec<Result<Vec<RuntimeMatchedChunk>, anyhow::Error>> =
            join_all(per_query_futures).await;
        let mut lexical_hits: Vec<RuntimeMatchedChunk> = Vec::new();
        for result in per_query_results {
            let query_hits = result?;
            lexical_hits = merge_chunks(lexical_hits, query_hits, lexical_limit);
        }
        Ok::<(Vec<RuntimeMatchedChunk>, usize, u128), anyhow::Error>((
            lexical_hits,
            lexical_query_count,
            started.elapsed().as_millis(),
        ))
    };

    let ((vector_hits, vector_elapsed_ms), (lexical_hits, lexical_query_count, lexical_elapsed_ms)) =
        tokio::try_join!(vector_future, lexical_future)?;
    tracing::info!(
        stage = "retrieval.chunks_fanout",
        vector_elapsed_ms,
        vector_hits = vector_hits.len(),
        lexical_elapsed_ms,
        lexical_query_count,
        lexical_hits = lexical_hits.len(),
        "vector + lexical chunk fan-out"
    );
    let mut chunks =
        merge_chunks(vector_hits, lexical_hits, limit.max(initial_table_row_count.unwrap_or(0)));
    let latest_version_chunks =
        load_latest_version_document_chunks(state, question, document_index, plan_keywords).await?;
    if !latest_version_chunks.is_empty() {
        chunks = merge_chunks(
            chunks,
            latest_version_chunks,
            latest_version_context_top_k(question, limit),
        );
    }
    let entity_bio_chunks =
        load_entity_bio_chunks(state, library_id, query_ir, document_index, plan_keywords).await?;
    if !entity_bio_chunks.is_empty() {
        // Cap at limit + the bio budget so entity-bio hits are additive
        // rather than pushing other high-score chunks off the top-K.
        let merged_limit = limit.saturating_add(ENTITY_BIO_CHUNK_CAP);
        chunks = merge_chunks(chunks, entity_bio_chunks, merged_limit);
    }
    // Diversify by document: cap at `MAX_CHUNKS_PER_DOCUMENT` chunks
    // per document_id in the final hit list. Without this, BM25 stem
    // collisions (e.g. «настроен/настроить/настроено» all stemming to
    // `настро`) let one document with the same word repeated dominate
    // top-10, squeezing out other documents that happen to carry the
    // actual answer. The cap preserves per-chunk order (scores stay
    // monotonic inside the surviving set), just drops the long tail
    // from an over-represented document.
    chunks = diversify_chunks_by_document(chunks, MAX_CHUNKS_PER_DOCUMENT);
    if !targeted_document_ids.is_empty() {
        chunks.retain(|chunk| targeted_document_ids.contains(&chunk.document_id));
    }
    if let Some(row_count) = initial_table_row_count {
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            row_count,
            plan_keywords,
        )
        .await?;
        chunks = merge_chunks(chunks, initial_rows, limit.max(row_count));
    }
    if targeted_table_aggregation {
        let direct_summary_chunks = load_table_summary_chunks_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT,
            plan_keywords,
        )
        .await?;
        let direct_row_chunks = load_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_ROW_LIMIT,
            plan_keywords,
        )
        .await?;
        chunks = merge_canonical_table_aggregation_chunks(
            chunks,
            direct_summary_chunks,
            direct_row_chunks,
            limit.max(DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT),
        );
    }

    Ok(chunks)
}

async fn load_latest_version_document_chunks(
    state: &AppState,
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if !question_requests_latest_versions(question) {
        return Ok(Vec::new());
    }
    let requested_count = requested_latest_version_count(question);
    let scope_terms = latest_version_scope_terms(question);
    let documents = latest_version_documents(document_index, requested_count, &scope_terms);
    if documents.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for (rank, document) in documents.into_iter().enumerate() {
        let rows = state
            .arango_document_store
            .list_chunks_by_revision_range(
                document.revision_id,
                0,
                LATEST_VERSION_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load latest-version chunks for document {} revision {}",
                    document.document_id, document.revision_id
                )
            })?;
        for (chunk_rank, row) in
            rows.into_iter().take(LATEST_VERSION_CHUNKS_PER_DOCUMENT).enumerate()
        {
            let score = latest_version_chunk_score(
                DOCUMENT_IDENTITY_SCORE_FLOOR,
                requested_count,
                rank,
                chunk_rank,
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunks.push(chunk);
            }
        }
    }
    Ok(chunks)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LatestVersionDocument {
    pub(crate) document_id: Uuid,
    pub(crate) revision_id: Uuid,
    pub(crate) version: Vec<u32>,
    pub(crate) title: String,
    pub(crate) family_key: String,
}

pub(crate) fn latest_version_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    count: usize,
    scope_terms: &[String],
) -> Vec<LatestVersionDocument> {
    let rows = document_index
        .values()
        .filter(|document| document.document_state == "active")
        .filter_map(|document| {
            let primary_title = document
                .title
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .or(document.file_name.as_deref())?;
            if !text_has_release_version_marker(primary_title) {
                return None;
            }
            let primary_title_lower = primary_title.to_lowercase();
            let version = extract_semver_like_version(&primary_title_lower)?;
            let revision_id = canonical_document_revision_id(document)?;
            let identity_text =
                format!("{primary_title_lower} {}", document.external_key.to_lowercase());
            Some((
                LatestVersionDocument {
                    document_id: document.document_id,
                    revision_id,
                    version,
                    title: primary_title.to_string(),
                    family_key: latest_version_family_key(primary_title),
                },
                identity_text,
            ))
        })
        .collect::<Vec<_>>();
    let scoped_rows = if scope_terms.is_empty() {
        rows
    } else {
        let scoped = rows
            .iter()
            .filter(|(_, identity_text)| {
                scope_terms.iter().any(|term| identity_text.contains(term))
            })
            .cloned()
            .collect::<Vec<_>>();
        if scoped.is_empty() { rows } else { scoped }
    };
    let mut rows = scoped_rows.into_iter().map(|(document, _)| document).collect::<Vec<_>>();
    if count > 1 {
        let family_sizes =
            rows.iter().fold(HashMap::<String, usize>::new(), |mut acc, document| {
                *acc.entry(document.family_key.clone()).or_default() += 1;
                acc
            });
        let top_two_counts = {
            let mut counts = family_sizes.values().copied().collect::<Vec<_>>();
            counts.sort_unstable_by(|left, right| right.cmp(left));
            counts
        };
        if let Some((family_key, family_count)) = family_sizes
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(right.0)))
            .map(|(family_key, count)| (family_key.clone(), *count))
        {
            let runner_up = top_two_counts.get(1).copied().unwrap_or(0);
            if family_count >= count && family_count > runner_up {
                rows.retain(|document| document.family_key == family_key);
            }
        }
    }
    rows.sort_by(|left, right| {
        compare_version_desc(&left.version, &right.version)
            .then_with(|| left.title.cmp(&right.title))
    });
    rows.dedup_by(|left, right| {
        left.version == right.version && left.title.eq_ignore_ascii_case(&right.title)
    });
    rows.truncate(count);
    rows
}

/// Hydrate a bag of `(chunk_id, score)` hits into ranked
/// `RuntimeMatchedChunk` rows with exactly ONE Arango round-trip.
/// The previous `join_all(get_chunk)` pattern turned every hit into a
/// separate coordinator call — on a typical 16-hit vector + 4×20-hit
/// lexical fan-out that was ~100 sequential Arango fetches per
/// grounded_answer turn. Batch hydration collapses them into ≤5.
///
/// Score/order is preserved via an id→score map: `list_chunks_by_ids`
/// returns rows unordered, so we re-zip the scores in a hash lookup
/// instead of relying on the database's ordering.
async fn batch_hydrate_hits(
    state: &AppState,
    hits: Vec<(Uuid, f32)>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    // Build the score lookup and the id list in one pass. Dedupe ids
    // — a hit list can legitimately contain the same chunk across
    // vector and lexical queries before the RRF merge, and we don't
    // want to waste network bytes on duplicate filter args.
    let mut score_by_chunk: HashMap<Uuid, f32> = HashMap::with_capacity(hits.len());
    for (chunk_id, score) in &hits {
        // Keep the best (highest) score if the same chunk appears
        // twice. Ranking downstream expects a single row per chunk.
        score_by_chunk
            .entry(*chunk_id)
            .and_modify(|existing| {
                if *score > *existing {
                    *existing = *score;
                }
            })
            .or_insert(*score);
    }
    let chunk_ids: Vec<Uuid> = score_by_chunk.keys().copied().collect();
    let chunk_rows = state
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to batch-load runtime query chunks")?;
    let mut mapped: Vec<RuntimeMatchedChunk> = Vec::with_capacity(chunk_rows.len());
    for chunk in chunk_rows {
        let Some(score) = score_by_chunk.get(&chunk.chunk_id).copied() else {
            continue;
        };
        if !targeted_document_ids.is_empty() && !targeted_document_ids.contains(&chunk.document_id)
        {
            continue;
        }
        let Some(matched) = map_chunk_hit(chunk, score, document_index, plan_keywords) else {
            continue;
        };
        mapped.push(matched);
    }
    // Preserve score order — the merge/rerank pipeline relies on
    // the hit list coming in "best-first".
    mapped.sort_by(score_desc_chunks);
    Ok(mapped)
}

pub(crate) async fn resolve_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
) -> anyhow::Result<Option<RuntimeVectorSearchContext>> {
    let providers = ai_repository::list_provider_catalog(&state.persistence.postgres)
        .await
        .context("failed to list provider catalog for runtime vector search")?;
    let Some(provider) = providers
        .into_iter()
        .find(|row| row.provider_kind == provider_profile.embedding.provider_kind.as_str())
    else {
        return Ok(None);
    };
    let models = ai_repository::list_model_catalog(&state.persistence.postgres, Some(provider.id))
        .await
        .context("failed to list model catalog for runtime vector search")?;
    let Some(model) =
        models.into_iter().find(|row| row.model_name == provider_profile.embedding.model_name)
    else {
        return Ok(None);
    };

    let Some(generation) = load_latest_library_generation(state, library_id).await? else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    Ok(Some(RuntimeVectorSearchContext { model_catalog_id: model.id }))
}

pub(crate) fn expanded_candidate_limit(
    planned_mode: RuntimeQueryMode,
    top_k: usize,
    rerank_enabled: bool,
    rerank_candidate_limit: usize,
) -> usize {
    if matches!(planned_mode, RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix) {
        let intrinsic_limit = top_k.saturating_mul(3).clamp(top_k, 96);
        if rerank_enabled {
            return intrinsic_limit.max(rerank_candidate_limit);
        }
        return intrinsic_limit;
    }
    top_k
}

/// **Historically** returned `true` for exact-literal-technical queries
/// (ones that look like URL/endpoint/method lookups) so the LLM embed
/// round-trip could be skipped when lexical search alone was deemed
/// sufficient. Prod smoke on short configure-style Russian questions
/// showed the flag was catastrophically over-aggressive — planner classifies
/// many natural-language technical questions as exact-literal (any
/// keyword with underscore / digit / uppercase flips it on), and
/// disabling vector retrieval then caused every relevant chunk with
/// concrete config examples to miss the lexical BM25 ranking
/// (stemming collision on `настро*`) without a vector lane to rescue
/// them. The canonical signal is "always run both lanes, let ranking
/// boost exact matches if they deserve the bump"; this function now
/// always returns `false`.
///
/// The const-fn signature is kept so existing callers don't need to
/// change; the planner's `exact_literal_technical` bit still affects
/// boosts and technical-literal context packing elsewhere.
pub(crate) const fn should_skip_vector_search(_plan: &RuntimeQueryPlan) -> bool {
    false
}

/// Hard cap on the number of lexical AQL searches dispatched to
/// Arango per query. Every additional query is a full
/// `search_chunks` round-trip; with a ~500 ms p50 per query and a
/// 1000+ document corpus, a 10-query fan-out added 5–8 s of
/// retrieval latency even when every query returned zero hits.
/// Eight is the empirical sweet spot — enough to carry both halves
/// of a role-pairing question ("X и отдельно Y") through the lexical
/// path when vector search might miss, while the concurrent
/// `join_all` fan-out keeps wall-clock inside the coordinator's
/// fan-out budget. Anything above 8 returned diminishing recall for
/// order-of-magnitude more latency.
const MAX_LEXICAL_QUERIES: usize = 8;

/// Maximum number of chunks from a single document the retriever is
/// allowed to surface in its final hit list. Two chunks (typically one
/// for context + one for the actual answer) gives the answer model
/// enough signal while preserving top-k diversity. Higher caps let a
/// single over-tokenised document drown out every other candidate.
const MAX_CHUNKS_PER_DOCUMENT: usize = 2;

/// Caps the number of chunks from any single `document_id` in a
/// retrieval result. Preserves the input order (which reflects the
/// caller's merged score ranking): walks the list, admits each chunk
/// only if its document has fewer than `max_per_doc` chunks already
/// admitted. Keeps all single-document results if one only has < N
/// chunks (no silent drop of legitimate results).
fn diversify_chunks_by_document(
    chunks: Vec<RuntimeMatchedChunk>,
    max_per_doc: usize,
) -> Vec<RuntimeMatchedChunk> {
    if max_per_doc == 0 {
        return chunks;
    }
    let mut counts: std::collections::HashMap<Uuid, usize> =
        std::collections::HashMap::with_capacity(chunks.len());
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let count = counts.entry(chunk.document_id).or_insert(0);
        if *count >= max_per_doc {
            continue;
        }
        *count += 1;
        out.push(chunk);
    }
    out
}

pub(crate) fn build_lexical_queries(question: &str, plan: &RuntimeQueryPlan) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();

    let mut push_query = |value: String, queries: &mut Vec<String>| {
        if queries.len() >= MAX_LEXICAL_QUERIES {
            return;
        }
        let normalized = value.trim().to_string();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            return;
        }
        queries.push(normalized);
    };

    // Priority 1 — the raw user question. Arango's full-text
    // analyser already splits it into relevant tokens; this is the
    // highest-signal query and must always go first.
    push_query(question.trim().to_string(), &mut queries);

    // Priority 2 — the plan's combined hi + lo keyword phrase. It
    // strips out noise the user naturally includes ("в каком
    // разделе", "как мне") and leaves the canonical terms.
    push_query(request_safe_query(plan), &mut queries);

    // Priority 3 — for exact-literal technical queries (port numbers,
    // error codes, config keys) AND for role-pairing questions that
    // split into multiple clauses ("X и отдельно Y"), push every
    // focus segment. The segment splitter already recognises
    // " и отдельно " / " and " / ";" delimiters, so a role-pairing
    // Russian question contributes both halves individually.
    if plan.intent_profile.exact_literal_technical
        || super::question_requests_multi_document_scope(question, None)
    {
        // IR is compiled in parallel with retrieval (see
        // `answer_pipeline::prepare_answer_query`), so the lexical query
        // builder cannot see `query_ir` yet. The focus-keyword helper
        // gracefully degrades to plain question tokens when ir is `None`.
        for segment in technical_literal_focus_keyword_segments(question, None) {
            push_query(segment.join(" "), &mut queries);
        }
    }

    // Priority 4 — multi-document role-clause expansions. When the
    // question asks about two roles ("which technology … and which
    // one …", "if a system needs … and also …"), push each normalized
    // clause and a canonical subject label for any clause whose role
    // we recognise. The subject_label is the canonical lowercase name
    // (e.g. "retrieval-augmented generation"), same coverage the
    // removed `query_aliases()` list used to provide.
    if super::question_requests_multi_document_scope(question, None) {
        for clause in super::extract_multi_document_role_clauses(question) {
            push_query(clause.clone(), &mut queries);
            if let Some(target) = super::role_clause_canonical_target(&clause) {
                push_query(target.subject_label().to_lowercase(), &mut queries);
            }
        }
    }

    // Priority 5 — plan-derived keyword variants. Hi/lo splits first
    // (they collapse the user's question to the canonical nouns),
    // then individual keywords for narrow-tail recall on corpora
    // where the vector space is sparse.
    if !plan.high_level_keywords.is_empty() {
        push_query(plan.high_level_keywords.join(" "), &mut queries);
    }
    if !plan.low_level_keywords.is_empty() {
        push_query(plan.low_level_keywords.join(" "), &mut queries);
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "), &mut queries);
    }
    for keyword in plan.keywords.iter().take(MAX_LEXICAL_QUERIES) {
        push_query(keyword.clone(), &mut queries);
    }

    queries
}

pub(crate) fn request_safe_query(plan: &RuntimeQueryPlan) -> String {
    if !plan.low_level_keywords.is_empty() {
        let combined =
            format!("{} {}", plan.high_level_keywords.join(" "), plan.low_level_keywords.join(" "));
        return combined.trim().to_string();
    }
    plan.keywords.join(" ")
}

pub(crate) fn map_chunk_hit(
    chunk: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&chunk.document_id)?;
    let canonical_revision_id = canonical_document_revision_id(document)?;
    if chunk.revision_id != canonical_revision_id {
        return None;
    }
    let source_text = chunk_answer_source_text(&chunk);
    Some(RuntimeMatchedChunk {
        chunk_id: chunk.chunk_id,
        document_id: chunk.document_id,
        revision_id: chunk.revision_id,
        chunk_index: chunk.chunk_index,
        document_label: document
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone()),
        excerpt: focused_excerpt_for(&source_text, keywords, 280),
        score: Some(score),
        source_text,
    })
}

fn chunk_answer_source_text(chunk: &KnowledgeChunkRow) -> String {
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    if chunk.content_text.trim().is_empty() && !chunk.normalized_text.trim().is_empty() {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    repair_technical_layout_noise(&chunk.content_text)
}

fn explicit_target_document_ids(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<Uuid> {
    super::explicit_target_document_ids_from_values(
        question,
        document_index.values().flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        }),
    )
}

pub(crate) fn canonical_document_revision_id(document: &KnowledgeDocumentRow) -> Option<Uuid> {
    document.readable_revision_id.or(document.active_revision_id)
}

pub(crate) fn merge_chunks(
    left: Vec<RuntimeMatchedChunk>,
    right: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(left, right, top_k)
}

/// Reciprocal Rank Fusion: merges two ranked lists into a single ranking.
/// Each document's score is `1/(k + rank_in_list)` summed across both lists.
/// This normalizes across different scoring scales (BM25 vs cosine similarity).
fn rrf_merge_chunks(
    vector_hits: Vec<RuntimeMatchedChunk>,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    const RRF_K: f32 = 60.0;

    let mut rrf_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut raw_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut chunks_by_id: HashMap<Uuid, RuntimeMatchedChunk> = HashMap::new();
    let mut record_hit = |rank: usize, chunk: RuntimeMatchedChunk| {
        let rrf_score = 1.0 / (RRF_K + rank as f32 + 1.0);
        *rrf_scores.entry(chunk.chunk_id).or_default() += rrf_score;
        let raw_score = score_value(chunk.score);
        if raw_score.is_finite() {
            raw_scores
                .entry(chunk.chunk_id)
                .and_modify(|existing| {
                    if raw_score > *existing {
                        *existing = raw_score;
                    }
                })
                .or_insert(raw_score);
        }
        chunks_by_id.entry(chunk.chunk_id).or_insert(chunk);
    };

    // Score vector hits by their rank position
    for (rank, chunk) in vector_hits.into_iter().enumerate() {
        record_hit(rank, chunk);
    }

    // Score lexical hits by their rank position
    for (rank, chunk) in lexical_hits.into_iter().enumerate() {
        record_hit(rank, chunk);
    }

    // Apply RRF scores back to chunks. Exception: dedicated document-
    // identity lanes deliberately emit high-scale scores, and those are
    // not comparable to ordinary BM25/cosine values. Preserve them as a
    // first-class focus signal so downstream consolidation can pack the
    // identified document instead of losing the signal to rank-only RRF.
    let mut values: Vec<RuntimeMatchedChunk> = chunks_by_id
        .into_values()
        .map(|mut chunk| {
            let rrf_score = rrf_scores.get(&chunk.chunk_id).copied();
            let raw_score = raw_scores.get(&chunk.chunk_id).copied();
            chunk.score = match raw_score {
                Some(score) if score >= DOCUMENT_IDENTITY_SCORE_FLOOR => Some(score),
                _ => rrf_score,
            };
            chunk
        })
        .collect();

    values.sort_by(score_desc_chunks);
    values.truncate(top_k);
    values
}

pub(crate) fn score_desc_chunks(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score).total_cmp(&score_value(left.score))
}

pub(crate) fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}

pub(crate) fn truncate_bundle(bundle: &mut RetrievalBundle, top_k: usize) {
    bundle.entities.truncate(top_k);
    bundle.relationships.truncate(top_k);
    bundle.chunks.truncate(top_k);
}

pub(crate) fn excerpt_for(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let excerpt = trimmed.chars().take(max_chars).collect::<String>();
    format!("{excerpt}...")
}

pub(crate) fn focused_excerpt_for(content: &str, keywords: &[String], max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let lines = trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let normalized_keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect::<Vec<_>>();
    if normalized_keywords.is_empty() {
        return excerpt_for(trimmed, max_chars);
    }

    let mut best_index = None;
    let mut best_score = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let lowered = line.to_lowercase();
        let score = normalized_keywords
            .iter()
            .filter(|keyword| lowered.contains(keyword.as_str()))
            .map(|keyword| keyword.chars().count().min(24))
            .sum::<usize>();
        if score > best_score {
            best_score = score;
            best_index = Some(index);
        }
    }

    let Some(center_index) = best_index else {
        return excerpt_for(trimmed, max_chars);
    };
    if best_score == 0 {
        return excerpt_for(trimmed, max_chars);
    }

    let max_focus_lines = 5usize;
    let mut selected = BTreeSet::from([center_index]);
    let mut radius = 1usize;
    loop {
        let excerpt =
            selected.iter().copied().map(|index| lines[index]).collect::<Vec<_>>().join(" ");
        if excerpt.chars().count() >= max_chars
            || selected.len() >= max_focus_lines
            || selected.len() == lines.len()
        {
            return excerpt_for(&excerpt, max_chars);
        }

        let mut expanded = false;
        if center_index >= radius {
            expanded |= selected.insert(center_index - radius);
        }
        if center_index + radius < lines.len() {
            expanded |= selected.insert(center_index + radius);
        }
        if !expanded {
            return excerpt_for(&excerpt, max_chars);
        }
        radius += 1;
    }
}

#[cfg(test)]
#[path = "retrieve_tests.rs"]
mod tests;
