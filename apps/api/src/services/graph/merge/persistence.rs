use super::*;
use anyhow::Result as AnyhowResult;

fn entity_node_input(
    entity: &crate::services::graph::extract::GraphEntityCandidate,
    chunk: &ChunkRow,
    identity_key: &str,
    existing: Option<&RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> repositories::RuntimeGraphNodeUpsertInput {
    let mut entity_aliases = entity.aliases.clone();
    entity_aliases.extend(
        crate::services::graph::extract::acronym_gloss::detect_acronym_aliases_for_label(
            &chunk.content,
            &entity.label,
        ),
    );
    let aliases = normalize_graph_aliases(&entity.label, &entity_aliases);
    let node_type_from_identity_key =
        crate::services::graph::identity::runtime_node_type_from_key(identity_key);
    let mut metadata = merge_graph_quality_metadata(
        existing.map(|row| &row.metadata_json),
        extraction_recovery,
        entity.summary.as_deref(),
    );
    if let Some(sub_type) = entity.sub_type.as_deref()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("sub_type".to_string(), serde_json::Value::String(sub_type.to_string()));
    }
    repositories::RuntimeGraphNodeUpsertInput {
        identity_key: identity_key.to_string(),
        label: entity.label.trim().to_string(),
        node_type: crate::services::graph::identity::runtime_node_type_slug(
            &node_type_from_identity_key,
        )
        .to_string(),
        aliases_json: serde_json::to_value(aliases).unwrap_or_else(|_| serde_json::json!([])),
        summary: entity.summary.clone(),
        metadata_json: metadata,
        support_count: existing.map_or(1, |row| row.support_count.max(1)),
    }
}

pub(super) fn partition_relations<'a>(
    graph_quality_guard: &GraphQualityGuardService,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    candidates: &'a GraphExtractionCandidateSet,
) -> RelationPartition<'a> {
    let mut filtered = Vec::new();
    let mut admitted = Vec::new();
    for relation in &candidates.relations {
        let source_key = entity_key_index.node_identity_key_for_label(&relation.source_label);
        let target_key = entity_key_index.node_identity_key_for_label(&relation.target_label);
        if let Some(reason) =
            graph_quality_guard.filter_reason(&source_key, &target_key, &relation.relation_type)
        {
            filtered.push(FilteredRelation {
                relation,
                source_key,
                target_key,
                reason_label: artifact_filter_reason_label(reason),
            });
        } else {
            admitted.push(AdmittedRelation { relation, source_key, target_key });
        }
    }
    RelationPartition { filtered, admitted }
}

const fn artifact_filter_reason_label(
    reason: crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason,
) -> &'static str {
    match reason {
        crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::EmptyRelation => {
            "empty_relation"
        }
        crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::DegenerateSelfLoop => {
            "degenerate_self_loop"
        }
        crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::LowValueArtifact => {
            "low_value_artifact"
        }
    }
}

fn relation_endpoint_input(
    identity_key: &str,
    label: &str,
    existing: Option<&RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> repositories::RuntimeGraphNodeUpsertInput {
    let node_type = crate::services::graph::identity::runtime_node_type_from_key(identity_key);
    let aliases = normalize_graph_aliases(label, &[label.to_string()]);
    repositories::RuntimeGraphNodeUpsertInput {
        identity_key: identity_key.to_string(),
        label: label.trim().to_string(),
        node_type: crate::services::graph::identity::runtime_node_type_slug(&node_type).to_string(),
        aliases_json: serde_json::to_value(aliases).unwrap_or_else(|_| serde_json::json!([])),
        summary: None,
        metadata_json: merge_graph_quality_metadata(
            existing.map(|row| &row.metadata_json),
            extraction_recovery,
            None,
        ),
        support_count: existing.map_or(1, |row| row.support_count.max(1)),
    }
}

fn relation_endpoint(
    nodes_by_key: &HashMap<String, RuntimeGraphNodeRow>,
    identity_key: &str,
    role: &str,
) -> std::result::Result<RuntimeGraphNodeRow, GraphServiceError> {
    nodes_by_key.get(identity_key).cloned().ok_or_else(|| {
        GraphServiceError::Internal(anyhow::anyhow!(
            "bulk upsert did not return {role} node for relation endpoint {identity_key}"
        ))
    })
}

#[must_use]
pub(super) fn build_entity_key_index(
    candidates: &GraphExtractionCandidateSet,
) -> crate::services::graph::identity::GraphLabelNodeTypeIndex {
    let mut index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    for entity in &candidates.entities {
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type);
    }
    index
}

