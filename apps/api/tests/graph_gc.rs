#![allow(clippy::expect_used, clippy::too_many_lines, clippy::unwrap_used)]

use std::{sync::Arc, time::Duration};

use anyhow::{Context as AnyhowContext, Result, anyhow};
use chrono::Utc;
use reqwest::{Client, StatusCode as ReqwestStatusCode};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use ironrag_backend::{
    app::config::Settings,
    infra::arangodb::{
        bootstrap::{ArangoBootstrapOptions, bootstrap_knowledge_plane},
        client::ArangoClient,
        collections::{
            KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_ENTITY_COLLECTION,
            KNOWLEDGE_EVIDENCE_COLLECTION, KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            KNOWLEDGE_RELATION_COLLECTION, KNOWLEDGE_RELATION_OBJECT_EDGE,
            KNOWLEDGE_RELATION_SUBJECT_EDGE, KNOWLEDGE_REVISION_COLLECTION,
        },
    },
    services::graph::gc::{Context as GraphGcContext, gc_zombie_nodes},
};

struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempPostgresDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let name = format!("graph_gc_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database for graph_gc")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(&format!("drop database if exists \"{name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {name}"))?;
        sqlx::query(&format!("create database \"{name}\""))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {name}"))?;
        admin_pool.close().await;

        Ok(Self { database_url: replace_database_name(base_database_url, &name)?, admin_url, name })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect postgres admin database for graph_gc cleanup")?;
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
    base_url: String,
    username: String,
    password: String,
    name: String,
    http: Client,
}

impl TempArangoDatabase {
    async fn create(settings: &Settings) -> Result<Self> {
        let base_url = settings.arangodb_url.trim().trim_end_matches('/').to_string();
        let name = format!("graph_gc_{}", Uuid::now_v7().simple());
        let http = Client::builder()
            .timeout(Duration::from_secs(settings.arangodb_request_timeout_seconds.max(1)))
            .build()
            .context("failed to build ArangoDB admin http client")?;
        let response = http
            .post(format!("{base_url}/_api/database"))
            .basic_auth(&settings.arangodb_username, Some(&settings.arangodb_password))
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .context("failed to create temp ArangoDB database for graph_gc")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "failed to create temp ArangoDB database {}: status {}",
                name,
                response.status()
            ));
        }

        Ok(Self {
            base_url,
            username: settings.arangodb_username.clone(),
            password: settings.arangodb_password.clone(),
            name,
            http,
        })
    }

    async fn drop(self) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/_api/database/{}", self.base_url, self.name))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .context("failed to drop temp ArangoDB database for graph_gc")?;
        if response.status() != ReqwestStatusCode::NOT_FOUND && !response.status().is_success() {
            return Err(anyhow!(
                "failed to drop temp ArangoDB database {}: status {}",
                self.name,
                response.status()
            ));
        }
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires local arangodb"]
async fn gc_zombie_nodes_deletes_orphan_entity_and_endpoint_relation() -> Result<()> {
    let mut settings = Settings::from_env().context("failed to load settings for graph_gc")?;
    let postgres = TempPostgresDatabase::create(&settings.database_url).await?;
    let arango = TempArangoDatabase::create(&settings).await?;
    settings.database_url = postgres.database_url.clone();
    settings.arangodb_database = arango.name.clone();

    let postgres_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&postgres.database_url)
        .await
        .context("failed to connect graph_gc postgres")?;
    sqlx::raw_sql(include_str!("../migrations/0001_init.sql"))
        .execute(&postgres_pool)
        .await
        .context("failed to apply canonical 0001_init.sql for graph_gc")?;

    let arango_client =
        Arc::new(ArangoClient::from_settings(&settings).context("failed to build Arango client")?);
    bootstrap_knowledge_plane(
        &arango_client,
        &ArangoBootstrapOptions {
            collections: true,
            views: true,
            graph: true,
            vector_indexes: false,
            vector_dimensions: 3072,
            vector_index_n_lists: 100,
            vector_index_default_n_probe: 8,
            vector_index_training_iterations: 25,
        },
    )
    .await
    .context("failed to bootstrap Arango knowledge plane for graph_gc")?;

    let library_id = Uuid::now_v7();
    let seeded = seed_graph_gc_fixture(&arango_client, library_id).await?;
    let context = GraphGcContext::new(Arc::clone(&arango_client), postgres_pool.clone());
    let report = gc_zombie_nodes(library_id, &context).await?;

    assert_eq!(report.entities_deleted, 1);
    assert_eq!(report.relations_deleted, 1);
    assert_eq!(report.libraries_scanned, 1);
    assert_eq!(count_by_library(&arango_client, KNOWLEDGE_ENTITY_COLLECTION, library_id).await?, 1);
    assert_eq!(
        count_by_library(&arango_client, KNOWLEDGE_RELATION_COLLECTION, library_id).await?,
        0
    );
    assert_eq!(
        count_document_by_key(&arango_client, KNOWLEDGE_ENTITY_COLLECTION, seeded.live_entity_id)
            .await?,
        1
    );
    assert_eq!(
        count_document_by_key(&arango_client, KNOWLEDGE_ENTITY_COLLECTION, seeded.zombie_entity_id)
            .await?,
        0
    );

    postgres_pool.close().await;
    arango.drop().await?;
    postgres.drop().await?;
    Ok(())
}

