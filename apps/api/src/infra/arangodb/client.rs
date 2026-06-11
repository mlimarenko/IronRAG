#![allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::missing_errors_doc)]

use anyhow::{Context, anyhow};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, sleep};

use crate::app::config::Settings;
use uuid::Uuid;

use crate::infra::arangodb::collections::{
    chunk_vector_collection_for_dim, chunk_vector_collection_for_library,
    chunk_vector_index_for_dim, chunk_vector_index_for_library, entity_vector_collection_for_dim,
    entity_vector_index_for_dim,
};

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct ArangoIndexRow {
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(rename = "type")]
    index_type: String,
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    unique: bool,
    #[serde(default)]
    sparse: bool,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Clone)]
pub struct ArangoClient {
    http: Client,
    base_url: String,
    database: String,
    username: String,
    password: String,
}

impl ArangoClient {
    pub fn from_settings(settings: &Settings) -> anyhow::Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(anyhow!("arangodb_url must not be empty"));
        }
        if settings.arangodb_database.trim().is_empty() {
            return Err(anyhow!("arangodb_database must not be empty"));
        }
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(
                settings.arangodb_request_timeout_seconds.max(1),
            ))
            .build()
            .context("failed to build ArangoDB HTTP client")?;
        Ok(Self {
            http,
            base_url,
            database: settings.arangodb_database.clone(),
            username: settings.arangodb_username.clone(),
            password: settings.arangodb_password.clone(),
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn database(&self) -> &str {
        &self.database
    }

    #[must_use]
    pub fn database_api_url(&self, path: &str) -> String {
        format!("{}/_db/{}/{}", self.base_url, self.database, path.trim_start_matches('/'))
    }

    fn system_api_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        crate::observability::inject_trace_context(
            self.http
                .request(method, self.database_api_url(path))
                .basic_auth(&self.username, Some(&self.password)),
        )
    }

    fn system_request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        crate::observability::inject_trace_context(
            self.http
                .request(method, self.system_api_url(path))
                .basic_auth(&self.username, Some(&self.password)),
        )
    }

    pub async fn ensure_database(&self) -> anyhow::Result<()> {
        let databases = self
            .system_request(Method::GET, "_api/database/user")
            .send()
            .await
            .context("failed to list ArangoDB databases")?;
        if !databases.status().is_success() {
            return Err(anyhow!(
                "failed to list ArangoDB databases: status {}",
                databases.status()
            ));
        }
        let payload = databases
            .json::<serde_json::Value>()
            .await
            .context("failed to decode ArangoDB databases response")?;
        let Some(names) = payload.get("result").and_then(serde_json::Value::as_array) else {
            return Err(anyhow!("ArangoDB databases response did not include `result` array"));
        };
        if names.iter().any(|name| name.as_str() == Some(self.database.as_str())) {
            return Ok(());
        }

        let body = serde_json::json!({
            "name": self.database,
        });
        let response =
            self.system_request(Method::POST, "_api/database").json(&body).send().await?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            return Ok(());
        }
        Err(anyhow!(
            "failed to ensure ArangoDB database {}: status {}",
            self.database,
            response.status()
        ))
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        let response = self.request(Method::GET, "_api/version").send().await?;
        if !response.status().is_success() {
            return Err(anyhow!("ArangoDB ping failed with status {}", response.status()));
        }
        Ok(())
    }

    pub async fn collection_exists(&self, name: &str) -> anyhow::Result<bool> {
        let response = self
            .request(Method::GET, &format!("_api/collection/{name}"))
            .send()
            .await
            .with_context(|| format!("failed to read collection metadata for {name}"))?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to read collection metadata for {name}: status {}",
                response.status()
            ));
        }
        Ok(true)
    }

    pub async fn view_exists(&self, name: &str) -> anyhow::Result<bool> {
        let response = self
            .request(Method::GET, &format!("_api/view/{name}"))
            .send()
            .await
            .with_context(|| format!("failed to read view metadata for {name}"))?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to read view metadata for {name}: status {}",
                response.status()
            ));
        }
        Ok(true)
    }

    pub async fn graph_exists(&self, name: &str) -> anyhow::Result<bool> {
        let response = self
            .request(Method::GET, &format!("_api/gharial/{name}"))
            .send()
            .await
            .with_context(|| format!("failed to read named graph metadata for {name}"))?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to read named graph metadata for {name}: status {}",
                response.status()
            ));
        }
        Ok(true)
    }

    pub async fn vector_index_exists(
        &self,
        collection: &str,
        index_name: &str,
    ) -> anyhow::Result<bool> {
        Ok(self
            .find_index_by_name(collection, index_name)
            .await?
            .is_some_and(|index| index.index_type == "vector"))
    }

    pub async fn vector_index_dimensions(
        &self,
        collection: &str,
        index_name: &str,
        field: &str,
    ) -> anyhow::Result<Option<u64>> {
        let Some(index) = self.find_index_by_name(collection, index_name).await? else {
            return Ok(None);
        };
        anyhow::ensure!(
            index.index_type == "vector",
            "index {index_name} on {collection} exists but has type {} instead of vector",
            index.index_type
        );
        anyhow::ensure!(
            index.fields == [field],
            "vector index {index_name} on {collection} has fields {:?}, expected [{field}]",
            index.fields
        );
        vector_index_dimension(&index)
            .map(Some)
            .ok_or_else(|| anyhow!("vector index {index_name} on {collection} has no dimension"))
    }

    pub async fn persistent_index_matches(
        &self,
        collection: &str,
        index_name: &str,
        fields: &[&str],
        unique: bool,
        sparse: bool,
    ) -> anyhow::Result<bool> {
        Ok(self.find_index_by_name(collection, index_name).await?.is_some_and(|index| {
            persistent_index_definition_matches(&index, fields, unique, sparse)
        }))
    }

    pub async fn ensure_document_collection(&self, name: &str) -> anyhow::Result<()> {
        self.ensure_collection(name, false).await
    }

    pub async fn ensure_edge_collection(&self, name: &str) -> anyhow::Result<()> {
        self.ensure_collection(name, true).await
    }

    async fn ensure_collection(&self, name: &str, edge: bool) -> anyhow::Result<()> {
        #[derive(Serialize)]
        struct CreateCollectionBody<'a> {
            name: &'a str,
            #[serde(rename = "type")]
            collection_type: i32,
        }

        let response = self
            .request(Method::POST, "_api/collection")
            .json(&CreateCollectionBody { name, collection_type: if edge { 3 } else { 2 } })
            .send()
            .await?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            return Ok(());
        }
        Err(anyhow!("failed to ensure collection {name}: status {}", response.status()))
    }

    /// Creates (or leaves in place) a custom ArangoSearch analyzer with the
    /// given type + properties + features. Idempotent: if an analyzer with
    /// `name` already exists the call is a no-op.
    ///
    /// Used for analyzers that are not part of the Arango default set —
    /// e.g. an application-level trigram analyzer that makes a title
    /// subquery tolerant to small spelling variants and single-character
    /// typos that the default stemming analyzers collapse into different
    /// stems.
    pub async fn ensure_analyzer(
        &self,
        name: &str,
        analyzer_type: &str,
        properties: serde_json::Value,
        features: &[&str],
    ) -> anyhow::Result<()> {
        let existing = self
            .request(Method::GET, &format!("_api/analyzer/{name}"))
            .send()
            .await
            .with_context(|| format!("failed to query analyzer {name}"))?;
        if existing.status().is_success() {
            return Ok(());
        }
        if existing.status().as_u16() != 404 {
            return Err(anyhow!(
                "unexpected status probing analyzer {name}: {}",
                existing.status()
            ));
        }
        let body = serde_json::json!({
            "name": name,
            "type": analyzer_type,
            "properties": properties,
            "features": features,
        });
        let response = self
            .request(Method::POST, "_api/analyzer")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("failed to create analyzer {name}"))?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            return Ok(());
        }
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(anyhow!("failed to create analyzer {name}: status {status}, body {text}"))
    }

    pub async fn ensure_view(&self, name: &str, links: serde_json::Value) -> anyhow::Result<()> {
        self.ensure_view_exists(name).await?;

        for attempt in 0..=3 {
            if self.view_links_match(name, &links).await? {
                return Ok(());
            }

            let properties = serde_json::json!({
                "links": links,
            });
            let update = self
                .request(Method::PATCH, &format!("_api/view/{name}/properties"))
                .json(&properties)
                .send()
                .await
                .with_context(|| format!("failed to update view properties for {name}"))?;
            if update.status().is_success() {
                continue;
            }

            let status = update.status();
            let response_body = update
                .text()
                .await
                .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
            if attempt < 3 && (status.is_server_error() || status.as_u16() == 404) {
                sleep(Duration::from_millis(150 * (attempt + 1) as u64)).await;
                continue;
            }
            return Err(anyhow!(
                "failed to update view properties for {name}: status {status}, body {response_body}",
            ));
        }

        if self.view_links_match(name, &links).await? {
            return Ok(());
        }
        Err(anyhow!("failed to reconcile view properties for {name} after retries"))
    }

    pub async fn ensure_named_graph(
        &self,
        name: &str,
        edge_definitions: serde_json::Value,
    ) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "name": name,
            "edgeDefinitions": edge_definitions,
        });
        let response = self.request(Method::POST, "_api/gharial").json(&body).send().await?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            return Ok(());
        }
        Err(anyhow!("failed to ensure named graph {name}: status {}", response.status()))
    }

    pub async fn ensure_vector_index(
        &self,
        collection: &str,
        index_name: &str,
        field: &str,
        dimension: u64,
        n_lists: u64,
        default_n_probe: u64,
        training_iterations: u64,
    ) -> anyhow::Result<()> {
        if let Some(existing) = self.find_index_by_name(collection, index_name).await? {
            anyhow::ensure!(
                vector_index_definition_matches(&existing, field, dimension),
                "vector index {index_name} on {collection} exists with a different definition"
            );
            let source_rows = self.count_vector_index_source_rows(collection).await?;
            let effective_n_lists = effective_vector_index_n_lists_memory_safe(
                n_lists,
                source_rows,
                dimension,
                vector_index_training_budget_bytes(),
            );
            let existing_n_lists =
                existing.params.get("nLists").and_then(serde_json::Value::as_u64).unwrap_or(1);
            if existing_n_lists >= effective_n_lists {
                return Ok(());
            }
            tracing::info!(
                collection,
                index_name,
                existing_n_lists,
                effective_n_lists,
                source_rows,
                "vector index nLists is stale; dropping and recreating"
            );
            let index_id = existing.id.as_deref().ok_or_else(|| {
                anyhow!("ArangoDB index {index_name} on {collection} did not include an id")
            })?;
            self.delete_index(index_id).await.with_context(|| {
                format!("failed to drop stale vector index {index_name} on {collection}")
            })?;
        }

        self.delete_vector_training_rows(collection).await?;
        let source_rows = self.count_vector_index_source_rows(collection).await?;
        let effective_n_lists = effective_vector_index_n_lists_memory_safe(
            n_lists,
            source_rows,
            dimension,
            vector_index_training_budget_bytes(),
        );
        if effective_n_lists != n_lists {
            tracing::warn!(
                collection,
                index_name,
                configured_n_lists = n_lists,
                effective_n_lists,
                source_rows,
                "clamped Arango vector index nLists to available vector rows"
            );
        }
        if source_rows == 0 {
            self.seed_vector_training_rows(collection, field, dimension, effective_n_lists).await?;
        }
        let body = serde_json::json!({
            "name": index_name,
            "type": "vector",
            "fields": [field],
            "params": {
                "metric": "cosine",
                "dimension": dimension,
                "nLists": effective_n_lists,
                "defaultNProbe": default_n_probe,
                "trainingIterations": training_iterations
            }
        });
        let response = self
            .request(Method::POST, &format!("_api/index?collection={collection}"))
            .json(&body)
            .send()
            .await?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            self.ensure_vector_index_definition(collection, index_name, field, dimension).await?;
            return Ok(());
        }
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        if status.as_u16() == 400
            && (response_body.contains("Number of training points")
                || response_body.contains("nx >= k"))
        {
            self.seed_vector_training_rows(collection, field, dimension, effective_n_lists).await?;
            let retry = self
                .request(Method::POST, &format!("_api/index?collection={collection}"))
                .json(&body)
                .send()
                .await?;
            if retry.status().is_success() || retry.status().as_u16() == 409 {
                self.ensure_vector_index_definition(collection, index_name, field, dimension)
                    .await?;
                return Ok(());
            }
            let retry_status = retry.status();
            let retry_body = retry.text().await.unwrap_or_default();
            return Err(anyhow!(
                "failed to ensure vector index {index_name} on {collection} after seeding: status {retry_status}, body {retry_body}",
            ));
        }
        Err(anyhow!(
            "failed to ensure vector index {index_name} on {collection}: status {status}, body {response_body}",
        ))
    }

    /// Idempotently create the per-dim chunk-vector collection for
    /// `dim` and attach the three persistent indexes its writers and
    /// readers require, then create the ANN index. Lets callers add
    /// a new vector dimension to the deployment without touching
    /// bootstrap or restarting the stack.
    ///
    /// Persistent index fields mirror the legacy single-dim collection
    /// in [`collections::KNOWLEDGE_PERSISTENT_INDEXES`] so existing
    /// queries (revision-generation lookups, library scans,
    /// chunk-model joins) work unchanged on each per-dim shard.
    pub async fn ensure_chunk_vector_collection_for_dim(
        &self,
        dim: u64,
        n_lists: u64,
        default_n_probe: u64,
        training_iterations: u64,
    ) -> anyhow::Result<()> {
        let collection = chunk_vector_collection_for_dim(dim);
        self.ensure_document_collection(&collection).await.with_context(|| {
            format!("failed to ensure per-dim chunk vector collection {collection}")
        })?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_revision_generation_index"),
            &["revision_id", "embedding_model_key", "vector_kind", "freshness_generation"],
            false,
            false,
        )
        .await?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_chunk_model_index"),
            &[
                "chunk_id",
                "embedding_model_key",
                "vector_kind",
                "freshness_generation",
                "created_at",
            ],
            false,
            false,
        )
        .await?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_library_index"),
            &["library_id", "vector_kind", "freshness_generation"],
            false,
            false,
        )
        .await?;
        let index_name = chunk_vector_index_for_dim(dim);
        self.ensure_vector_index(
            &collection,
            &index_name,
            "vector",
            dim,
            n_lists,
            default_n_probe,
            training_iterations,
        )
        .await
    }

    /// Idempotently create the per-(library, dim) chunk-vector shard for
    /// `(dim, library_id)` and attach the same three persistent indexes
    /// plus ANN index that [`ensure_chunk_vector_collection_for_dim`]
    /// installs on the shared per-dim shard. Each library's chunk vectors
    /// live in their own shard so `APPROX_NEAR_COSINE` scans one library's
    /// (small) vector set instead of every library's vectors at once.
    ///
    /// `n_lists` MUST already be sized for this (typically tiny) shard's row
    /// count — IVF training needs at least as many sample points as lists.
    /// The callers in `search_store` compute it from the live row count; the
    /// shared `ensure_vector_index` additionally clamps to available rows and
    /// seeds synthetic training rows when the shard is empty, so a first
    /// write or an under-populated shard never hard-fails ingest.
    pub async fn ensure_chunk_vector_collection_for_library(
        &self,
        dim: u64,
        library_id: Uuid,
        n_lists: u64,
        default_n_probe: u64,
        training_iterations: u64,
    ) -> anyhow::Result<()> {
        let collection = chunk_vector_collection_for_library(dim, library_id);
        self.ensure_document_collection(&collection).await.with_context(|| {
            format!("failed to ensure per-library chunk vector collection {collection}")
        })?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_revision_generation_index"),
            &["revision_id", "embedding_model_key", "vector_kind", "freshness_generation"],
            false,
            false,
        )
        .await?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_chunk_model_index"),
            &[
                "chunk_id",
                "embedding_model_key",
                "vector_kind",
                "freshness_generation",
                "created_at",
            ],
            false,
            false,
        )
        .await?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_library_index"),
            &["library_id", "vector_kind", "freshness_generation"],
            false,
            false,
        )
        .await?;
        let index_name = chunk_vector_index_for_library(dim, library_id);
        self.ensure_vector_index(
            &collection,
            &index_name,
            "vector",
            dim,
            n_lists,
            default_n_probe,
            training_iterations,
        )
        .await
    }

    /// Count the live (non-seed) rows in `collection`, returning 0 when the
    /// collection does not exist yet. Used by the per-library shard ensure to
    /// size IVF `nLists` from the actual row count before training.
    pub async fn count_chunk_vector_rows(&self, collection: &str) -> anyhow::Result<u64> {
        if !self.collection_exists(collection).await? {
            return Ok(0);
        }
        self.count_vector_index_source_rows(collection).await
    }

    /// Whether `collection` holds at least one row for `library_id`. Returns
    /// false when the collection does not exist. A `LIMIT 1` existence probe
    /// over the `library_id` persistent index — cheap enough to gate a
    /// chunk-vector write that has not yet been routed to a per-library shard.
    pub async fn chunk_vector_collection_has_library_rows(
        &self,
        collection: &str,
        library_id: Uuid,
    ) -> anyhow::Result<bool> {
        if !self.collection_exists(collection).await? {
            return Ok(false);
        }
        let cursor = self
            .query_json(
                "FOR row IN @@collection FILTER row.library_id == @library_id LIMIT 1 RETURN 1",
                serde_json::json!({ "@collection": collection, "library_id": library_id }),
            )
            .await
            .with_context(|| {
                format!("failed to probe library rows in chunk vector collection {collection}")
            })?;
        let has_rows = cursor
            .get("result")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| !rows.is_empty());
        Ok(has_rows)
    }

    /// Idempotently create the per-dim entity-vector collection +
    /// persistent index + ANN index for `dim`. Mirrors
    /// [`ensure_chunk_vector_collection_for_dim`] on the graph-node
    /// vector side.
    pub async fn ensure_entity_vector_collection_for_dim(
        &self,
        dim: u64,
        n_lists: u64,
        default_n_probe: u64,
        training_iterations: u64,
    ) -> anyhow::Result<()> {
        let collection = entity_vector_collection_for_dim(dim);
        self.ensure_document_collection(&collection).await.with_context(|| {
            format!("failed to ensure per-dim entity vector collection {collection}")
        })?;
        self.ensure_persistent_index(
            &collection,
            &format!("{collection}_library_index"),
            &["library_id", "embedding_model_key"],
            false,
            false,
        )
        .await?;
        let index_name = entity_vector_index_for_dim(dim);
        self.ensure_vector_index(
            &collection,
            &index_name,
            "vector",
            dim,
            n_lists,
            default_n_probe,
            training_iterations,
        )
        .await
    }

    /// Enumerate every `knowledge_chunk_vector_d<dim>` collection
    /// currently present in the database. Used by the
    /// per-library rebuild path to drop a library's rows from any
    /// previous-dim shard it might still have material in, and by
    /// the snapshot exporter to discover all live per-dim shards
    /// at runtime instead of from a static list.
    pub async fn list_per_dim_chunk_vector_collections(&self) -> anyhow::Result<Vec<String>> {
        self.list_collections_matching("knowledge_chunk_vector_d").await
    }

    /// Same as [`list_per_dim_chunk_vector_collections`] but for the
    /// graph-node vector shards.
    pub async fn list_per_dim_entity_vector_collections(&self) -> anyhow::Result<Vec<String>> {
        self.list_collections_matching("knowledge_entity_vector_d").await
    }

    /// Enumerate every per-(library, dim) chunk-vector shard
    /// (`knowledge_chunk_vector_d{dim}_l{library_hex}`) currently present.
    /// Used by bootstrap to (re)build each shard's IVF index on restart and
    /// by the per-library migration to discover already-sharded libraries.
    /// The shared per-dim shards (no `_l` suffix) are intentionally excluded:
    /// [`parse_library_vector_shard`] returns `None` for them.
    pub async fn list_per_library_chunk_vector_collections(&self) -> anyhow::Result<Vec<String>> {
        let candidates = self.list_collections_matching("knowledge_chunk_vector_d").await?;
        Ok(candidates
            .into_iter()
            .filter(|name| {
                crate::infra::arangodb::collections::parse_library_vector_shard(name).is_some_and(
                    |shard| {
                        shard.kind == crate::infra::arangodb::collections::VectorShardKind::Chunk
                    },
                )
            })
            .collect())
    }

    async fn list_collections_matching(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        #[derive(Deserialize)]
        struct CollectionEntry {
            name: String,
        }
        #[derive(Deserialize)]
        struct CollectionsResponse {
            result: Vec<CollectionEntry>,
        }
        let response = self
            .request(Method::GET, "_api/collection?excludeSystem=true")
            .send()
            .await
            .context("failed to list Arango collections")?;
        if !response.status().is_success() {
            return Err(anyhow!("failed to list Arango collections: status {}", response.status()));
        }
        let body: CollectionsResponse =
            response.json().await.context("failed to decode Arango collection list response")?;
        Ok(body
            .result
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name.starts_with(prefix))
            .collect())
    }

    pub async fn delete_index_by_name(
        &self,
        collection: &str,
        index_name: &str,
    ) -> anyhow::Result<()> {
        let Some(existing) = self.find_index_by_name(collection, index_name).await? else {
            return Ok(());
        };
        let index_id = existing.id.as_deref().ok_or_else(|| {
            anyhow!("ArangoDB index {index_name} on {collection} did not include an id")
        })?;
        self.delete_index(index_id)
            .await
            .with_context(|| format!("failed to delete vector index {index_name}"))
    }

    pub async fn recreate_vector_index(
        &self,
        collection: &str,
        index_name: &str,
        field: &str,
        dimension: u64,
        n_lists: u64,
        default_n_probe: u64,
        training_iterations: u64,
    ) -> anyhow::Result<()> {
        self.delete_index_by_name(collection, index_name).await?;
        self.ensure_vector_index(
            collection,
            index_name,
            field,
            dimension,
            n_lists,
            default_n_probe,
            training_iterations,
        )
        .await
    }

    async fn ensure_vector_index_definition(
        &self,
        collection: &str,
        index_name: &str,
        field: &str,
        dimension: u64,
    ) -> anyhow::Result<()> {
        let Some(index) = self.find_index_by_name(collection, index_name).await? else {
            return Err(anyhow!("vector index {index_name} on {collection} was not created"));
        };
        anyhow::ensure!(
            vector_index_definition_matches(&index, field, dimension),
            "vector index {index_name} on {collection} conflicts with the canonical definition"
        );
        Ok(())
    }

    async fn delete_index(&self, index_id: &str) -> anyhow::Result<()> {
        let response =
            self.request(Method::DELETE, &format!("_api/index/{index_id}")).send().await?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            return Ok(());
        }
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        Err(anyhow!(
            "failed to delete ArangoDB index {index_id}: status {status}, body {response_body}"
        ))
    }

    pub async fn ensure_persistent_index(
        &self,
        collection: &str,
        index_name: &str,
        fields: &[&str],
        unique: bool,
        sparse: bool,
    ) -> anyhow::Result<()> {
        if let Some(existing) = self.find_index_by_name(collection, index_name).await? {
            anyhow::ensure!(
                persistent_index_definition_matches(&existing, fields, unique, sparse),
                "persistent index {index_name} on {collection} exists with a different definition",
            );
            return Ok(());
        }

        let body = serde_json::json!({
            "name": index_name,
            "type": "persistent",
            "fields": fields,
            "unique": unique,
            "sparse": sparse,
        });
        let response = self
            .request(Method::POST, &format!("_api/index?collection={collection}"))
            .json(&body)
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        if response.status().as_u16() == 409 {
            anyhow::ensure!(
                self.persistent_index_matches(collection, index_name, fields, unique, sparse)
                    .await?,
                "persistent index {index_name} on {collection} conflicts with the canonical definition",
            );
            return Ok(());
        }

        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        Err(anyhow!(
            "failed to ensure persistent index {index_name} on {collection}: status {status}, body {response_body}",
        ))
    }

    async fn seed_vector_training_rows(
        &self,
        collection: &str,
        field: &str,
        dimension: u64,
        n_lists: u64,
    ) -> anyhow::Result<()> {
        let sample_count = n_lists.max(1);
        let dimensions = usize::try_from(dimension).context("vector dimension is too large")?;
        let mut rows = Vec::with_capacity(usize::try_from(sample_count).unwrap_or(0));
        for i in 0..sample_count {
            let value = (i + 1) as f64 / (sample_count as f64 + 1.0);
            let vector = vec![value; dimensions];
            let mut row = serde_json::Map::new();
            row.insert(
                "_key".to_string(),
                serde_json::Value::String(format!("__bootstrap_vector_seed__{i}")),
            );
            row.insert("__bootstrap_vector_seed__".to_string(), serde_json::Value::Bool(true));
            row.insert(
                field.to_string(),
                serde_json::to_value(vector).context("failed to encode seed vector")?,
            );
            rows.push(serde_json::Value::Object(row));
        }

        let _ = self
            .query_json(
                "FOR row IN @rows
                 INSERT row INTO @@collection
                 OPTIONS { overwriteMode: \"ignore\" }",
                serde_json::json!({
                    "@collection": collection,
                    "rows": rows,
                }),
            )
            .await
            .with_context(|| format!("failed to seed vector training rows for {collection}"))?;

        Ok(())
    }

    async fn delete_vector_training_rows(&self, collection: &str) -> anyhow::Result<()> {
        let _ = self
            .query_json_with_options(
                "FOR row IN @@collection
                 FILTER row.__bootstrap_vector_seed__ == true
                 REMOVE row IN @@collection",
                serde_json::json!({
                    "@collection": collection,
                }),
                serde_json::json!({ "maxRuntime": 600 }),
            )
            .await
            .with_context(|| format!("failed to delete vector training rows for {collection}"))?;
        Ok(())
    }

    async fn count_vector_index_source_rows(&self, collection: &str) -> anyhow::Result<u64> {
        let cursor = self
            .query_json_with_options(
                "FOR row IN @@collection
                 FILTER row.__bootstrap_vector_seed__ != true
                 COLLECT WITH COUNT INTO length
                 RETURN length",
                serde_json::json!({
                    "@collection": collection,
                }),
                serde_json::json!({ "maxRuntime": 600 }),
            )
            .await
            .with_context(|| {
                format!("failed to count vector index source rows for {collection}")
            })?;
        cursor
            .get("result")
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow!("ArangoDB count query returned no row for {collection}"))
    }

    pub async fn query_json(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::json!({
            "query": query,
            "bindVars": bind_vars,
        });
        self.run_cursor_query(query, &body, None).await
    }

    /// Run an AQL cursor query and merge query-level `options` (such as
    /// `maxRuntime`) into the POST body, with an extended HTTP timeout so
    /// the long-running cursor on the Arango side actually gets a chance to
    /// finish. Use this when the default cursor budget is not enough — for
    /// example a one-shot maintenance scan over a large legacy collection
    /// whose initial `DISTINCT` pass cannot finish inside the canonical
    /// 15-second HTTP timeout.
    pub async fn query_json_with_options(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        options: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        const EXTENDED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
        let body = serde_json::json!({
            "query": query,
            "bindVars": bind_vars,
            "options": options,
        });
        self.run_cursor_query(query, &body, Some(EXTENDED_TIMEOUT)).await
    }

    async fn run_cursor_query(
        &self,
        query: &str,
        body: &serde_json::Value,
        timeout_override: Option<std::time::Duration>,
    ) -> anyhow::Result<serde_json::Value> {
        let query_span_started = std::time::Instant::now();
        let mut cursor = self
            .send_cursor_request_with_timeout(
                Method::POST,
                "_api/cursor",
                Some(body),
                "AQL query",
                timeout_override,
            )
            .await?;
        let mut merged_rows = take_cursor_result_rows(&mut cursor)?;
        if cursor.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false)
            || merged_rows.len() >= 1000
        {
            tracing::info!(
                query_prefix = %query.chars().take(96).collect::<String>(),
                initial_rows = merged_rows.len(),
                has_more = cursor.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false),
                cursor_id = cursor.get("id").and_then(serde_json::Value::as_str).unwrap_or("-"),
                "arangodb cursor received initial batch"
            );
        }

        while cursor.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            let cursor_id = cursor
                .get("id")
                .and_then(serde_json::Value::as_str)
                .context("ArangoDB cursor reported hasMore=true without an id")?
                .to_string();
            let mut next_cursor = self
                .send_cursor_request(
                    Method::PUT,
                    &format!("_api/cursor/{cursor_id}"),
                    None,
                    "ArangoDB cursor continuation",
                )
                .await?;
            let next_rows = take_cursor_result_rows(&mut next_cursor)?;
            tracing::info!(
                query_prefix = %query.chars().take(96).collect::<String>(),
                cursor_id = %cursor_id,
                batch_rows = next_rows.len(),
                has_more = next_cursor.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false),
                "arangodb cursor fetched continuation batch"
            );
            merged_rows.extend(next_rows);
            if let Some(extra) = next_cursor.get("extra").cloned() {
                cursor["extra"] = extra;
            }
            if let Some(count) = next_cursor.get("count").cloned() {
                cursor["count"] = count;
            }
            cursor["hasMore"] =
                next_cursor.get("hasMore").cloned().unwrap_or(serde_json::Value::Bool(false));
            if let Some(id) = next_cursor.get("id").cloned() {
                cursor["id"] = id;
            } else if let Some(object) = cursor.as_object_mut() {
                object.remove("id");
            }
        }

        cursor["result"] = serde_json::Value::Array(merged_rows);
        cursor["hasMore"] = serde_json::Value::Bool(false);
        if let Some(object) = cursor.as_object_mut() {
            object.remove("id");
        }
        if cursor
            .get("result")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| rows.len() >= 1000)
        {
            tracing::info!(
                query_prefix = %query.chars().take(96).collect::<String>(),
                merged_rows = cursor
                    .get("result")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, std::vec::Vec::len),
                "arangodb cursor merged final result"
            );
        }
        let row_count =
            cursor.get("result").and_then(serde_json::Value::as_array).map_or(0, Vec::len);
        crate::services::query::turn_spans::record_span(
            format!(
                "arango.{}",
                aql_primary_collection(
                    query,
                    body.get("bindVars").unwrap_or(&serde_json::Value::Null)
                )
            ),
            "db",
            query_span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(row_count as u64),
        );
        Ok(cursor)
    }

    /// Streams query results batch-by-batch instead of buffering the
    /// whole cursor in memory. The caller receives each batch via
    /// `handle_batch`; rows are dropped between batches, so memory use
    /// scales with batch size, not with total row count. Use this for
    /// bulk exports where the result set can be hundreds of thousands
    /// of rows. Each HTTP call carries a 10-minute timeout —
    /// snapshot-style edge scans with DOCUMENT() lookups can
    /// legitimately take tens of seconds, well beyond the 15-second
    /// canonical query default.
    pub async fn query_json_batches<F, Fut>(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        handle_batch: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(Vec<serde_json::Value>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        self.query_json_batches_with_batch_size(query, bind_vars, None, handle_batch).await
    }

    /// Same as `query_json_batches` but lets the caller request a
    /// specific AQL cursor `batchSize`. Useful for collections whose
    /// rows are large (high-dim vector shards): the Arango default of
    /// 1000 rows can exceed cursor / network memory on `~3072 floats`
    /// rows, where 64 rows already yields ~1.5 MiB of JSON. Lowering
    /// the batch size keeps both Arango and the in-process buffer
    /// bounded.
    pub async fn query_json_batches_with_batch_size<F, Fut>(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        batch_size: Option<u32>,
        handle_batch: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(Vec<serde_json::Value>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        self.query_json_batches_with_options(query, bind_vars, batch_size, false, handle_batch)
            .await
    }

    /// Streaming variant: sets the cursor request's `stream: true` option,
    /// which makes Arango produce result rows lazily as the FILTER runs
    /// instead of materializing the full result set before the first
    /// batch. Required for large-collection exports where the materialized
    /// result would exceed `--query.memory-limit` (errorNum 32).
    pub async fn query_json_batches_streaming<F, Fut>(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        batch_size: Option<u32>,
        handle_batch: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(Vec<serde_json::Value>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        self.query_json_batches_with_options(query, bind_vars, batch_size, true, handle_batch).await
    }

    async fn query_json_batches_with_options<F, Fut>(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        batch_size: Option<u32>,
        stream: bool,
        mut handle_batch: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(Vec<serde_json::Value>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        const BULK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
        let span_started = std::time::Instant::now();
        let mut span_row_count: u64 = 0;
        let mut body = serde_json::json!({
            "query": query,
            "bindVars": bind_vars,
        });
        if let Some(object) = body.as_object_mut() {
            if let Some(size) = batch_size {
                object.insert("batchSize".to_string(), serde_json::Value::from(size));
            }
            if stream {
                // Streaming cursor: Arango does not materialize the full
                // result set before returning the first batch. Pays off
                // hugely on huge collections (e.g. vector exports) where
                // the result would otherwise blow past --query.memory-limit.
                object.insert("stream".to_string(), serde_json::Value::Bool(true));
            }
        }
        let mut cursor = self
            .send_cursor_request_with_timeout(
                Method::POST,
                "_api/cursor",
                Some(&body),
                "AQL query",
                Some(BULK_TIMEOUT),
            )
            .await?;
        let initial_rows = take_cursor_result_rows(&mut cursor)?;
        span_row_count += initial_rows.len() as u64;
        handle_batch(initial_rows).await?;
        while cursor.get("hasMore").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            let cursor_id = cursor
                .get("id")
                .and_then(serde_json::Value::as_str)
                .context("ArangoDB cursor reported hasMore=true without an id")?
                .to_string();
            let mut next_cursor = self
                .send_cursor_request_with_timeout(
                    Method::PUT,
                    &format!("_api/cursor/{cursor_id}"),
                    None,
                    "ArangoDB cursor continuation",
                    Some(BULK_TIMEOUT),
                )
                .await?;
            let next_rows = take_cursor_result_rows(&mut next_cursor)?;
            span_row_count += next_rows.len() as u64;
            handle_batch(next_rows).await?;
            cursor["hasMore"] =
                next_cursor.get("hasMore").cloned().unwrap_or(serde_json::Value::Bool(false));
            if let Some(id) = next_cursor.get("id").cloned() {
                cursor["id"] = id;
            } else if let Some(object) = cursor.as_object_mut() {
                object.remove("id");
            }
        }
        crate::services::query::turn_spans::record_span(
            format!(
                "arango.{}",
                aql_primary_collection(
                    query,
                    body.get("bindVars").unwrap_or(&serde_json::Value::Null)
                )
            ),
            "db",
            span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(span_row_count),
        );
        Ok(())
    }

    /// Runs an AQL query with an explicit long timeout for bulk writes
    /// (restore inserts, clear-library sweeps). Inherits the same
    /// cursor payload semantics as `query_json`, but bypasses the
    /// canonical 15-second timeout.
    pub async fn query_json_bulk(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        const BULK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
        let body = serde_json::json!({
            "query": query,
            "bindVars": bind_vars,
        });
        let span_started = std::time::Instant::now();
        let response = self
            .send_cursor_request_with_timeout(
                Method::POST,
                "_api/cursor",
                Some(&body),
                "AQL bulk query",
                Some(BULK_TIMEOUT),
            )
            .await?;
        let row_count =
            response.get("result").and_then(serde_json::Value::as_array).map_or(0, Vec::len);
        crate::services::query::turn_spans::record_span(
            format!(
                "arango.{}",
                aql_primary_collection(
                    query,
                    body.get("bindVars").unwrap_or(&serde_json::Value::Null)
                )
            ),
            "db",
            span_started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(row_count as u64),
        );
        Ok(response)
    }

    async fn send_cursor_request(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
        operation: &str,
    ) -> anyhow::Result<serde_json::Value> {
        self.send_cursor_request_with_timeout(method, path, body, operation, None).await
    }

    /// Inner cursor request helper that accepts an optional per-request
    /// timeout override. Used by bulk snapshot restore paths that need
    /// headroom beyond the canonical 15-second query timeout.
    async fn send_cursor_request_with_timeout(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
        operation: &str,
        timeout: Option<std::time::Duration>,
    ) -> anyhow::Result<serde_json::Value> {
        let request = self.request(method, path);
        let request = if let Some(payload) = body { request.json(payload) } else { request };
        let request = if let Some(override_timeout) = timeout {
            request.timeout(override_timeout)
        } else {
            request
        };
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let response_body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
            return Err(anyhow!("{operation} failed with status {status}, body {response_body}"));
        }
        response
            .json::<serde_json::Value>()
            .await
            .with_context(|| format!("failed to decode {operation} response"))
    }

    async fn find_index_by_name(
        &self,
        collection: &str,
        index_name: &str,
    ) -> anyhow::Result<Option<ArangoIndexRow>> {
        Ok(self.list_indexes(collection).await?.into_iter().find(|index| index.name == index_name))
    }

    async fn list_indexes(&self, collection: &str) -> anyhow::Result<Vec<ArangoIndexRow>> {
        let response = self
            .request(Method::GET, &format!("_api/index?collection={collection}"))
            .send()
            .await
            .with_context(|| format!("failed to list indexes for {collection}"))?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to list indexes for {collection}: status {}",
                response.status()
            ));
        }
        let payload = response
            .json::<serde_json::Value>()
            .await
            .with_context(|| format!("failed to decode index list for {collection}"))?;
        let Some(indexes) = payload.get("indexes").and_then(serde_json::Value::as_array) else {
            return Err(anyhow!("ArangoDB index listing for {collection} did not include indexes"));
        };
        indexes
            .iter()
            .cloned()
            .map(serde_json::from_value::<ArangoIndexRow>)
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to decode index metadata for {collection}"))
    }

    async fn ensure_view_exists(&self, name: &str) -> anyhow::Result<()> {
        if self.get_view_links(name).await?.is_some() {
            return Ok(());
        }

        let body = serde_json::json!({
            "name": name,
            "type": "arangosearch",
        });
        let response = self.request(Method::POST, "_api/view").json(&body).send().await?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            return Ok(());
        }
        let status = response.status();
        let response_body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
        Err(anyhow!("failed to ensure view {name}: status {status}, body {response_body}"))
    }

    async fn get_view_links(&self, name: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let response = self
            .request(Method::GET, &format!("_api/view/{name}/properties"))
            .send()
            .await
            .with_context(|| format!("failed to load view properties for {name}"))?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            let status = response.status();
            let response_body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
            return Err(anyhow!(
                "failed to load view properties for {name}: status {status}, body {response_body}",
            ));
        }
        let payload = response
            .json::<serde_json::Value>()
            .await
            .with_context(|| format!("failed to decode view properties for {name}"))?;
        Ok(payload.get("links").cloned())
    }

    async fn view_links_match(
        &self,
        name: &str,
        expected_links: &serde_json::Value,
    ) -> anyhow::Result<bool> {
        let Some(actual_links) = self.get_view_links(name).await? else {
            return Ok(false);
        };
        Ok(view_links_semantically_match(expected_links, &actual_links))
    }
}