pub(super) fn graph_edge_integrity_skip_reason(
    scope: &GraphMergeScope,
    from_node: &RuntimeGraphNodeRow,
    to_node: &RuntimeGraphNodeRow,
) -> Option<&'static str> {
    if from_node.id.is_nil() || to_node.id.is_nil() {
        return Some("missing_node_id");
    }
    let from_library_id = from_node.library_id;
    let to_library_id = to_node.library_id;
    if from_library_id != scope.library_id || to_library_id != scope.library_id {
        return Some("cross_library_node_reference");
    }
    if from_node.projection_version != scope.projection_version
        || to_node.projection_version != scope.projection_version
    {
        return Some("projection_version_mismatch");
    }
    None
}

pub(super) async fn preload_existing_nodes_for_merge_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    candidates: &GraphExtractionCandidateSet,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
) -> AnyhowResult<HashMap<String, RuntimeGraphNodeRow>> {
    let mut identity_keys = BTreeSet::<String>::new();
    identity_keys.insert(format!("document:{}", document.id));
    for entity in &candidates.entities {
        identity_keys.insert(entity_key_index.node_identity_key_for_label(&entity.label));
    }
    for relation in &candidates.relations {
        identity_keys.insert(entity_key_index.node_identity_key_for_label(&relation.source_label));
        identity_keys.insert(entity_key_index.node_identity_key_for_label(&relation.target_label));
    }
    let rows = repositories::list_runtime_graph_nodes_by_identity_keys_with_executor(
        &mut *connection,
        scope.library_id,
        &identity_keys.into_iter().collect::<Vec<_>>(),
        scope.projection_version,
    )
    .await?;
    Ok(rows.into_iter().map(|row| (row.identity_key.clone(), row)).collect())
}

pub(super) async fn upsert_document_node_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    preloaded_existing: &HashMap<String, RuntimeGraphNodeRow>,
) -> AnyhowResult<RuntimeGraphNodeRow> {
    let label = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&document.external_key);
    let identity_key = format!("document:{}", document.id);
    let support_count =
        preloaded_existing.get(&identity_key).map_or(1, |row| row.support_count.max(1));
    repositories::upsert_runtime_graph_document_node_with_executor(
        &mut *connection,
        repositories::RuntimeGraphDocumentNodeInput {
            library_id: scope.library_id,
            document_id: document.id,
            identity_key: &identity_key,
            label,
            aliases_json: serde_json::json!([label, document.external_key.clone()]),
            summary: Some("Source document node"),
            metadata_json: serde_json::json!({
                "document_id": document.id,
                "mime_type": document.mime_type,
            }),
            support_count,
            projection_version: scope.projection_version,
        },
    )
    .await
    .map_err(Into::into)
}

/// Node-upsert inputs for one chunk's extracted entities.
///
/// Groups the chunk/candidate rows with the preloaded key index and existing
/// nodes so the bulk-upsert entry point stays within the argument budget.
pub(super) struct EntityNodeUpsertInputs<'a> {
    pub(super) chunk: &'a ChunkRow,
    pub(super) candidates: &'a GraphExtractionCandidateSet,
    pub(super) entity_key_index: &'a crate::services::graph::identity::GraphLabelNodeTypeIndex,
    pub(super) preloaded_existing: &'a HashMap<String, RuntimeGraphNodeRow>,
    pub(super) extraction_recovery: Option<&'a ExtractionRecoverySummary>,
}

pub(super) async fn upsert_entity_nodes_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    input: EntityNodeUpsertInputs<'_>,
) -> std::result::Result<EntityMergeResult, GraphServiceError> {
    let EntityNodeUpsertInputs {
        chunk,
        candidates,
        entity_key_index,
        preloaded_existing,
        extraction_recovery,
    } = input;
    let mut inputs = Vec::with_capacity(candidates.entities.len());
    let mut identity_keys = Vec::with_capacity(candidates.entities.len());
    let mut seen_keys = BTreeSet::new();
    for entity in &candidates.entities {
        let identity_key = entity_key_index.node_identity_key_for_label(&entity.label);
        identity_keys.push(identity_key.clone());
        if seen_keys.insert(identity_key.clone()) {
            inputs.push(entity_node_input(
                entity,
                chunk,
                &identity_key,
                preloaded_existing.get(&identity_key),
                extraction_recovery,
            ));
        }
    }
    let rows = repositories::bulk_upsert_runtime_graph_nodes_with_executor(
        &mut *connection,
        scope.library_id,
        scope.projection_version,
        &inputs,
    )
    .await?;
    Ok(EntityMergeResult {
        nodes_by_key: rows.into_iter().map(|row| (row.identity_key.clone(), row)).collect(),
        identity_keys,
    })
}

