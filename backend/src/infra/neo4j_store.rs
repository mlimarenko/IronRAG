use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use neo4rs::{ConfigBuilder, Graph, query};
use uuid::Uuid;

use crate::{
    app::config::Settings,
    infra::graph_store::{
        GraphProjectionData, GraphProjectionEdgeWrite, GraphProjectionNodeWrite, GraphStore,
    },
};

#[derive(Clone)]
pub struct Neo4jStore {
    graph: Arc<Graph>,
}

impl Neo4jStore {
    pub fn connect(settings: &Settings) -> anyhow::Result<Self> {
        let config = ConfigBuilder::default()
            .uri(settings.neo4j_uri.clone())
            .user(settings.neo4j_username.clone())
            .password(settings.neo4j_password.clone())
            .db(settings.neo4j_database.clone())
            .max_connections(settings.neo4j_max_connections)
            .build()?;
        let graph = Graph::connect(config)?;
        Ok(Self { graph: Arc::new(graph) })
    }

    async fn write_projection_node(
        txn: &mut neo4rs::Txn,
        library_id: &str,
        projection_version: i64,
        node: &GraphProjectionNodeWrite,
    ) -> anyhow::Result<()> {
        txn.run(
            query(
                "MERGE (n:RustRAGNode {
                    library_id: $library_id,
                    projection_version: $projection_version,
                    node_id: $node_id
                 })
                 SET n.canonical_key = $canonical_key,
                     n.label = $label,
                     n.node_type = $node_type,
                     n.support_count = $support_count,
                     n.summary = $summary,
                     n.aliases = $aliases,
                     n.metadata_json = $metadata_json",
            )
            .param("library_id", library_id.to_string())
            .param("projection_version", projection_version)
            .param("node_id", node.node_id.to_string())
            .param("canonical_key", node.canonical_key.clone())
            .param("label", node.label.clone())
            .param("node_type", node.node_type.clone())
            .param("support_count", i64::from(node.support_count))
            .param("summary", node.summary.clone().unwrap_or_default())
            .param("aliases", node.aliases.clone())
            .param("metadata_json", node.metadata_json.to_string()),
        )
        .await
        .with_context(|| format!("failed to project node {}", node.node_id))
    }

    async fn write_projection_edge(
        txn: &mut neo4rs::Txn,
        library_id: &str,
        projection_version: i64,
        edge: &GraphProjectionEdgeWrite,
    ) -> anyhow::Result<()> {
        txn.run(
            query(
                "MATCH (source:RustRAGNode {
                    library_id: $library_id,
                    projection_version: $projection_version,
                    node_id: $from_node_id
                 })
                 MATCH (target:RustRAGNode {
                    library_id: $library_id,
                    projection_version: $projection_version,
                    node_id: $to_node_id
                 })
                 MERGE (source)-[r:RUNTIME_RELATION {
                    library_id: $library_id,
                    projection_version: $projection_version,
                    edge_id: $edge_id
                 }]->(target)
                 SET r.relation_type = $relation_type,
                     r.canonical_key = $canonical_key,
                     r.support_count = $support_count,
                     r.summary = $summary,
                     r.weight = $weight,
                     r.metadata_json = $metadata_json",
            )
            .param("library_id", library_id.to_string())
            .param("projection_version", projection_version)
            .param("edge_id", edge.edge_id.to_string())
            .param("from_node_id", edge.from_node_id.to_string())
            .param("to_node_id", edge.to_node_id.to_string())
            .param("relation_type", edge.relation_type.clone())
            .param("canonical_key", edge.canonical_key.clone())
            .param("support_count", i64::from(edge.support_count))
            .param("summary", edge.summary.clone().unwrap_or_default())
            .param("weight", edge.weight.unwrap_or_default())
            .param("metadata_json", edge.metadata_json.to_string()),
        )
        .await
        .with_context(|| format!("failed to project edge {}", edge.edge_id))
    }
}