fn vector_index_definition_matches(index: &ArangoIndexRow, field: &str, dimension: u64) -> bool {
    index.index_type == "vector"
        && index.fields == [field]
        && vector_index_dimension(index) == Some(dimension)
}

/// Best-effort label for an AQL query span: the first collection it iterates
/// (`FOR x IN <collection>`), so the inspector can show "which" query ran
/// without leaking the full statement.
///
/// Most runtime queries reference the collection through a bind parameter
/// (`FOR x IN @@collection`), so the bare token after `IN` is the bind name,
/// not the collection. `bind_vars` is the query's `bindVars` map; a collection
/// bind `@@name` resolves against its key `@name` to the real collection (e.g.
/// `knowledge_chunk_vector_d3072`). Falls back to the bind name if unresolved,
/// or `aql` for queries with no plain `IN` source (subquery / `DOCUMENT(...)`).
fn aql_primary_collection(query: &str, bind_vars: &serde_json::Value) -> String {
    let mut tokens = query.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token.eq_ignore_ascii_case("in") {
            if let Some(next) = tokens.peek() {
                let raw = next.trim_start_matches(['(', '[']);
                // Collection bind parameter `@@name` → bindVars key `@name`.
                if let Some(rest) = raw.strip_prefix("@@") {
                    let bind_name =
                        rest.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                    if !bind_name.is_empty() {
                        if let Some(resolved) = bind_vars
                            .get(format!("@{bind_name}"))
                            .and_then(serde_json::Value::as_str)
                        {
                            return resolved.chars().take(64).collect();
                        }
                        return bind_name.chars().take(64).collect();
                    }
                }
                let collection = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
                if !collection.is_empty()
                    && collection.chars().next().is_some_and(|c| !c.is_ascii_digit())
                {
                    return collection.chars().take(64).collect();
                }
            }
        }
    }
    "aql".to_string()
}

