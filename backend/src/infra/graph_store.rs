use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct GraphProjectionNodeWrite {
    pub node_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub aliases: Vec<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct GraphProjectionEdgeWrite {
    pub edge_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub support_count: i32,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct GraphProjectionData {
    pub nodes: Vec<GraphProjectionNodeWrite>,
    pub edges: Vec<GraphProjectionEdgeWrite>,
}

#[async_trait]
pub trait GraphStore: Send + Sync {
    fn backend_name(&self) -> &'static str;
    async fn ping(&self) -> anyhow::Result<()>;
    async fn replace_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
        nodes: &[GraphProjectionNodeWrite],
        edges: &[GraphProjectionEdgeWrite],
    ) -> anyhow::Result<()>;
    async fn load_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphProjectionData>;
}
