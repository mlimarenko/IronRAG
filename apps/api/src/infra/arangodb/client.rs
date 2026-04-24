#![allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::missing_errors_doc)]

use anyhow::{Context, anyhow};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, sleep};

use crate::app::config::Settings;

#[derive(Debug, Clone, Deserialize)]
struct ArangoIndexRow {
    name: String,
    #[serde(rename = "type")]
    index_type: String,
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    unique: bool,
    #[serde(default)]
    sparse: bool,
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
        self.http
            .request(method, self.database_api_url(path))
            .basic_auth(&self.username, Some(&self.password))
    }

    fn system_request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.system_api_url(path))
            .basic_auth(&self.username, Some(&self.password))
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
        if self.index_exists(collection, index_name).await? {
            return Ok(());
        }

        let body = serde_json::json!({
            "name": index_name,
            "type": "vector",
            "fields": [field],
            "params": {
                "metric": "cosine",
                "dimension": dimension,
                "nLists": n_lists,
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
            return Ok(());
        }
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();
        if status.as_u16() == 400
            && (response_body.contains("Number of training points")
                || response_body.contains("nx >= k"))
        {
            self.seed_vector_training_rows(collection, field, dimension, n_lists).await?;
            let retry = self
                .request(Method::POST, &format!("_api/index?collection={collection}"))
                .json(&body)
                .send()
                .await?;
            if retry.status().is_success() || retry.status().as_u16() == 409 {
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

    pub async fn query_json(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::json!({
            "query": query,
            "bindVars": bind_vars,
        });
        let mut cursor =
            self.send_cursor_request(Method::POST, "_api/cursor", Some(&body), "AQL query").await?;
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
        mut handle_batch: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(Vec<serde_json::Value>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<()>>,
    {
        const BULK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
        let body = serde_json::json!({
            "query": query,
            "bindVars": bind_vars,
        });
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
            handle_batch(next_rows).await?;
            cursor["hasMore"] =
                next_cursor.get("hasMore").cloned().unwrap_or(serde_json::Value::Bool(false));
            if let Some(id) = next_cursor.get("id").cloned() {
                cursor["id"] = id;
            } else if let Some(object) = cursor.as_object_mut() {
                object.remove("id");
            }
        }
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
        self.send_cursor_request_with_timeout(
            Method::POST,
            "_api/cursor",
            Some(&body),
            "AQL bulk query",
            Some(BULK_TIMEOUT),
        )
        .await
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

    async fn index_exists(&self, collection: &str, index_name: &str) -> anyhow::Result<bool> {
        Ok(self.find_index_by_name(collection, index_name).await?.is_some())
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
        ArangoClient, ArangoIndexRow, persistent_index_definition_matches,
        view_links_semantically_match,
    };

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