fn vector_index_dimension(index: &ArangoIndexRow) -> Option<u64> {
    index.params.get("dimension").and_then(serde_json::Value::as_u64)
}

/// Empirical peak RSS cost per IVF centroid (nList) observed on ArangoDB 3.12.4
/// with large collections (650k × 3072-dim vectors, 2 GiB container).
///
/// The theoretical formula `256 × nLists × dim × 4` badly underestimates actual
/// memory: it predicts ~6 GB for nLists=317 but Arango crashed with nLists=30
/// (>2 GiB actual). Measured on a representative prod-scale stand (2 GiB cap):
///
/// | nLists | Peak RSS | Outcome |
/// |--------|----------|---------|
/// | 8      | 1.0 GiB  | OK      |
/// | 25     | 1.5 GiB  | OK      |
/// | 30     | >2 GiB   | CRASH   |
/// | 317    | >2 GiB   | CRASH   |
///
/// Observed overhead ≈ 1.5 GiB / 25 lists ≈ 60 MB/list. Using 50 MB/list as
/// the empirical constant keeps the formula slightly below the observed safe
/// boundary; the 1 GB budget then caps at 20 lists, giving a ~5-list buffer
/// below the last observed safe point (nLists=25).
///
/// HNSW is NOT available as a standalone index type in ArangoDB 3.12.4; it only
/// appears as a FAISS composite factory sub-component (e.g. "IVF_HNSW,PQ"),
/// so switching algorithm is not an option here.
const FAISS_EMPIRICAL_BYTES_PER_LIST: u64 = 50_000_000;

