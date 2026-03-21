use anyhow::{Context, anyhow};
use reqwest::{Client, Method};
use serde::Serialize;
use tokio::time::{Duration, sleep};

use crate::app::config::Settings;

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
            return Ok(());
        }
        Err(anyhow!(
            "failed to ensure vector index {index_name} on {collection}: status {status}, body {response_body}",
        ))
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
        let response = self.request(Method::POST, "_api/cursor").json(&body).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let response_body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
            return Err(anyhow!("AQL query failed with status {status}, body {response_body}"));
        }
        response
            .json::<serde_json::Value>()
            .await
            .context("failed to decode ArangoDB cursor response")
    }

    async fn index_exists(&self, collection: &str, index_name: &str) -> anyhow::Result<bool> {
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
        Ok(indexes
            .iter()
            .any(|index| index.get("name").and_then(serde_json::Value::as_str) == Some(index_name)))
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

#[cfg(test)]
mod tests {
    use super::view_links_semantically_match;

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
                    "content_text": { "analyzers": ["text_en"] },
                    "normalized_text": { "analyzers": ["text_en"] }
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
                    "content_text": { "analyzers": ["text_en"] },
                    "normalized_text": { "analyzers": ["text_en"] }
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
                    "content_text": { "analyzers": ["text_en"] },
                    "normalized_text": { "analyzers": ["text_en"] }
                }
            }
        });
        let actual = serde_json::json!({
            "knowledge_chunk": {
                "analyzers": ["identity"],
                "fields": {
                    "content_text": { "analyzers": ["text_en"] }
                },
                "includeAllFields": true
            }
        });

        assert!(!view_links_semantically_match(&expected, &actual));
    }
}