#[async_trait]
impl GraphStore for Neo4jStore {
    fn backend_name(&self) -> &'static str {
        "neo4j"
    }

    async fn ping(&self) -> anyhow::Result<()> {
        self.graph.run(query("RETURN 1")).await?;
        Ok(())
    }

    async fn replace_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
        nodes: &[GraphProjectionNodeWrite],
        edges: &[GraphProjectionEdgeWrite],
    ) -> anyhow::Result<()> {
        let library_id = library_id.to_string();
        let mut txn = self.graph.start_txn().await?;

        txn.run(
            query(
                "MATCH (n:RustRAGNode {
                    library_id: $library_id,
                    projection_version: $projection_version
                 })
                 DETACH DELETE n",
            )
            .param("library_id", library_id.clone())
            .param("projection_version", projection_version),
        )
        .await
        .context("failed to clear target Neo4j projection version")?;

        for node in nodes {
            Self::write_projection_node(&mut txn, &library_id, projection_version, node).await?;
        }
        for edge in edges {
            Self::write_projection_edge(&mut txn, &library_id, projection_version, edge).await?;
        }

        txn.run(
            query(
                "MATCH (n:RustRAGNode {library_id: $library_id})
                 WHERE n.projection_version <> $projection_version
                 DETACH DELETE n",
            )
            .param("library_id", library_id)
            .param("projection_version", projection_version),
        )
        .await
        .context("failed to purge stale Neo4j projection versions")?;

        txn.commit().await.context("failed to commit Neo4j projection transaction")
    }

    async fn load_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphProjectionData> {
        let library_id = library_id.to_string();
        let mut node_stream = self
            .graph
            .execute(
                query(
                    "MATCH (n:RustRAGNode {
                        library_id: $library_id,
                        projection_version: $projection_version
                     })
                     RETURN
                        n.node_id as node_id,
                        n.canonical_key as canonical_key,
                        n.label as label,
                        n.node_type as node_type,
                        n.support_count as support_count,
                        n.summary as summary,
                        coalesce(n.aliases, []) as aliases,
                        coalesce(n.metadata_json, '{}') as metadata_json
                     ORDER BY n.node_type ASC, n.label ASC",
                )
                .param("library_id", library_id.clone())
                .param("projection_version", projection_version),
            )
            .await
            .context("failed to query Neo4j projection nodes")?;
        let mut nodes = Vec::new();
        while let Some(row) = node_stream.next().await.context("failed to read Neo4j node row")? {
            let node_id: String = row.get("node_id").context("missing node_id")?;
            let metadata_json: String =
                row.get("metadata_json").unwrap_or_else(|_| "{}".to_string());
            nodes.push(GraphProjectionNodeWrite {
                node_id: Uuid::parse_str(&node_id).context("invalid projected node id")?,
                canonical_key: row.get("canonical_key").context("missing canonical_key")?,
                label: row.get("label").context("missing label")?,
                node_type: row.get("node_type").context("missing node_type")?,
                support_count: row
                    .get::<i64>("support_count")
                    .map(|value| i32::try_from(value).unwrap_or(i32::MAX))
                    .unwrap_or_default(),
                summary: row
                    .get::<String>("summary")
                    .ok()
                    .and_then(|value| if value.trim().is_empty() { None } else { Some(value) }),
                aliases: row.get("aliases").unwrap_or_default(),
                metadata_json: serde_json::from_str(&metadata_json)
                    .unwrap_or_else(|_| serde_json::json!({})),
            });
        }

        let mut edge_stream = self
            .graph
            .execute(
                query(
                    "MATCH (source:RustRAGNode {
                        library_id: $library_id,
                        projection_version: $projection_version
                     })-[r:RUNTIME_RELATION {
                        library_id: $library_id,
                        projection_version: $projection_version
                     }]->(target:RustRAGNode {
                        library_id: $library_id,
                        projection_version: $projection_version
                     })
                     RETURN
                        r.edge_id as edge_id,
                        source.node_id as from_node_id,
                        target.node_id as to_node_id,
                        r.relation_type as relation_type,
                        r.canonical_key as canonical_key,
                        r.support_count as support_count,
                        r.summary as summary,
                        r.weight as weight,
                        coalesce(r.metadata_json, '{}') as metadata_json
                     ORDER BY r.relation_type ASC, r.canonical_key ASC",
                )
                .param("library_id", library_id)
                .param("projection_version", projection_version),
            )
            .await
            .context("failed to query Neo4j projection edges")?;
        let mut edges = Vec::new();
        while let Some(row) = edge_stream.next().await.context("failed to read Neo4j edge row")? {
            let edge_id: String = row.get("edge_id").context("missing edge_id")?;
            let from_node_id: String = row.get("from_node_id").context("missing from_node_id")?;
            let to_node_id: String = row.get("to_node_id").context("missing to_node_id")?;
            let metadata_json: String =
                row.get("metadata_json").unwrap_or_else(|_| "{}".to_string());
            edges.push(GraphProjectionEdgeWrite {
                edge_id: Uuid::parse_str(&edge_id).context("invalid projected edge id")?,
                from_node_id: Uuid::parse_str(&from_node_id)
                    .context("invalid projected edge source id")?,
                to_node_id: Uuid::parse_str(&to_node_id)
                    .context("invalid projected edge target id")?,
                relation_type: row.get("relation_type").context("missing relation_type")?,
                canonical_key: row.get("canonical_key").context("missing canonical_key")?,
                support_count: row
                    .get::<i64>("support_count")
                    .map(|value| i32::try_from(value).unwrap_or(i32::MAX))
                    .unwrap_or_default(),
                summary: row
                    .get::<String>("summary")
                    .ok()
                    .and_then(|value| if value.trim().is_empty() { None } else { Some(value) }),
                weight: row.get("weight").ok(),
                metadata_json: serde_json::from_str(&metadata_json)
                    .unwrap_or_else(|_| serde_json::json!({})),
            });
        }

        Ok(GraphProjectionData { nodes, edges })
    }
}