pub(super) async fn upsert_entity_mention_edges_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    document_node: &RuntimeGraphNodeRow,
    entity_merge: &EntityMergeResult,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<Vec<(RuntimeGraphNodeRow, EdgePersistenceOutcome)>, GraphServiceError> {
    let mut results = Vec::new();
    for key in &entity_merge.identity_keys {
        let Some(entity_node) = entity_merge.nodes_by_key.get(key).cloned() else {
            continue;
        };
        let edge = upsert_graph_edge_with_connection(
            connection,
            scope,
            GraphEdgeUpsertParams {
                from_node: document_node,
                to_node: &entity_node,
                relation_type: "mentions",
                summary: Some("Document mentions extracted entity"),
                extraction_recovery,
            },
        )
        .await?;
        results.push((entity_node, edge));
    }
    Ok(results)
}

pub(super) async fn persist_filtered_relations_with_connection(
    connection: &mut sqlx::PgConnection,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    chunk: &ChunkRow,
    relations: &[FilteredRelation<'_>],
) -> std::result::Result<usize, GraphServiceError> {
    for filtered in relations {
        let relation = filtered.relation;
        repositories::create_runtime_graph_filtered_artifact_with_executor(
            &mut *connection,
            repositories::RuntimeGraphFilteredArtifactInput {
                library_id: scope.library_id,
                ingestion_run_id: scope.activated_by_attempt_id,
                revision_id: scope.revision_id,
                target_kind: "edge",
                candidate_key: &crate::services::graph::identity::graph_edge_identity_key(
                    &filtered.source_key,
                    &relation.relation_type,
                    &filtered.target_key,
                ),
                source_node_key: Some(filtered.source_key.as_str()),
                target_node_key: Some(filtered.target_key.as_str()),
                relation_type: Some(
                    &graph_quality_guard.normalized_relation_type(&relation.relation_type),
                ),
                filter_reason: filtered.reason_label,
                summary: relation.summary.as_deref(),
                metadata_json: serde_json::json!({
                    "document_id": document.id,
                    "chunk_id": chunk.id,
                    "source_label": &relation.source_label,
                    "target_label": &relation.target_label,
                    "raw_relation_type": &relation.relation_type,
                    "source_file_name": &document.external_key,
                }),
            },
        )
        .await?;
    }
    Ok(relations.len())
}

pub(super) async fn upsert_relation_endpoint_nodes_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    relations: &[AdmittedRelation<'_>],
    mut nodes_by_key: HashMap<String, RuntimeGraphNodeRow>,
    preloaded_existing: &HashMap<String, RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<HashMap<String, RuntimeGraphNodeRow>, GraphServiceError> {
    let mut inputs = Vec::new();
    let mut seen_keys = BTreeSet::new();
    for admitted in relations {
        for (key, label) in [
            (admitted.source_key.as_str(), admitted.relation.source_label.as_str()),
            (admitted.target_key.as_str(), admitted.relation.target_label.as_str()),
        ] {
            if !nodes_by_key.contains_key(key) && seen_keys.insert(key.to_string()) {
                inputs.push(relation_endpoint_input(
                    key,
                    label,
                    preloaded_existing.get(key),
                    extraction_recovery,
                ));
            }
        }
    }
    let rows = repositories::bulk_upsert_runtime_graph_nodes_with_executor(
        &mut *connection,
        scope.library_id,
        scope.projection_version,
        &inputs,
    )
    .await?;
    nodes_by_key.extend(rows.into_iter().map(|row| (row.identity_key.clone(), row)));
    Ok(nodes_by_key)
}

pub(super) async fn upsert_admitted_relation_edges_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    relations: &[AdmittedRelation<'_>],
    nodes_by_key: &HashMap<String, RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<Vec<RelationMergeResult>, GraphServiceError> {
    let mut results = Vec::with_capacity(relations.len());
    for admitted in relations {
        let source_node = relation_endpoint(nodes_by_key, &admitted.source_key, "source")?;
        let target_node = relation_endpoint(nodes_by_key, &admitted.target_key, "target")?;
        let edge = upsert_graph_edge_with_connection(
            connection,
            scope,
            GraphEdgeUpsertParams {
                from_node: &source_node,
                to_node: &target_node,
                relation_type: &admitted.relation.relation_type,
                summary: admitted.relation.summary.as_deref(),
                extraction_recovery,
            },
        )
        .await?;
        results.push(match edge {
            EdgePersistenceOutcome::Admitted(edge) => RelationMergeResult::Admitted {
                source_node: Box::new(source_node),
                target_node: Box::new(target_node),
                edge,
            },
            EdgePersistenceOutcome::Filtered => RelationMergeResult::Filtered {
                source_node: Box::new(source_node),
                target_node: Box::new(target_node),
            },
        });
    }
    Ok(results)
}

/// Endpoint and relation metadata for one runtime graph edge upsert.
struct GraphEdgeUpsertParams<'a> {
    from_node: &'a RuntimeGraphNodeRow,
    to_node: &'a RuntimeGraphNodeRow,
    relation_type: &'a str,
    summary: Option<&'a str>,
    extraction_recovery: Option<&'a ExtractionRecoverySummary>,
}

async fn upsert_graph_edge_with_connection(
    connection: &mut sqlx::PgConnection,
    scope: &GraphMergeScope,
    params: GraphEdgeUpsertParams<'_>,
) -> AnyhowResult<EdgePersistenceOutcome> {
    let GraphEdgeUpsertParams { from_node, to_node, relation_type, summary, extraction_recovery } =
        params;
    let normalized_relation_type =
        crate::services::graph::identity::normalize_relation_type(relation_type);
    if normalized_relation_type.is_empty() {
        return Ok(EdgePersistenceOutcome::Filtered);
    }
    let identity_key = crate::services::graph::identity::graph_edge_identity_key(
        &from_node.identity_key,
        &normalized_relation_type,
        &to_node.identity_key,
    );
    if let Some(reason) = graph_edge_integrity_skip_reason(scope, from_node, to_node) {
        repositories::create_runtime_graph_filtered_artifact_with_executor(
            &mut *connection,
            repositories::RuntimeGraphFilteredArtifactInput {
                library_id: scope.library_id,
                ingestion_run_id: scope.activated_by_attempt_id,
                revision_id: scope.revision_id,
                target_kind: "edge",
                candidate_key: &identity_key,
                source_node_key: Some(&from_node.identity_key),
                target_node_key: Some(&to_node.identity_key),
                relation_type: Some(&normalized_relation_type),
                filter_reason: "graph_persistence_integrity",
                summary,
                metadata_json: serde_json::json!({
                    "skip_reason": reason,
                    "from_node_id": from_node.id,
                    "to_node_id": to_node.id,
                    "from_projection_version": from_node.projection_version,
                    "to_projection_version": to_node.projection_version,
                    "expected_projection_version": scope.projection_version,
                }),
            },
        )
        .await?;
        return Ok(EdgePersistenceOutcome::Filtered);
    }
    let existing = repositories::get_runtime_graph_edge_by_key_with_executor(
        &mut *connection,
        scope.library_id,
        &identity_key,
        scope.projection_version,
    )
    .await?;
    let support_count = existing.as_ref().map_or(1, |row| row.support_count.max(1));
    repositories::upsert_runtime_graph_edge_with_executor(
        &mut *connection,
        repositories::RuntimeGraphEdgeInput {
            library_id: scope.library_id,
            from_node_id: from_node.id,
            to_node_id: to_node.id,
            relation_type: &normalized_relation_type,
            identity_key: &identity_key,
            summary,
            weight: None,
            support_count,
            metadata_json: merge_graph_quality_metadata(
                existing.as_ref().map(|row| &row.metadata_json),
                extraction_recovery,
                summary,
            ),
            projection_version: scope.projection_version,
        },
    )
    .await
    .map(|edge| EdgePersistenceOutcome::Admitted(Box::new(edge)))
    .map_err(Into::into)
}
