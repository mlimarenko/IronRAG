use anyhow::{Context, Result};
use reqwest::Client;
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::collections::{
            DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
            KNOWLEDGE_CHUNK_VECTOR_INDEX, KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
            KNOWLEDGE_ENTITY_VECTOR_INDEX, KNOWLEDGE_GRAPH_NAME, KNOWLEDGE_PERSISTENT_INDEXES,
            KNOWLEDGE_SEARCH_VIEW,
        },
        persistence::{canonical_ai_catalog_seeded, canonical_baseline_present},
    },
};

const SEEDED_PROVIDER_COUNT: i64 = 3;
const SEEDED_MODEL_COUNT: i64 = 40;
const SEEDED_PRICE_COUNT: i64 = 118;

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("bootstrap_stack_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect bootstrap-stack admin postgres")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(&format!("drop database if exists \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {database_name}"))?;
        sqlx::query(&format!("create database \"{database_name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {database_name}"))?;
        admin_pool.close().await;

        let database_url = replace_database_name(base_database_url, &database_name)?;

        Ok(Self { name: database_name, admin_url, database_url })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect bootstrap-stack admin postgres for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(&format!("drop database if exists \"{}\"", self.name))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct TempArangoDatabase {
    name: String,
    base_url: String,
    username: String,
    password: String,
    http: Client,
}

impl TempArangoDatabase {
    fn new(base_url: &str, username: &str, password: &str) -> Result<Self> {
        Ok(Self {
            name: format!("bootstrap_stack_{}", Uuid::now_v7().simple()),
            base_url: base_url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .context("failed to build bootstrap-stack arango client")?,
        })
    }

    fn db_api_url(&self, path: &str) -> String {
        format!("{}/_db/{}/{}", self.base_url, self.name, path.trim_start_matches('/'))
    }

    async fn database_exists(&self) -> Result<bool> {
        let response = self
            .http
            .get(format!("{}/_api/database/user", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to list Arango databases")?;
        let payload = response
            .error_for_status()
            .context("Arango database listing failed")?
            .json::<serde_json::Value>()
            .await
            .context("failed to decode Arango database list")?;
        let names = payload
            .get("result")
            .and_then(serde_json::Value::as_array)
            .context("Arango database list missing `result`")?;
        Ok(names.iter().any(|name| name.as_str() == Some(self.name.as_str())))
    }

    async fn collection_names(&self) -> Result<Vec<String>> {
        let payload = self
            .http
            .get(self.db_api_url("_api/collection"))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to list Arango collections")?
            .error_for_status()
            .context("Arango collection listing failed")?
            .json::<serde_json::Value>()
            .await
            .context("failed to decode Arango collection list")?;
        let collections = payload
            .get("result")
            .and_then(serde_json::Value::as_array)
            .context("Arango collection list missing `result`")?;
        Ok(collections
            .iter()
            .filter_map(|row| row.get("name").and_then(serde_json::Value::as_str))
            .filter(|name| !name.starts_with('_'))
            .map(ToOwned::to_owned)
            .collect())
    }

    async fn has_view(&self, view_name: &str) -> Result<bool> {
        let response = self
            .http
            .get(self.db_api_url(&format!("_api/view/{view_name}")))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to read Arango view")?;
        Ok(response.status().is_success())
    }

    async fn has_graph(&self, graph_name: &str) -> Result<bool> {
        let response = self
            .http
            .get(self.db_api_url(&format!("_api/gharial/{graph_name}")))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to read Arango named graph")?;
        Ok(response.status().is_success())
    }

    async fn has_index(&self, collection: &str, index_name: &str) -> Result<bool> {
        let payload = self
            .http
            .get(self.db_api_url(&format!("_api/index?collection={collection}")))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to list Arango indexes")?
            .error_for_status()
            .context("Arango index listing failed")?
            .json::<serde_json::Value>()
            .await
            .context("failed to decode Arango index list")?;
        let indexes = payload
            .get("indexes")
            .and_then(serde_json::Value::as_array)
            .context("Arango index list missing `indexes`")?;
        Ok(indexes
            .iter()
            .any(|row| row.get("name").and_then(serde_json::Value::as_str) == Some(index_name)))
    }

    async fn has_persistent_index(
        &self,
        collection: &str,
        index_name: &str,
        fields: &[&str],
        unique: bool,
        sparse: bool,
    ) -> Result<bool> {
        let payload = self
            .http
            .get(self.db_api_url(&format!("_api/index?collection={collection}")))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to list Arango indexes")?
            .error_for_status()
            .context("Arango index listing failed")?
            .json::<serde_json::Value>()
            .await
            .context("failed to decode Arango index list")?;
        let indexes = payload
            .get("indexes")
            .and_then(serde_json::Value::as_array)
            .context("Arango index list missing `indexes`")?;

        Ok(indexes.iter().any(|row| {
            row.get("name").and_then(serde_json::Value::as_str) == Some(index_name)
                && row.get("type").and_then(serde_json::Value::as_str) == Some("persistent")
                && row.get("fields").and_then(serde_json::Value::as_array).is_some_and(
                    |actual_fields| {
                        actual_fields.len() == fields.len()
                            && actual_fields
                                .iter()
                                .zip(fields.iter().copied())
                                .all(|(actual, expected)| actual.as_str() == Some(expected))
                    },
                )
                && row.get("unique").and_then(serde_json::Value::as_bool) == Some(unique)
                && row.get("sparse").and_then(serde_json::Value::as_bool) == Some(sparse)
        }))
    }

    async fn drop(self) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/_api/database/{}", self.base_url, self.name))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to delete Arango test database")?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "failed to delete Arango test database {}: status {}",
            self.name,
            response.status()
        ))
    }
}