struct SeededGraph {
    live_entity_id: Uuid,
    zombie_entity_id: Uuid,
}

async fn seed_graph_gc_fixture(client: &ArangoClient, library_id: Uuid) -> Result<SeededGraph> {
    let workspace_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let evidence_id = Uuid::now_v7();
    let live_entity_id = Uuid::now_v7();
    let zombie_entity_id = Uuid::now_v7();
    let relation_id = Uuid::now_v7();
    let now = Utc::now();

    insert_one(
        client,
        KNOWLEDGE_DOCUMENT_COLLECTION,
        serde_json::json!({
            "_key": document_id,
            "document_id": document_id,
            "workspace_id": workspace_id,
            "library_id": library_id,
            "external_key": "synthetic/graph-gc.md",
            "file_name": "graph-gc.md",
            "title": "Graph GC Synthetic Fixture",
            "document_state": "active",
            "active_revision_id": revision_id,
            "readable_revision_id": revision_id,
            "latest_revision_no": 1,
            "created_at": now,
            "updated_at": now,
            "deleted_at": null,
        }),
    )
    .await?;
    insert_one(
        client,
        KNOWLEDGE_REVISION_COLLECTION,
        serde_json::json!({
            "_key": revision_id,
            "revision_id": revision_id,
            "workspace_id": workspace_id,
            "library_id": library_id,
            "document_id": document_id,
            "revision_number": 1,
            "revision_state": "active",
            "revision_kind": "synthetic",
            "mime_type": "text/plain",
            "checksum": "synthetic-checksum",
            "title": "Graph GC Synthetic Fixture",
            "byte_size": 64,
            "normalized_text": "synthetic graph gc fixture",
            "text_checksum": "synthetic-text-checksum",
            "text_state": "ready",
            "vector_state": "ready",
            "graph_state": "ready",
            "superseded_by_revision_id": null,
            "created_at": now,
        }),
    )
    .await?;
    insert_one(
        client,
        KNOWLEDGE_CHUNK_COLLECTION,
        serde_json::json!({
            "_key": chunk_id,
            "chunk_id": chunk_id,
            "workspace_id": workspace_id,
            "library_id": library_id,
            "document_id": document_id,
            "revision_id": revision_id,
            "chunk_index": 0,
            "chunk_kind": "text",
            "content_text": "synthetic graph gc fixture",
            "normalized_text": "synthetic graph gc fixture",
            "support_block_ids": [],
            "section_path": [],
            "heading_trail": [],
            "chunk_state": "ready",
        }),
    )
    .await?;
    for (entity_id, label) in
        [(live_entity_id, "Live Synthetic Entity"), (zombie_entity_id, "Zombie Synthetic Entity")]
    {
        insert_one(
            client,
            KNOWLEDGE_ENTITY_COLLECTION,
            serde_json::json!({
                "_key": entity_id,
                "entity_id": entity_id,
                "workspace_id": workspace_id,
                "library_id": library_id,
                "canonical_label": label,
                "aliases": [],
                "entity_type": "concept",
                "summary": null,
                "confidence": 0.9,
                "support_count": 1,
                "freshness_generation": 1,
                "entity_state": "active",
                "created_at": now,
                "updated_at": now,
            }),
        )
        .await?;
    }
    insert_one(
        client,
        KNOWLEDGE_EVIDENCE_COLLECTION,
        serde_json::json!({
            "_key": evidence_id,
            "evidence_id": evidence_id,
            "workspace_id": workspace_id,
            "library_id": library_id,
            "document_id": document_id,
            "revision_id": revision_id,
            "chunk_id": chunk_id,
            "quote_text": "synthetic graph gc fixture",
            "literal_spans_json": [],
            "evidence_kind": "synthetic",
            "extraction_method": "test",
            "confidence": 1.0,
            "evidence_state": "active",
            "freshness_generation": 1,
            "created_at": now,
            "updated_at": now,
        }),
    )
    .await?;
    insert_one(
        client,
        KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
        edge_doc(
            evidence_id,
            live_entity_id,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            KNOWLEDGE_ENTITY_COLLECTION,
            library_id,
        ),
    )
    .await?;
    insert_one(
        client,
        KNOWLEDGE_RELATION_COLLECTION,
        serde_json::json!({
            "_key": relation_id,
            "relation_id": relation_id,
            "workspace_id": workspace_id,
            "library_id": library_id,
            "predicate": "references",
            "normalized_assertion": "zombie references live",
            "confidence": 0.8,
            "support_count": 1,
            "contradiction_state": "none",
            "freshness_generation": 1,
            "relation_state": "active",
            "created_at": now,
            "updated_at": now,
        }),
    )
    .await?;
    insert_one(
        client,
        KNOWLEDGE_RELATION_SUBJECT_EDGE,
        edge_doc(
            relation_id,
            zombie_entity_id,
            KNOWLEDGE_RELATION_COLLECTION,
            KNOWLEDGE_ENTITY_COLLECTION,
            library_id,
        ),
    )
    .await?;
    insert_one(
        client,
        KNOWLEDGE_RELATION_OBJECT_EDGE,
        edge_doc(
            relation_id,
            live_entity_id,
            KNOWLEDGE_RELATION_COLLECTION,
            KNOWLEDGE_ENTITY_COLLECTION,
            library_id,
        ),
    )
    .await?;

    Ok(SeededGraph { live_entity_id, zombie_entity_id })
}