/// Default memory budget (bytes) for IVF k-means training inside the Arango
/// container, used when `IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES` is unset
/// (see [`vector_index_training_budget_bytes`]).
///
/// The canonical baseline `vector` tier in docker-compose.yml (anchor
/// `x-ironrag-resources-vector`) / Helm `values.yaml` is 5 GiB. ArangoDB idles
/// around 1.2-1.5 GiB on a populated stage, leaving ~3.5 GiB headroom on that
/// cap. Using 3 GB as the budget yields nLists ≈ 60 with the empirical
/// 50 MB/list overhead — comfortably below the headroom while giving a much
/// tighter IVF partitioning than the old 1 GB / 20 lists pair (which forced
/// 5+ s `APPROX_NEAR_COSINE` scans on 650k-row shards and produced
/// `retrieval.vector_failed` timeouts on the 30 s Arango HTTP client cap).
///
/// Per the canonical multi-dim vector-layer sizing policy, this budget is
/// sized for the largest per-dim shard the deployment expects to host. When a
/// heavier shard is rolled out on a larger host, the Arango container cap is
/// raised in lock-step (e.g. the `docker-compose.large.yml` overlay) and the
/// budget is bumped via the env override below — never recompile, never
/// silently exceed the container budget.
const VECTOR_INDEX_TRAINING_BUDGET_BYTES: u64 = 3_000_000_000;

