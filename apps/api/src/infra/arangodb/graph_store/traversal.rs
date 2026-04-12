use super::*;

impl ArangoGraphStore {
    pub async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>> {
        let query = format!(
            "FOR relation IN {relation_collection}
             FILTER relation.library_id == @library_id
             LET subject = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {subject_edge}
                  FILTER entity.library_id == @library_id
                  LIMIT 1
                  RETURN entity
             )
             LET object = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {object_edge}
                  FILTER entity.library_id == @library_id
                  LIMIT 1
                  RETURN entity
             )
             FILTER subject != null AND object != null
             SORT relation.support_count DESC, relation.updated_at DESC, relation.relation_id DESC
             LIMIT 10000
             RETURN MERGE(
                relation,
                {{
                  subject_entity_id: subject.entity_id,
                  object_entity_id: object.entity_id
                }}
             )",
            relation_collection = KNOWLEDGE_RELATION_COLLECTION,
            subject_edge = KNOWLEDGE_RELATION_SUBJECT_EDGE,
            object_edge = KNOWLEDGE_RELATION_OBJECT_EDGE,
        );
        let cursor = self
            .client
            .query_json(
                &query,
                serde_json::json!({
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge relation topology by library")?;
        decode_many_results(cursor)
    }

    pub async fn list_document_graph_links_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeDocumentGraphLinkRow>> {
        let query = format!(
            "FOR document IN {document_collection}
               FILTER document.library_id == @library_id
                 AND document.deleted_at == null
               LET revision_id = document.active_revision_id != null
                 ? document.active_revision_id
                 : document.readable_revision_id
               FILTER revision_id != null
               LET revision_vertex_id = CONCAT(@revision_collection, '/', revision_id)
               LET mention_rows = (
                 FOR chunk IN OUTBOUND revision_vertex_id {revision_chunk_edge_collection}
                   FILTER chunk.library_id == @library_id
                   FOR entity, edge IN OUTBOUND chunk._id {mention_edge_collection}
                     FILTER entity != null
                       AND entity.library_id == @library_id
                     COLLECT target_node_id = entity.entity_id
                     AGGREGATE mention_count = COUNT(1)
                     RETURN {{
                        document_id: document.document_id,
                        target_node_id,
                        target_node_type: \"entity\",
                        mention_count,
                        support_count: 0
                     }}
               )
               LET evidence_rows = (
                 FOR evidence IN INBOUND revision_vertex_id {evidence_source_edge_collection}
                   FILTER evidence != null
                     AND evidence.library_id == @library_id
                   LET entity_rows = (
                     FOR entity, edge IN OUTBOUND evidence._id {evidence_support_entity_edge_collection}
                       FILTER entity != null
                         AND entity.library_id == @library_id
                       COLLECT target_node_id = entity.entity_id
                       AGGREGATE support_count = COUNT(1)
                       RETURN {{
                          document_id: document.document_id,
                          target_node_id,
                          target_node_type: \"entity\",
                          mention_count: 0,
                          support_count
                       }}
                   )
                   LET relation_rows = (
                     FOR relation, edge IN OUTBOUND evidence._id {evidence_support_relation_edge_collection}
                       FILTER relation != null
                         AND relation.library_id == @library_id
                       COLLECT target_node_id = relation.relation_id
                       AGGREGATE support_count = COUNT(1)
                       RETURN {{
                          document_id: document.document_id,
                          target_node_id,
                          target_node_type: \"topic\",
                          mention_count: 0,
                          support_count
                       }}
                   )
                   RETURN UNION(entity_rows, relation_rows)
               )
               LET rows = APPEND(mention_rows, FLATTEN(evidence_rows))
               FOR row IN rows
                 COLLECT
                   document_id = row.document_id,
                   target_node_id = row.target_node_id,
                   target_node_type = row.target_node_type
                 AGGREGATE
                   mention_count = SUM(row.mention_count),
                   support_count = SUM(row.support_count)
                 LET total_support_count = mention_count + support_count
                 FILTER total_support_count > 0
                 LET relation_type =
                   target_node_type == \"entity\" && mention_count > 0 ? \"mentions\" : \"supports\"
                 SORT total_support_count DESC, document_id ASC, target_node_type ASC, target_node_id ASC
                 RETURN {{
                    document_id,
                    target_node_id,
                    target_node_type,
                    relation_type,
                    support_count: total_support_count
                 }}",
            document_collection = KNOWLEDGE_DOCUMENT_COLLECTION,
            revision_chunk_edge_collection = KNOWLEDGE_REVISION_CHUNK_EDGE,
            evidence_source_edge_collection = KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            mention_edge_collection = KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            evidence_support_entity_edge_collection = KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            evidence_support_relation_edge_collection = KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
        );
        let cursor = self
            .client
            .query_json(
                &query,
                serde_json::json!({
                    "library_id": library_id,
                    "revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                }),
            )
            .await
            .context("failed to list knowledge document graph links")?;
        decode_many_results(cursor)
    }

    pub async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>> {
        let query = format!(
            "FOR relation IN {relation_collection}
             FILTER relation.relation_id == @relation_id
             LET subject = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {subject_edge}
                  LIMIT 1
                  RETURN entity
             )
             LET object = FIRST(
                FOR entity IN OUTBOUND CONCAT(\"{relation_collection}/\", relation.relation_id) {object_edge}
                  LIMIT 1
                  RETURN entity
             )
             FILTER subject != null AND object != null
             LIMIT 1
             RETURN MERGE(
                relation,
                {{
                  subject_entity_id: subject.entity_id,
                  object_entity_id: object.entity_id
                }}
             )",
            relation_collection = KNOWLEDGE_RELATION_COLLECTION,
            subject_edge = KNOWLEDGE_RELATION_SUBJECT_EDGE,
            object_edge = KNOWLEDGE_RELATION_OBJECT_EDGE,
        );
        let cursor = self
            .client
            .query_json(
                &query,
                serde_json::json!({
                    "relation_id": relation_id,
                }),
            )
            .await
            .context("failed to get knowledge relation topology by id")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vertex, edge, path IN 0..@max_depth ANY @start_vertex GRAPH @graph_name
                 OPTIONS { bfs: true, uniqueVertices: \"global\" }
                 FILTER HAS(vertex, \"library_id\")
                   AND vertex.library_id == @library_id
                 LET vertex_kind = PARSE_IDENTIFIER(vertex._id).collection
                 FILTER vertex_kind == @entity_collection
                    OR vertex_kind == @relation_collection
                    OR vertex_kind == @evidence_collection
                    OR vertex_kind == @chunk_collection
                    OR vertex_kind == @revision_collection
                    OR vertex_kind == @document_collection
                 LET vertex_id = vertex_kind == @entity_collection ? vertex.entity_id :
                     vertex_kind == @relation_collection ? vertex.relation_id :
                     vertex_kind == @evidence_collection ? vertex.evidence_id :
                     vertex_kind == @chunk_collection ? vertex.chunk_id :
                     vertex_kind == @revision_collection ? vertex.revision_id :
                     vertex_kind == @document_collection ? vertex.document_id :
                     null
                 FILTER vertex_id != null
                 SORT LENGTH(path.vertices) ASC, vertex_kind ASC, vertex_id ASC
                 LIMIT @limit
                 RETURN {
                    path_length: LENGTH(path.vertices) - 1,
                    vertex_kind,
                    vertex_id,
                    edge_kind: edge == null ? null : PARSE_IDENTIFIER(edge._id).collection,
                    edge_key: edge == null ? null : edge._key,
                    edge_rank: edge == null ? null : edge.rank,
                    edge_score: edge == null ? null : edge.score,
                    edge_inclusion_reason: edge == null ? null : edge.inclusionReason,
                    vertex
                }",
                serde_json::json!({
                    "graph_name": KNOWLEDGE_GRAPH_NAME,
                    "start_vertex": format!("{}/{}", KNOWLEDGE_ENTITY_COLLECTION, entity_id),
                    "library_id": library_id,
                    "max_depth": max_depth.max(1),
                    "limit": limit.max(1),
                    "entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                    "document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                }),
            )
            .await
            .context("failed to list knowledge entity neighborhood")?;
        decode_many_results(cursor)
    }

    pub async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vertex, edge, path IN 0..@max_depth ANY @start_vertex GRAPH @graph_name
                 OPTIONS { bfs: true, uniqueVertices: \"global\" }
                 FILTER HAS(vertex, \"library_id\")
                   AND vertex.library_id == @library_id
                 LET vertex_kind = PARSE_IDENTIFIER(vertex._id).collection
                 FILTER vertex_kind == @entity_collection
                    OR vertex_kind == @relation_collection
                    OR vertex_kind == @evidence_collection
                    OR vertex_kind == @chunk_collection
                    OR vertex_kind == @revision_collection
                    OR vertex_kind == @document_collection
                 LET vertex_id = vertex_kind == @entity_collection ? vertex.entity_id :
                     vertex_kind == @relation_collection ? vertex.relation_id :
                     vertex_kind == @evidence_collection ? vertex.evidence_id :
                     vertex_kind == @chunk_collection ? vertex.chunk_id :
                     vertex_kind == @revision_collection ? vertex.revision_id :
                     vertex_kind == @document_collection ? vertex.document_id :
                     null
                 FILTER vertex_id != null
                 SORT LENGTH(path.vertices) ASC, vertex_kind ASC, vertex_id ASC
                 LIMIT @limit
                 RETURN {
                    path_length: LENGTH(path.vertices) - 1,
                    vertex_kind,
                    vertex_id,
                    edge_kind: edge == null ? null : PARSE_IDENTIFIER(edge._id).collection,
                    edge_key: edge == null ? null : edge._key,
                    edge_rank: edge == null ? null : edge.rank,
                    edge_score: edge == null ? null : edge.score,
                    edge_inclusion_reason: edge == null ? null : edge.inclusionReason,
                    vertex
                }",
                serde_json::json!({
                    "graph_name": KNOWLEDGE_GRAPH_NAME,
                    "start_vertex": format!("{}/{}", KNOWLEDGE_RELATION_COLLECTION, relation_id),
                    "library_id": library_id,
                    "max_depth": max_depth.max(1),
                    "limit": limit.max(1),
                    "entity_collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                    "document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                }),
            )
            .await
            .context("failed to expand knowledge relation-centric neighborhood")?;
        decode_many_results(cursor)
    }

    pub async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@relation_collection
                 FILTER relation.relation_id == @relation_id
                   AND relation.library_id == @library_id
                 FOR evidence, edge, path IN 1..1 INBOUND relation._id GRAPH @graph_name
                 FILTER PARSE_IDENTIFIER(evidence._id).collection == @evidence_collection
                 SORT edge.rank ASC, edge.created_at ASC, evidence.created_at ASC, evidence.evidence_id ASC
                 LIMIT @limit
                 LET source_document = FIRST(
                    FOR document IN @@document_collection
                      FILTER document.document_id == evidence.document_id
                      LIMIT 1
                      RETURN document
                 )
                 LET source_revision = FIRST(
                    FOR revision IN @@revision_collection
                      FILTER revision.revision_id == evidence.revision_id
                      LIMIT 1
                      RETURN revision
                 )
                 LET source_chunk = FIRST(
                    FOR chunk IN @@chunk_collection
                      FILTER evidence.chunk_id != null
                        AND chunk.chunk_id == evidence.chunk_id
                      LIMIT 1
                      RETURN chunk
                 )
                 RETURN {
                    relation,
                    evidence,
                    support_edge_rank: edge.rank,
                    support_edge_score: edge.score,
                    support_edge_inclusion_reason: edge.inclusionReason,
                    source_document,
                    source_revision,
                    source_chunk
                }",
                serde_json::json!({
                    "graph_name": KNOWLEDGE_GRAPH_NAME,
                    "@relation_collection": KNOWLEDGE_RELATION_COLLECTION,
                    "evidence_collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "@document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "@revision_collection": KNOWLEDGE_REVISION_COLLECTION,
                    "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "relation_id": relation_id,
                    "library_id": library_id,
                    "limit": limit.max(1),
                }),
            )
            .await
            .context("failed to lookup evidence-backed knowledge relation")?;
        decode_many_results(cursor)
    }
}
