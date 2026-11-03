//! Opt-in native lexical experiment. Never reads application DSNs or calls a model.

use std::{collections::HashMap, fs, path::PathBuf, time::Instant};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use ironrag_backend::infra::{
    knowledge_plane::{ChunkLexicalQuery, LexicalQueryOrigin, SearchStore},
    postgres::pg_search_store::PgSearchStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

#[derive(Parser)]
struct Args {
    /// Explicit fresh local database irg285_<run>; no environment fallback.
    #[arg(long)]
    dsn: String,
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    manifest_sha256: String,
    documents: Vec<Document>,
    queries: Vec<Query>,
    limit: usize,
    warmups: usize,
    repetitions: usize,
}

#[derive(Deserialize, Serialize)]
struct Document {
    key: String,
    path: String,
    sha256: String,
    text: String,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    title: String,
}

#[derive(Deserialize)]
struct Query {
    id: String,
    query: String,
    bm25_query: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct Hit {
    chunk_id: Uuid,
    document_key: String,
    score: f64,
    text_sha256: String,
}

#[derive(FromRow)]
struct RawHit {
    chunk_id: Uuid,
    score: f64,
}

const ARMS: [&str; 3] = ["production_fts", "raw_fts", "raw_bm25"];
const LIBRARY_ID: Uuid = Uuid::from_u128(0x28500000_0000_4000_8000_000000000002);

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_dsn(dsn: &str) -> Result<()> {
    let url = url::Url::parse(dsn).context("invalid explicit experiment DSN")?;
    ensure!(
        url.scheme() == "postgresql"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some_and(|port| port != 5432)
            && url.username() == "postgres"
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path().strip_prefix("/irg285_").is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
            && url.query_pairs().all(|(key, value)| key == "sslmode" && value == "disable"),
        "refusing target: require password-free explicit loopback port and fresh irg285_<run> database"
    );
    Ok(())
}

fn load_manifest(path: &PathBuf) -> Result<(Manifest, Value)> {
    let mut value: Value = serde_json::from_slice(&fs::read(path)?)?;
    let expected = value["manifest_sha256"].as_str().context("manifest digest missing")?.to_owned();
    value.as_object_mut().context("manifest must be an object")?.remove("manifest_sha256");
    value.sort_all_objects();
    // Compact sorted UTF-8 matches the Python manifest owner.
    ensure!(digest(&serde_json::to_vec(&value)?) == expected, "manifest digest mismatch");
    value["manifest_sha256"] = json!(expected);
    let manifest: Manifest = serde_json::from_value(value.clone())?;
    ensure!(manifest.schema_version == 1 && manifest.limit == 10, "unsupported manifest contract");
    ensure!(!manifest.documents.is_empty() && !manifest.queries.is_empty(), "empty experiment");
    ensure!(
        manifest.warmups > 0 && (2..=100).contains(&manifest.repetitions),
        "invalid sample budget"
    );
    let mut identities = std::collections::HashSet::new();
    for doc in &manifest.documents {
        ensure!(
            digest(doc.text.as_bytes()) == doc.sha256,
            "fixture checksum mismatch: {}",
            doc.key
        );
        ensure!(identities.insert(doc.chunk_id), "duplicate chunk identity");
    }
    Ok((manifest, value))
}

async fn prepare(pool: &PgPool, manifest: &Manifest) -> Result<Value> {
    let tables: i64 = sqlx::query_scalar("select count(*) from pg_tables where schemaname not in ('pg_catalog','information_schema')")
        .fetch_one(pool).await?;
    ensure!(
        tables == 0,
        "refusing nonempty experiment database; create another fresh irg285_<run>"
    );
    let start = Instant::now();
    sqlx::migrate!("./migrations").run(pool).await?;
    sqlx::raw_sql("set search_path=public; create extension pg_search;").execute(pool).await?;
    let migration_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    // Several statements share a bound JSON value. Split only the static seed;
    // fixture content never enters SQL text. Keep the entire seed atomic.
    let mut tx = pool.begin().await?;
    for statement in include_str!("seed.sql").split(';').filter(|sql| !sql.trim().is_empty()) {
        let query = sqlx::query(sqlx::AssertSqlSafe(statement));
        if statement.contains("$1") {
            query.bind(serde_json::to_value(&manifest.documents)?).execute(&mut *tx).await?;
        } else {
            query.execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    let seed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut setup = json!({"migration_ms":migration_ms,"seed_ms":seed_ms});
    for (name, sql) in [
        ("raw_fts_index_ms", "create index lexical_raw_fts on lexical_raw using gin(search_tsv)"),
        ("raw_bm25_index_ms", "create index lexical_raw_bm25 on lexical_raw using bm25(id,body)"),
    ] {
        let start = Instant::now();
        sqlx::raw_sql(sql).execute(pool).await?;
        setup[name] = json!(start.elapsed().as_secs_f64() * 1000.0);
    }
    sqlx::raw_sql("analyze knowledge_chunk; analyze knowledge_document; analyze lexical_raw;")
        .execute(pool)
        .await?;
    let visible: i64 =
        sqlx::query_scalar("select count(*) from content_current_revision").fetch_one(pool).await?;
    ensure!(
        visible == manifest.documents.len() as i64,
        "seed did not publish exactly the declared corpus"
    );
    let equal: bool = sqlx::query_scalar(
        "select bool_and(search_tsv=to_tsvector('simple',body)) from lexical_raw",
    )
    .fetch_one(pool)
    .await?;
    ensure!(equal, "diagnostic input differs from production generated FTS text");
    Ok(setup)
}

async fn search(
    store: &PgSearchStore,
    arm: &str,
    query: &Query,
    limit: usize,
) -> Result<Vec<RawHit>> {
    if arm == "production_fts" {
        return Ok(store
            .search_chunks(ChunkLexicalQuery {
                library_id: LIBRARY_ID,
                query: &query.query,
                query_origin: LexicalQueryOrigin::UserQuery,
                limit,
                temporal_start: None,
                temporal_end: None,
                text_search_config: None,
            })
            .await?
            .into_iter()
            .map(|row| RawHit { chunk_id: row.chunk_id, score: row.score })
            .collect());
    }
    let (sql, input) = match arm {
        "raw_fts" => (
            "select chunk_id, ts_rank_cd(search_tsv,websearch_to_tsquery('simple',ironrag_unaccent($1)))::float8 as score from lexical_raw where search_tsv @@ websearch_to_tsquery('simple',ironrag_unaccent($1)) order by score desc,chunk_id limit $2",
            &query.query,
        ),
        "raw_bm25" => (
            "select chunk_id,pdb.score(id)::float8 as score from lexical_raw where body @@@ pdb.parse($1,lenient=>false,conjunction_mode=>true) order by score desc,chunk_id limit $2",
            &query.bm25_query,
        ),
        _ => anyhow::bail!("unknown experiment arm"),
    };
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(input)
        .bind(limit as i64)
        .fetch_all(&store.pool)
        .await?)
}

async fn run(args: &Args) -> Result<Value> {
    validate_dsn(&args.dsn)?;
    ensure!(!args.output.exists(), "refusing to overwrite an existing receipt");
    let (manifest, _) = load_manifest(&args.manifest)?;
    let pool = PgPoolOptions::new().max_connections(1).acquire_timeout(std::time::Duration::from_secs(10))
        .after_connect(|connection, _| Box::pin(async move {
            sqlx::raw_sql("set search_path=public; set statement_timeout='30s'; set lock_timeout='5s'; set max_parallel_workers_per_gather=0").execute(connection).await?;
            Ok(())
        })).connect(&args.dsn).await.context("connecting to explicit disposable target")?;
    let setup = prepare(&pool, &manifest).await?;
    // The baseline migration sets session timeouts to zero: restore query bounds.
    sqlx::raw_sql("set statement_timeout='30s'; set lock_timeout='5s'").execute(&pool).await?;
    let versions: (String,String,String) = sqlx::query_as("select current_setting('server_version'),(select extversion from pg_extension where extname='pg_search'),(select extversion from pg_extension where extname='vector')").fetch_one(&pool).await?;
    ensure!(versions.0.starts_with("18.") && versions.1 == "0.26.2", "unexpected native versions");
    let store = PgSearchStore { pool: pool.clone() };
    let docs: HashMap<_, _> = manifest.documents.iter().map(|doc| (doc.chunk_id, doc)).collect();
    let mut results = Vec::new();
    for query in &manifest.queries {
        let mut hits: HashMap<&str, Vec<Hit>> = HashMap::new();
        let mut samples: HashMap<&str, Vec<f64>> = HashMap::new();
        for iteration in 0..(manifest.warmups + manifest.repetitions) {
            // Rotate arm order each round; setup and warmup never enter samples.
            for offset in 0..ARMS.len() {
                let arm = ARMS[(iteration + offset) % ARMS.len()];
                let start = Instant::now();
                let rows = search(&store, arm, query, manifest.limit)
                    .await
                    .with_context(|| format!("native query failed: {arm}/{}", query.id))?;
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let current = rows
                    .into_iter()
                    .map(|row| {
                        let doc = docs
                            .get(&row.chunk_id)
                            .context("native query returned unknown chunk")?;
                        ensure!(row.score.is_finite(), "nonfinite native score");
                        Ok(Hit {
                            chunk_id: row.chunk_id,
                            document_key: doc.key.clone(),
                            score: row.score,
                            text_sha256: doc.sha256.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if let Some(previous) = hits.get(arm) {
                    ensure!(
                        *previous == current,
                        "unstable native ranks or scores: {arm}/{}",
                        query.id
                    );
                } else {
                    hits.insert(arm, current);
                }
                if iteration >= manifest.warmups {
                    samples.entry(arm).or_default().push(elapsed);
                }
            }
        }
        for arm in ARMS {
            results.push(
                json!({"arm":arm,"query_id":query.id,"samples_ms":samples[arm],"hits":hits[arm]}),
            );
        }
    }
    let sizes: Vec<(String,i64)> = sqlx::query_as("select indexrelname,pg_relation_size(indexrelid)::bigint from pg_stat_user_indexes where relname in ('knowledge_chunk','lexical_raw') order by indexrelname").fetch_all(&pool).await?;
    let settings: Vec<(String,String)> = sqlx::query_as("select name,setting from pg_settings where name in ('shared_buffers','work_mem','effective_cache_size','max_parallel_workers_per_gather','jit','shared_preload_libraries') order by name").fetch_all(&pool).await?;
    Ok(json!({"schema_version":1,"manifest_sha256":manifest.manifest_sha256,"arms":ARMS,
        "versions":{"postgres":versions.0,"pg_search":versions.1,"vector":versions.2},
        "settings":settings.into_iter().collect::<HashMap<_,_>>(),"setup_ms":setup,
        "index_bytes":sizes.into_iter().collect::<HashMap<_,_>>(),"results":results,
        "rank_stability":"identical across warmups and measured repetitions",
        "model_calls":0,"cache_context":"warm in-process queries; no OS/page cache flush; rotated arm order"}))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let result = run(&args).await?;
    use std::io::Write;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&args.output)?;
    file.write_all(&serde_json::to_vec_pretty(&result)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_implicit_shared_and_redirected_targets() {
        for dsn in [
            "postgresql://postgres@localhost:32972/irg285_x",
            "postgresql://postgres@127.0.0.1:5432/irg285_x",
            "postgresql://postgres@127.0.0.1:32972/ironrag",
            "postgresql://postgres@127.0.0.1:32972/irg285_x?host=elsewhere",
            "postgresql://postgres:secret@127.0.0.1:32972/irg285_x",
        ] {
            assert!(validate_dsn(dsn).is_err());
        }
        assert!(
            validate_dsn("postgresql://postgres@127.0.0.1:32972/irg285_run1?sslmode=disable")
                .is_ok()
        );
    }
}