/// Resolve the IVF k-means training memory budget (bytes).
///
/// Reads `IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES` so a larger-host overlay
/// (e.g. `docker-compose.large.yml`, which also raises the Arango container
/// cap) can widen the budget in lock-step without a recompile, satisfying the
/// canonical multi-dim vector-layer sizing policy. Falls back to
/// [`VECTOR_INDEX_TRAINING_BUDGET_BYTES`] when unset, empty, or unparseable.
fn vector_index_training_budget_bytes() -> u64 {
    parse_vector_index_training_budget(
        std::env::var("IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES").ok(),
    )
}

/// Pure parser for the IVF training budget env override.
///
/// Split out from [`vector_index_training_budget_bytes`] so it is unit-testable
/// without mutating process environment. A missing, blank, non-numeric, or
/// zero value falls back to the compiled-in default
/// [`VECTOR_INDEX_TRAINING_BUDGET_BYTES`]; zero is rejected because a zero
/// budget would clamp nLists to 1 and defeat IVF partitioning entirely.
fn parse_vector_index_training_budget(raw: Option<String>) -> u64 {
    raw.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(VECTOR_INDEX_TRAINING_BUDGET_BYTES)
}

fn effective_vector_index_n_lists(configured_n_lists: u64, source_rows: u64) -> u64 {
    configured_n_lists.max(1).min(source_rows.max(1))
}