struct BootstrapStackFixture {
    state: AppState,
    temp_database: TempDatabase,
    temp_arango: TempArangoDatabase,
}

impl BootstrapStackFixture {
    async fn create() -> Result<Self> {
        let mut settings =
            Settings::from_env().context("failed to load settings for bootstrap-stack test")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let temp_arango = TempArangoDatabase::new(
            &settings.arangodb_url,
            &settings.arangodb_username,
            &settings.arangodb_password,
        )?;
        settings.database_url = temp_database.database_url.clone();
        settings.arangodb_database = temp_arango.name.clone();
        settings.destructive_fresh_bootstrap_required = true;
        settings.destructive_allow_legacy_startup_side_effects = false;
        settings.legacy_ui_bootstrap_enabled = false;
        settings.legacy_bootstrap_token_endpoint_enabled = false;
        settings.bootstrap_claim_enabled = true;

        let state = AppState::new(settings).await?;
        Ok(Self { state, temp_database, temp_arango })
    }

    async fn cleanup(self) -> Result<()> {
        self.state.persistence.postgres.close().await;
        self.temp_database.drop().await?;
        self.temp_arango.drop().await
    }
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database url is missing database name: {database_url}"))?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("failed to terminate connections for {database_name}"))?;
    Ok(())
}

async fn scalar_count(postgres: &PgPool, table_name: &str) -> Result<i64> {
    sqlx::query_scalar::<_, i64>(&format!("select count(*) from {table_name}"))
        .fetch_one(postgres)
        .await
        .with_context(|| format!("failed to count rows in {table_name}"))
}

#[tokio::test]
#[ignore = "requires local postgres, redis, and arango services"]
async fn fresh_startup_bootstraps_postgres_catalog_and_arango_knowledge_plane() -> Result<()> {
    let fixture = BootstrapStackFixture::create().await?;

    let result = async {
        assert!(canonical_baseline_present(&fixture.state.persistence.postgres).await?);
        assert!(canonical_ai_catalog_seeded(&fixture.state.persistence.postgres).await?);
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_provider_catalog").await?,
            SEEDED_PROVIDER_COUNT
        );
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_model_catalog").await?,
            SEEDED_MODEL_COUNT
        );
        assert_eq!(
            scalar_count(&fixture.state.persistence.postgres, "ai_price_catalog").await?,
            SEEDED_PRICE_COUNT
        );

        assert!(fixture.temp_arango.database_exists().await?);

        let collections = fixture.temp_arango.collection_names().await?;
        for collection in DOCUMENT_COLLECTIONS {
            assert!(
                collections.iter().any(|candidate| candidate == collection),
                "missing document collection {collection}"
            );
        }
        for collection in EDGE_COLLECTIONS {
            assert!(
                collections.iter().any(|candidate| candidate == collection),
                "missing edge collection {collection}"
            );
        }

        assert!(fixture.temp_arango.has_view(KNOWLEDGE_SEARCH_VIEW).await?);
        assert!(fixture.temp_arango.has_graph(KNOWLEDGE_GRAPH_NAME).await?);
        assert!(
            fixture
                .temp_arango
                .has_index(KNOWLEDGE_CHUNK_VECTOR_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_INDEX)
                .await?
        );
        assert!(
            fixture
                .temp_arango
                .has_index(KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_INDEX)
                .await?
        );
        for index in KNOWLEDGE_PERSISTENT_INDEXES {
            assert!(
                fixture
                    .temp_arango
                    .has_persistent_index(
                        index.collection,
                        index.name,
                        index.fields,
                        index.unique,
                        index.sparse,
                    )
                    .await?,
                "missing or mismatched persistent index {} on {}",
                index.name,
                index.collection
            );
        }

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