fn edge_doc(
    from_id: Uuid,
    to_id: Uuid,
    from_collection: &str,
    to_collection: &str,
    library_id: Uuid,
) -> serde_json::Value {
    serde_json::json!({
        "_key": format!("{from_id}:{to_id}"),
        "_from": format!("{from_collection}/{from_id}"),
        "_to": format!("{to_collection}/{to_id}"),
        "library_id": library_id,
    })
}

async fn insert_one(
    client: &ArangoClient,
    collection: &str,
    document: serde_json::Value,
) -> Result<()> {
    client
        .query_json(
            "INSERT @document INTO @@collection",
            serde_json::json!({
                "@collection": collection,
                "document": document,
            }),
        )
        .await
        .with_context(|| {
            format!("failed to insert synthetic graph GC document into {collection}")
        })?;
    Ok(())
}

async fn count_by_library(
    client: &ArangoClient,
    collection: &str,
    library_id: Uuid,
) -> Result<i64> {
    count_query(
        client,
        "RETURN LENGTH((
          FOR doc IN @@collection
            FILTER doc.library_id == @library_id
            RETURN 1
        ))",
        serde_json::json!({
            "@collection": collection,
            "library_id": library_id,
        }),
    )
    .await
}

async fn count_document_by_key(client: &ArangoClient, collection: &str, key: Uuid) -> Result<i64> {
    count_query(
        client,
        "RETURN LENGTH((
          FOR doc IN @@collection
            FILTER doc._key == @key
            RETURN 1
        ))",
        serde_json::json!({
            "@collection": collection,
            "key": key.to_string(),
        }),
    )
    .await
}

async fn count_query(
    client: &ArangoClient,
    query: &str,
    bind_vars: serde_json::Value,
) -> Result<i64> {
    let cursor = client.query_json(query, bind_vars).await.context("failed to count rows")?;
    cursor
        .get("result")
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| anyhow!("count query returned no integer row"))
}

fn replace_database_name(database_url: &str, new_name: &str) -> Result<String> {
    let mut url = url::Url::parse(database_url).context("invalid postgres database url")?;
    url.set_path(new_name);
    Ok(url.to_string())
}

async fn terminate_database_connections(pool: &sqlx::PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(pool)
    .await
    .with_context(|| format!("failed to terminate connections to {database_name}"))?;
    Ok(())
}