/// Like [`effective_vector_index_n_lists`] but additionally enforces a memory
/// budget for IVF k-means training.
///
/// Uses the empirical per-list overhead constant [`FAISS_EMPIRICAL_BYTES_PER_LIST`]
/// (50 MB) rather than the theoretical `256 × dim × 4` formula. The theoretical
/// model severely underestimates ArangoDB 3.12.4's actual peak RSS because it
/// ignores Arango-side data structures, FAISS quantizer state, and OS page-cache
/// pressure. The empirical constant was derived from production measurements on a
/// 650k × 3072-dim shard under a 2 GiB container (see constant doc-comment).
///
/// Cap formula:
/// ```text
/// nLists_max = budget_bytes / FAISS_EMPIRICAL_BYTES_PER_LIST
/// ```
///
/// At the default 3 GB budget (matched to the canonical 5 GiB Arango
/// container in docker-compose.yml `x-ironrag-resources-vector` tier):
/// nLists_max = 60, yielding ~85k-130k vector comparisons per query at
/// nProbe=8 over a 650k-row shard (vs 5.2M for a full scan), well under the
/// 30 s `arangodb_request_timeout_seconds` cap.
fn effective_vector_index_n_lists_memory_safe(
    configured_n_lists: u64,
    source_rows: u64,
    _dimension: u64,
    budget_bytes: u64,
) -> u64 {
    let row_capped = effective_vector_index_n_lists(configured_n_lists, source_rows);
    let memory_cap =
        budget_bytes.checked_div(FAISS_EMPIRICAL_BYTES_PER_LIST).unwrap_or(u64::MAX).max(1);
    row_capped.min(memory_cap)
}

fn persistent_index_definition_matches(
    index: &ArangoIndexRow,
    fields: &[&str],
    unique: bool,
    sparse: bool,
) -> bool {
    index.index_type == "persistent"
        && index.fields.iter().map(String::as_str).eq(fields.iter().copied())
        && index.unique == unique
        && index.sparse == sparse
}

fn view_links_semantically_match(
    expected_links: &serde_json::Value,
    actual_links: &serde_json::Value,
) -> bool {
    let Some(expected_map) = expected_links.as_object() else {
        return expected_links == actual_links;
    };
    let Some(actual_map) = actual_links.as_object() else {
        return false;
    };

    expected_map.iter().all(|(collection_name, expected_config)| {
        let Some(actual_config) = actual_map.get(collection_name) else {
            return false;
        };
        collection_link_matches(expected_config, actual_config)
    })
}

fn collection_link_matches(
    expected_config: &serde_json::Value,
    actual_config: &serde_json::Value,
) -> bool {
    let Some(expected_object) = expected_config.as_object() else {
        return expected_config == actual_config;
    };
    let Some(actual_object) = actual_config.as_object() else {
        return false;
    };

    if expected_object
        .get("includeAllFields")
        .zip(actual_object.get("includeAllFields"))
        .is_some_and(|(expected, actual)| expected != actual)
    {
        return false;
    }

    if expected_object
        .get("analyzers")
        .zip(actual_object.get("analyzers"))
        .is_some_and(|(expected, actual)| expected != actual)
    {
        return false;
    }

    let expected_fields = expected_object.get("fields").and_then(serde_json::Value::as_object);
    let actual_fields = actual_object
        .get("fields")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let actual_collection_analyzers = actual_object.get("analyzers");

    expected_fields.is_none_or(|fields| {
        fields.iter().all(|(field_name, expected_field)| {
            let Some(actual_field) = actual_fields.get(field_name) else {
                return false;
            };
            field_link_matches(expected_field, actual_field, actual_collection_analyzers)
        })
    })
}

fn field_link_matches(
    expected_field: &serde_json::Value,
    actual_field: &serde_json::Value,
    actual_collection_analyzers: Option<&serde_json::Value>,
) -> bool {
    let Some(expected_object) = expected_field.as_object() else {
        return expected_field == actual_field;
    };
    let Some(actual_object) = actual_field.as_object() else {
        return false;
    };

    expected_object.iter().all(|(key, expected_value)| match key.as_str() {
        "analyzers" => actual_object
            .get("analyzers")
            .or(actual_collection_analyzers)
            .is_some_and(|actual_value| actual_value == expected_value),
        "fields" => {
            let expected_nested = expected_value.as_object();
            let actual_nested = actual_object
                .get("fields")
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            expected_nested.is_none_or(|fields| {
                fields.iter().all(|(nested_name, expected_nested_field)| {
                    let Some(actual_nested_field) = actual_nested.get(nested_name) else {
                        return false;
                    };
                    field_link_matches(
                        expected_nested_field,
                        actual_nested_field,
                        actual_object.get("analyzers").or(actual_collection_analyzers),
                    )
                })
            })
        }
        _ => actual_object.get(key).is_some_and(|actual_value| actual_value == expected_value),
    })
}

fn take_cursor_result_rows(
    cursor: &mut serde_json::Value,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let result =
        cursor.get_mut("result").context("ArangoDB cursor payload missing result field")?;
    let rows =
        result.as_array_mut().context("ArangoDB cursor payload result field is not an array")?;
    Ok(std::mem::take(rows))
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use anyhow::Context;
    use axum::{
        Json, Router,
        extract::{Path, State},
        routing::{post, put},
    };
    use reqwest::Client;
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::{
        ArangoClient, ArangoIndexRow, FAISS_EMPIRICAL_BYTES_PER_LIST,
        VECTOR_INDEX_TRAINING_BUDGET_BYTES, aql_primary_collection, effective_vector_index_n_lists,
        effective_vector_index_n_lists_memory_safe, parse_vector_index_training_budget,
        persistent_index_definition_matches, vector_index_definition_matches,
        view_links_semantically_match,
    };

    #[test]
    fn aql_primary_collection_resolves_collection_bind_to_real_name() {
        // The dominant runtime form: `FOR x IN @@collection` with the real
        // collection in bindVars under the single-`@` key.
        let bind_vars = json!({ "@collection": "knowledge_chunk_vector_d3072" });
        let label = aql_primary_collection(
            "FOR vector IN @@collection FILTER vector.library_id == @library LIMIT @k RETURN vector",
            &bind_vars,
        );
        assert_eq!(label, "knowledge_chunk_vector_d3072");
    }

    #[test]
    fn aql_primary_collection_reads_static_collection_token() {
        let label = aql_primary_collection(
            "FOR doc IN knowledge_chunk FILTER doc._key == @key RETURN doc",
            &json!({}),
        );
        assert_eq!(label, "knowledge_chunk");
    }

    #[test]
    fn aql_primary_collection_falls_back_to_bind_name_when_unresolved() {
        // Collection bind present in the query but missing from bindVars: keep
        // the bind name rather than collapsing every query to one generic token.
        let label = aql_primary_collection("FOR e IN @@edge_collection RETURN e", &json!({}));
        assert_eq!(label, "edge_collection");
    }

    #[test]
    fn aql_primary_collection_uses_first_collection_for_multi_collection_query() {
        let bind_vars = json!({ "@bundle_collection": "knowledge_context_bundle" });
        let label = aql_primary_collection(
            "FOR bundle IN @@bundle_collection FOR edge IN @@chunk_edge_collection RETURN bundle",
            &bind_vars,
        );
        assert_eq!(label, "knowledge_context_bundle");
    }

    #[test]
    fn aql_primary_collection_falls_back_to_aql_without_in_source() {
        let label = aql_primary_collection("RETURN DOCUMENT('knowledge_chunk/123')", &json!({}));
        assert_eq!(label, "aql");
    }

    async fn create_cursor(State(requests): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
        requests.fetch_add(1, Ordering::SeqCst);
        Json(json!({
            "result": (1..=1000).map(|value| json!({ "value": value })).collect::<Vec<_>>(),
            "hasMore": true,
            "id": "cursor-1",
            "extra": { "stats": { "writesExecuted": 0 } }
        }))
    }

    async fn continue_cursor(
        Path(cursor_id): Path<String>,
        State(requests): State<Arc<AtomicUsize>>,
    ) -> Json<serde_json::Value> {
        requests.fetch_add(1, Ordering::SeqCst);
        assert_eq!(cursor_id, "cursor-1");
        Json(json!({
            "result": [{ "value": 1001 }, { "value": 1002 }],
            "hasMore": false
        }))
    }

    async fn spawn_cursor_server() -> anyhow::Result<(SocketAddr, Arc<AtomicUsize>)> {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/_db/testdb/_api/cursor", post(create_cursor))
            .route("/_db/testdb/_api/cursor/{cursor_id}", put(continue_cursor))
            .with_state(Arc::clone(&requests));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok((address, requests))
    }

    #[tokio::test]
    async fn query_json_merges_all_cursor_batches() -> anyhow::Result<()> {
        let (address, requests) = spawn_cursor_server().await?;
        let client = ArangoClient {
            http: Client::builder().build()?,
            base_url: format!("http://{address}"),
            database: "testdb".to_string(),
            username: "user".to_string(),
            password: "password".to_string(),
        };

        let payload = client.query_json("FOR doc IN docs RETURN doc", json!({})).await?;
        let rows = payload
            .get("result")
            .and_then(serde_json::Value::as_array)
            .context("result array missing from merged cursor payload")?;

        assert_eq!(rows.len(), 1002);
        assert_eq!(
            rows.first().and_then(|row| row.get("value")).and_then(serde_json::Value::as_i64),
            Some(1),
        );
        assert_eq!(
            rows.last().and_then(|row| row.get("value")).and_then(serde_json::Value::as_i64),
            Some(1002),
        );
        assert_eq!(payload.get("hasMore").and_then(serde_json::Value::as_bool), Some(false));
        assert_eq!(
            payload
                .get("extra")
                .and_then(|extra| extra.get("stats"))
                .and_then(|stats| stats.get("writesExecuted"))
                .and_then(serde_json::Value::as_i64),
            Some(0),
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[test]
    fn persistent_index_definition_requires_exact_match() {
        let index = ArangoIndexRow {
            id: Some("knowledge_chunk/123".to_string()),
            name: "knowledge_document_library_updated_index".to_string(),
            index_type: "persistent".to_string(),
            fields: vec![
                "library_id".to_string(),
                "workspace_id".to_string(),
                "updated_at".to_string(),
                "document_id".to_string(),
            ],
            unique: false,
            sparse: false,
            params: serde_json::Value::Null,
        };

        assert!(persistent_index_definition_matches(
            &index,
            &["library_id", "workspace_id", "updated_at", "document_id"],
            false,
            false,
        ));
        assert!(!persistent_index_definition_matches(
            &index,
            &["library_id", "updated_at", "document_id"],
            false,
            false,
        ));
    }

    #[test]
    fn vector_index_definition_requires_dimension_and_field_match() {
        let index = ArangoIndexRow {
            id: Some("knowledge_chunk_vector/123".to_string()),
            name: "knowledge_chunk_vector_index".to_string(),
            index_type: "vector".to_string(),
            fields: vec!["vector".to_string()],
            unique: false,
            sparse: false,
            params: serde_json::json!({
                "metric": "cosine",
                "dimension": 3072,
            }),
        };

        assert!(vector_index_definition_matches(&index, "vector", 3072));
        assert!(!vector_index_definition_matches(&index, "embedding", 3072));
        assert!(!vector_index_definition_matches(&index, "vector", 1536));
    }

    #[test]
    fn effective_vector_index_n_lists_tracks_available_source_rows() {
        assert_eq!(effective_vector_index_n_lists(100, 0), 1);
        assert_eq!(effective_vector_index_n_lists(100, 1), 1);
        assert_eq!(effective_vector_index_n_lists(100, 32), 32);
        assert_eq!(effective_vector_index_n_lists(100, 256), 100);
        assert_eq!(effective_vector_index_n_lists(0, 256), 1);
    }

    #[test]
    fn effective_vector_index_n_lists_memory_safe_caps_training_memory() {
        let dim = 3072_u64;
        let budget = VECTOR_INDEX_TRAINING_BUDGET_BYTES;

        // Empirical cap: budget / FAISS_EMPIRICAL_BYTES_PER_LIST = 1e9 / 50e6 = 20.
        let expected_cap = budget / FAISS_EMPIRICAL_BYTES_PER_LIST;

        // Configured nLists well above memory cap → capped at empirical limit.
        let capped = effective_vector_index_n_lists_memory_safe(10_000, 1_000_000, dim, budget);
        assert_eq!(capped, expected_cap, "should cap at empirical memory limit");

        // Configured nLists below memory cap → row-count clamp only.
        let small = effective_vector_index_n_lists_memory_safe(5, 1_000_000, dim, budget);
        assert_eq!(small, 5, "small nLists should not be reduced by memory cap");

        // Empty collection → minimum of 1.
        let empty = effective_vector_index_n_lists_memory_safe(100, 0, dim, budget);
        assert_eq!(empty, 1, "empty collection should yield nLists=1");

        // dimension parameter is ignored in the empirical model (no regression).
        let huge_dim = 1_000_000_u64;
        let floor = effective_vector_index_n_lists_memory_safe(100, 1_000_000, huge_dim, budget);
        assert_eq!(
            floor, expected_cap,
            "huge dim should still yield the empirical cap, not floor at 1"
        );
    }

    #[test]
    fn parse_vector_index_training_budget_falls_back_and_overrides() {
        // Unset / blank / whitespace-only → compiled-in default.
        assert_eq!(parse_vector_index_training_budget(None), VECTOR_INDEX_TRAINING_BUDGET_BYTES);
        assert_eq!(
            parse_vector_index_training_budget(Some(String::new())),
            VECTOR_INDEX_TRAINING_BUDGET_BYTES
        );
        assert_eq!(
            parse_vector_index_training_budget(Some("   ".to_string())),
            VECTOR_INDEX_TRAINING_BUDGET_BYTES
        );

        // Non-numeric / zero → default (zero would clamp nLists to 1).
        assert_eq!(
            parse_vector_index_training_budget(Some("not-a-number".to_string())),
            VECTOR_INDEX_TRAINING_BUDGET_BYTES
        );
        assert_eq!(
            parse_vector_index_training_budget(Some("0".to_string())),
            VECTOR_INDEX_TRAINING_BUDGET_BYTES
        );

        // Valid positive value (with surrounding whitespace) → parsed override.
        // 4 GB matches the docker-compose.large.yml overlay for bigger hosts.
        assert_eq!(
            parse_vector_index_training_budget(Some("  4000000000  ".to_string())),
            4_000_000_000
        );
    }

    /// Regression guard for a large per-dim shard scenario: a populated
    /// production-class library on a high-dim embedding model (~650k rows ×
    /// 3072-dim vectors) under the canonical 5 GiB Arango container budget
    /// declared in docker-compose.yml (`x-ironrag-resources-vector`) /
    /// values.yaml.
    ///
    /// The empirical IVF build cost is roughly 50 MB per nList. With the
    /// default 3 GB `VECTOR_INDEX_TRAINING_BUDGET_BYTES` the cap is 60
    /// lists, which fits below the 5 GiB container budget (Arango idle stays
    /// around 1.5 GiB, plus 3 GiB for the IVF build, giving a ~4.5 GiB peak
    /// with ~0.5 GiB headroom). That partitioning yields much faster query
    /// times than the previous nLists=20 setting.
    ///
    /// The invariant: `idle (~1.5 GiB) + budget` must stay under the Arango
    /// container cap. On a larger host the cap is raised (the
    /// `docker-compose.large.yml` overlay) and the budget bumped in lock-step
    /// via `IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES`; never raise the
    /// budget alone, or the OOM cycle returns.
    #[test]
    fn effective_vector_index_n_lists_large_shard_fits_within_canonical_arango_container() {
        let rows = 650_000_u64;
        let dim = 3072_u64;
        let budget = VECTOR_INDEX_TRAINING_BUDGET_BYTES;
        let max_safe_lists = budget / FAISS_EMPIRICAL_BYTES_PER_LIST;
        // Simulate a caller requesting a much larger nLists than the budget
        // allows (the old theoretical formula used to pass through ~317).
        let configured = 1_000_u64;

        let effective = effective_vector_index_n_lists_memory_safe(configured, rows, dim, budget);

        assert!(
            effective <= max_safe_lists,
            "effective nLists must not exceed empirical budget cap; got {effective} > {max_safe_lists}"
        );
        assert!(effective >= 1, "must return at least 1");
    }

    #[test]
    fn view_links_match_arango_normalized_response_shape() {
        let expected = serde_json::json!({
            "knowledge_document": {
                "includeAllFields": false,
                "fields": {
                    "external_key": { "analyzers": ["identity"] }
                }
            },
            "knowledge_chunk": {
                "includeAllFields": true,
                "fields": {
                    "content_text": { "analyzers": ["text_en", "text_ru"] },
                    "normalized_text": { "analyzers": ["text_en", "text_ru"] }
                }
            }
        });
        let actual = serde_json::json!({
            "knowledge_document": {
                "analyzers": ["identity"],
                "fields": {
                    "external_key": {}
                },
                "includeAllFields": false,
                "storeValues": "none",
                "trackListPositions": false
            },
            "knowledge_chunk": {
                "analyzers": ["identity"],
                "fields": {
                    "content_text": { "analyzers": ["text_en", "text_ru"] },
                    "normalized_text": { "analyzers": ["text_en", "text_ru"] }
                },
                "includeAllFields": true,
                "storeValues": "none",
                "trackListPositions": false
            }
        });

        assert!(view_links_semantically_match(&expected, &actual));
    }

    #[test]
    fn view_links_fail_when_expected_field_is_missing() {
        let expected = serde_json::json!({
            "knowledge_chunk": {
                "includeAllFields": true,
                "fields": {
                    "content_text": { "analyzers": ["text_en", "text_ru"] },
                    "normalized_text": { "analyzers": ["text_en", "text_ru"] }
                }
            }
        });
        let actual = serde_json::json!({
            "knowledge_chunk": {
                "analyzers": ["identity"],
                "fields": {
                    "content_text": { "analyzers": ["text_en", "text_ru"] }
                },
                "includeAllFields": true
            }
        });

        assert!(!view_links_semantically_match(&expected, &actual));
    }
}
