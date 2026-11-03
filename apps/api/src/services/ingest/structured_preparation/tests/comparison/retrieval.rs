//! Opt-in native lexical replay of pinned treatment windows, without providers.
//! The create-new JSONL journal retains first observations even if a later step fails.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use uuid::Uuid;

use crate::{
    domains::knowledge::TypedTechnicalFact,
    infra::{
        knowledge_plane::{ChunkLexicalQuery, LexicalQueryOrigin, SearchStore},
        knowledge_rows::{
            KnowledgeChunkSearchRow, KnowledgeStructuredBlockSearchRow,
            KnowledgeTechnicalFactSearchRow,
        },
        postgres::pg_search_store::PgSearchStore,
    },
    shared::extraction::structured_document::{
        StructuredBlockData, StructuredChunkWindow, validate_ordered_semantic_blocks,
        validate_structured_chunk_windows,
    },
};

const INPUT_CAP: u64 = 64 * 1024 * 1024;
const SUPPORT_LIMIT: usize = 16;
const SUPPORT_PAYLOAD_CAP: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetrievalLane {
    Chunks,
    StructuredBlocks,
    TechnicalFacts,
}

impl RetrievalLane {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value {
            None | Some("chunks") => Ok(Self::Chunks),
            Some("structured_blocks") => Ok(Self::StructuredBlocks),
            Some("technical_facts") => Ok(Self::TechnicalFacts),
            _ => anyhow::bail!(
                "invalid ASSERTION_RETRIEVAL_LANE: require chunks, structured_blocks or technical_facts"
            ),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Chunks => "chunks",
            Self::StructuredBlocks => "structured_blocks",
            Self::TechnicalFacts => "technical_facts",
        }
    }
}

enum NativeHits {
    Chunks(Vec<KnowledgeChunkSearchRow>),
    StructuredBlocks(Vec<KnowledgeStructuredBlockSearchRow>),
    TechnicalFacts(Vec<KnowledgeTechnicalFactSearchRow>),
}

fn validate_lane_arms(lane: RetrievalLane, arms: &[Arm<'_>]) -> Result<()> {
    if lane == RetrievalLane::StructuredBlocks {
        ensure!(
            arms.iter()
                .flat_map(|arm| &arm.documents)
                .all(|document| document.treatment.blocks == document.case.blocks),
            "structured_blocks requires unchanged canonical blocks; derived treatment blocks are not admitted"
        );
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Export {
    protocol: String,
    source_sha: String,
    input_manifest_sha256: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    case_id: String,
    category: String,
    source_sha256: String,
    normalized_sha256: String,
    normalized_text: String,
    blocks: Vec<StructuredBlockData>,
    #[serde(default)]
    technical_facts: Vec<TypedTechnicalFact>,
    treatments: Vec<Treatment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Treatment {
    treatment_id: String,
    profile_id: String,
    max_chars: usize,
    overlap_chars: usize,
    preparation_ms: f64,
    blocks: Vec<StructuredBlockData>,
    chunks: Vec<StructuredChunkWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Annotations {
    protocol: String,
    input_manifest_sha256: String,
    cases: Vec<Question>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Question {
    id: String,
    document_id: String,
    question: String,
    expected_facts: Vec<String>,
}

struct Arm<'a> {
    treatment_id: &'a str,
    profile_id: &'a str,
    pdf_only: bool,
    documents: Vec<CorpusDocument<'a>>,
}

#[derive(Clone, Copy)]
struct CorpusDocument<'a> {
    case: &'a Case,
    treatment: &'a Treatment,
}

#[derive(Clone, Copy)]
struct DocumentSeed<'a> {
    workspace: Uuid,
    library: Uuid,
    document: Uuid,
    revision: Uuid,
    corpus: CorpusDocument<'a>,
}

struct IndexedChunk<'a> {
    seed: DocumentSeed<'a>,
    chunk: &'a StructuredChunkWindow,
}

struct SeededCorpus<'a> {
    libraries: Vec<Uuid>,
    hit_map: HashMap<Uuid, IndexedChunk<'a>>,
    documents: HashMap<Uuid, DocumentSeed<'a>>,
}

fn validate_facts(case: &Case) -> Result<()> {
    ensure!(case.technical_facts.len() <= 4096, "case exceeds 4096 technical facts");
    let blocks: HashSet<_> = case.blocks.iter().map(|block| block.block_id).collect();
    let mut ids = HashSet::new();
    let mut scope = None;
    for fact in &case.technical_facts {
        ensure!(
            !fact.fact_id.is_nil() && ids.insert(fact.fact_id),
            "duplicate/empty fact identity"
        );
        let current = (fact.workspace_id, fact.library_id, fact.document_id, fact.revision_id);
        ensure!(
            [current.0, current.1, current.2, current.3].iter().all(|id| !id.is_nil()),
            "empty original fact scope"
        );
        ensure!(
            scope.is_none_or(|previous| previous == current),
            "inconsistent original fact scope"
        );
        scope = Some(current);
        ensure!(fact.confidence.is_none_or(f64::is_finite), "nonfinite fact confidence");
        ensure!(!fact.support_block_ids.is_empty(), "fact has no source blocks");
        let mut supports = HashSet::new();
        ensure!(
            fact.support_block_ids.iter().all(|id| blocks.contains(id) && supports.insert(*id)),
            "unresolved/duplicate fact source support"
        );
        // This export has canonical block IDs but no original chunk-ID registry.
        // Refuse opaque foreign chunk links rather than silently dropping/reusing them.
        ensure!(fact.support_chunk_ids.is_empty(), "unresolved original fact chunk support");
    }
    Ok(())
}

fn remap_fact(seed: DocumentSeed<'_>, original: &TypedTechnicalFact) -> TypedTechnicalFact {
    let mut fact = original.clone();
    fact.fact_id = identity(&[&seed.revision.to_string(), "fact", &original.fact_id.to_string()]);
    fact.workspace_id = seed.workspace;
    fact.library_id = seed.library;
    fact.document_id = seed.document;
    fact.revision_id = seed.revision;
    fact.support_block_ids = original
        .support_block_ids
        .iter()
        .map(|id| identity(&[&seed.revision.to_string(), "block", &id.to_string()]))
        .collect();
    // Match canonical preparation's block-to-chunk expansion in the fresh arm.
    fact.support_chunk_ids = seed
        .corpus
        .treatment
        .chunks
        .iter()
        .filter(|chunk| {
            chunk.support_block_ids.iter().any(|id| original.support_block_ids.contains(id))
        })
        .map(|chunk| chunk_id(seed.revision, chunk))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    fact
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_hash(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn pinned(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(is_hash(expected, 64), "invalid file SHA256 pin");
    let file = File::open(path).context("cannot open pinned replay input")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "replay input must be a regular file");
    ensure!(metadata.len() <= INPUT_CAP, "replay input exceeds 64 MiB");
    let mut bytes = Vec::new();
    file.take(INPUT_CAP + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= INPUT_CAP, "replay input exceeds 64 MiB");
    ensure!(digest(&bytes) == expected, "replay input SHA256 mismatch");
    Ok(bytes)
}

/// Parsing errors deliberately omit the supplied DSN, including any credentials.
fn validate_dsn(dsn: &str) -> Result<url::Url> {
    let url = url::Url::parse(dsn).map_err(|_| anyhow::anyhow!("invalid replay DSN"))?;
    ensure!(
        url.as_str() == dsn
            && url.scheme() == "postgresql"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some_and(|port| port != 0 && port != 5432)
            && url.username() == "postgres"
            && url.password().is_none()
            && url.fragment().is_none()
            && url.path().strip_prefix("/irg256_").is_some_and(|name| {
                !name.is_empty()
                    && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && url.path().len() <= 64
            })
            && url.query() == Some("sslmode=disable"),
        "refusing target: require password-free postgres loopback, explicit non-5432 port, new irg256_<name>, and only sslmode=disable"
    );
    Ok(url)
}

fn require_fresh_database(preexisting: bool, objects: i64) -> Result<()> {
    ensure!(!preexisting && objects == 0, "refusing preexisting or nonempty replay database");
    Ok(())
}

/// Length-prefixed keys avoid ambiguous concatenation; UUIDs are SHA256-derived.
fn identity(parts: &[&str]) -> Uuid {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let hash = hash.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // RFC 9562 custom version 8; not a claim to implement UUIDv5's SHA1 algorithm.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn validate_inputs<'a>(export: &'a Export, annotations: &Annotations) -> Result<Vec<Arm<'a>>> {
    ensure!(export.protocol == "irg256-native-treatment-export/1", "wrong treatment protocol");
    ensure!(is_hash(&export.source_sha, 40), "invalid treatment source SHA");
    ensure!(is_hash(&export.input_manifest_sha256, 64), "invalid input manifest SHA256");
    ensure!(
        annotations.protocol == "irg256-pilot-query-annotations/1"
            && annotations.input_manifest_sha256 == export.input_manifest_sha256,
        "annotation protocol or corpus identity mismatch"
    );
    ensure!(export.cases.len() == 4, "replay requires the four-document category corpus");
    ensure!((1..=64).contains(&annotations.cases.len()), "invalid query count");
    let mut cases = HashSet::new();
    let mut sources = HashSet::new();
    let mut categories = HashSet::new();
    let mut original_fact_ids = HashSet::new();
    let mut groups: BTreeMap<(&str, &str), Vec<CorpusDocument<'_>>> = BTreeMap::new();
    let mut result_count = 0;
    for case in &export.cases {
        ensure!(
            !case.case_id.trim().is_empty() && cases.insert(&case.case_id),
            "duplicate/empty case ID"
        );
        ensure!(
            !case.category.is_empty() && categories.insert(&case.category),
            "duplicate/empty category"
        );
        ensure!(
            is_hash(&case.source_sha256, 64) && sources.insert(&case.source_sha256),
            "invalid/duplicate source identity"
        );
        ensure!(
            digest(case.normalized_text.as_bytes()) == case.normalized_sha256,
            "normalized document identity mismatch"
        );
        ensure!(!case.normalized_text.trim().is_empty(), "empty normalized document");
        validate_ordered_semantic_blocks(&case.blocks)?;
        validate_facts(case)?;
        ensure!(
            case.technical_facts.iter().all(|fact| original_fact_ids.insert(fact.fact_id)),
            "duplicate original fact ID across cases"
        );
        ensure!(
            case.blocks.iter().map(|b| b.block_id).collect::<HashSet<_>>().len()
                == case.blocks.len(),
            "duplicate canonical block identity"
        );
        ensure!(!case.treatments.is_empty(), "case has no treatments");
        let mut treatments = HashSet::new();
        for treatment in &case.treatments {
            result_count += 1;
            ensure!(result_count <= 256, "more than 256 treatment results");
            ensure!(
                !treatment.treatment_id.trim().is_empty()
                    && !treatment.profile_id.trim().is_empty()
                    && treatments.insert((&treatment.treatment_id, &treatment.profile_id)),
                "duplicate/empty treatment identity"
            );
            ensure!(
                treatment.max_chars > 0 && treatment.overlap_chars < treatment.max_chars,
                "invalid character profile"
            );
            ensure!(
                treatment.preparation_ms.is_finite() && treatment.preparation_ms >= 0.0,
                "invalid preparation measurement"
            );
            ensure!(
                !treatment.chunks.is_empty() && treatment.chunks.len() <= 4096,
                "empty/oversized treatment"
            );
            validate_ordered_semantic_blocks(&treatment.blocks)?;
            for fact in &case.technical_facts {
                for id in &fact.support_block_ids {
                    let canonical = case
                        .blocks
                        .iter()
                        .find(|block| block.block_id == *id)
                        .context("missing canonical fact support")?;
                    ensure!(
                        treatment.blocks.iter().any(|block| block == canonical),
                        "treatment omits or changes original fact support"
                    );
                }
            }
            validate_structured_chunk_windows(&treatment.blocks, &treatment.chunks)?;
            ensure!(
                treatment.blocks.iter().map(|b| b.block_id).collect::<HashSet<_>>().len()
                    == treatment.blocks.len(),
                "duplicate support block identity"
            );
            for chunk in &treatment.chunks {
                ensure!(
                    chunk.literal_digest.as_deref()
                        == Some(
                            format!("sha256:{}", digest(chunk.normalized_text.as_bytes())).as_str()
                        ),
                    "chunk normalized digest mismatch"
                );
                ensure!(chunk.quality_score.is_finite(), "nonfinite chunk quality");
                ensure!(
                    chunk.support_block_ids.iter().collect::<HashSet<_>>().len()
                        == chunk.support_block_ids.len(),
                    "duplicate support reference"
                );
            }
            groups
                .entry((&treatment.treatment_id, &treatment.profile_id))
                .or_default()
                .push(CorpusDocument { case, treatment });
        }
    }
    let mut questions = HashSet::new();
    for query in &annotations.cases {
        ensure!(
            !query.id.trim().is_empty()
                && questions.insert(&query.id)
                && cases.contains(&query.document_id)
                && !query.question.trim().is_empty()
                && query.question.len() <= 16 * 1024
                && !query.expected_facts.is_empty()
                && query.expected_facts.iter().all(|fact| !fact.trim().is_empty()),
            "invalid/duplicate query or missing evidence checklist"
        );
    }
    ensure!((1..=64).contains(&groups.len()), "invalid arm count");
    let mut arms = Vec::new();
    for ((treatment_id, profile_id), documents) in &groups {
        let pdf_only = documents.len() != export.cases.len();
        let mut documents = documents.clone();
        if pdf_only {
            ensure!(
                documents.iter().all(|doc| doc.case.category == "pdf_tables"),
                "partial arm is not PDF-only"
            );
            let distractors = groups
                .get(&("structural", *profile_id))
                .context("PDF-only arm requires same-profile structural distractors")?;
            for case in &export.cases {
                if !documents.iter().any(|doc| doc.case.case_id == case.case_id) {
                    let distractor = distractors
                        .iter()
                        .find(|doc| doc.case.case_id == case.case_id)
                        .context("missing distractor document")?;
                    documents.push(*distractor);
                }
            }
        }
        ensure!(documents.len() == 4, "arm omits corpus distractors");
        let first = documents[0].treatment;
        ensure!(
            documents.iter().all(|doc| doc.treatment.max_chars == first.max_chars
                && doc.treatment.overlap_chars == first.overlap_chars),
            "inconsistent arm profile"
        );
        ensure!(
            documents.iter().map(|doc| doc.treatment.chunks.len()).sum::<usize>() <= 4096,
            "arm exceeds 4096 chunks"
        );
        arms.push(Arm { treatment_id, profile_id, pdf_only, documents });
    }
    Ok(arms)
}

fn record(output: &mut File, event: Value) -> Result<()> {
    serde_json::to_writer(&mut *output, &event)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn select_arm<'a>(mut arms: Vec<Arm<'a>>, selector: Option<&str>) -> Result<Vec<Arm<'a>>> {
    if let Some(selector) = selector {
        arms.retain(|arm| format!("{}/{}", arm.treatment_id, arm.profile_id) == selector);
        ensure!(
            arms.len() == 1,
            "selected replay arm must identify exactly one pinned treatment/profile"
        );
    }
    Ok(arms)
}

async fn fresh_pool(target: &url::Url, output: &mut File) -> Result<PgPool> {
    let database = target.path().trim_start_matches('/');
    // SQLx's default options otherwise consume PG* environment and .pgpass.
    // Reject ambient connection settings without reading or printing their values.
    ensure!(
        !std::env::vars_os().any(|(key, _)| key.to_string_lossy().starts_with("PG")),
        "unset ambient PG* variables for the explicit isolated replay"
    );
    let connection = |name: &str| {
        PgConnectOptions::new_without_pgpass()
            .host("127.0.0.1")
            .port(target.port().expect("DSN guard requires explicit port"))
            .username("postgres")
            .password("")
            .database(name)
            .ssl_mode(PgSslMode::Disable)
            .application_name("irg256-native-retrieval-replay")
    };
    let options =
        || PgPoolOptions::new().max_connections(1).acquire_timeout(Duration::from_secs(15));
    let admin_pool = options().connect_with(connection("postgres")).await?;
    let preexisting: bool =
        sqlx::query_scalar("select exists(select 1 from pg_database where datname=$1)")
            .bind(database)
            .fetch_one(&admin_pool)
            .await?;
    record(
        output,
        json!({"event":"database_guard","database":database,"preexisting":preexisting}),
    )?;
    require_fresh_database(preexisting, 0)?;
    // The identifier has already been restricted to ASCII alphanumeric/underscore.
    // CREATE DATABASE remains fail-closed on a concurrent creator; never reuse it.
    let create = format!("CREATE DATABASE \"{database}\" TEMPLATE template0");
    sqlx::query(sqlx::AssertSqlSafe(create.as_str())).execute(&admin_pool).await?;
    admin_pool.close().await;
    let pool = options().connect_with(connection(database)).await?;
    let objects: i64 = sqlx::query_scalar(
        "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname not in ('pg_catalog','information_schema') and n.nspname not like 'pg_toast%' and n.nspname not like 'pg_temp%'"
    ).fetch_one(&pool).await?;
    record(output, json!({"event":"empty_database_guard","userObjects":objects}))?;
    require_fresh_database(false, objects)?;
    Ok(pool)
}

async fn seed_document(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    seed: DocumentSeed<'_>,
) -> Result<()> {
    let DocumentSeed {
        workspace,
        library,
        document,
        revision,
        corpus: CorpusDocument { case, treatment },
    } = seed;
    sqlx::query("insert into content_document(id,workspace_id,library_id,external_key,revision_sequence) values($1,$2,$3,$4,1)")
        .bind(document).bind(workspace).bind(library).bind(&case.case_id).execute(&mut **tx).await?;
    // Titles/file names are opaque identities, never annotation labels or answers.
    let opaque_title = document.to_string();
    sqlx::query("insert into knowledge_document(document_id,workspace_id,library_id,external_key,file_name,title,document_state,active_revision_id,readable_revision_id,latest_revision_no,created_at,updated_at) values($1,$2,$3,$4,$6,$6,'active',$5,$5,1,now(),now())")
        .bind(document).bind(workspace).bind(library).bind(&case.case_id).bind(revision).bind(&opaque_title).execute(&mut **tx).await?;
    let checksum = format!("sha256:{}", case.normalized_sha256);
    sqlx::query("insert into knowledge_revision(revision_id,workspace_id,library_id,document_id,revision_number,revision_state,revision_kind,mime_type,checksum,title,byte_size,normalized_text,text_state,vector_state,graph_state,text_readable_at,created_at) values($1,$2,$3,$4,1,'ready','upload','text/plain',$5,$6,$7,$8,'text_readable','accepted','accepted',now(),now())")
        .bind(revision).bind(workspace).bind(library).bind(document).bind(&checksum).bind(&opaque_title)
        .bind(case.normalized_text.len() as i64).bind(&case.normalized_text).execute(&mut **tx).await?;
    let admission = json!({"origin":"mutation","sourceIdentity":null,"materializationToken":null,
        "visibilityIntentId":null,"replacement":{"checksum":checksum,"byteSize":case.normalized_text.len(),
            "mimeType":"text/plain","fileName":opaque_title,"title":opaque_title,
            "languageCode":null,"sourceUri":null,"documentHint":null}});
    sqlx::query("insert into content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,title,admission_identity) values($1,$2,$3,$4,1,'upload',$5,'text/plain',$6,$7,$8)")
        .bind(revision).bind(document).bind(workspace).bind(library).bind(&checksum)
        .bind(case.normalized_text.len() as i64).bind(&opaque_title).bind(admission).execute(&mut **tx).await?;
    sqlx::query("insert into content_document_head(document_id,active_revision_id,readable_revision_id) values($1,$2,$2)")
        .bind(document).bind(revision).execute(&mut **tx).await?;
    sqlx::query("insert into knowledge_structured_revision(revision_id,workspace_id,library_id,document_id,preparation_state,normalization_profile,source_format,block_count,chunk_count,typed_fact_count,outline_json,prepared_at,updated_at) values($1,$2,$3,$4,'prepared',$5,'text',$6,$7,$8,'[]',now(),now())")
        .bind(revision).bind(workspace).bind(library).bind(document).bind(&treatment.profile_id)
        .bind(treatment.blocks.len() as i64).bind(treatment.chunks.len() as i64).bind(case.technical_facts.len() as i64).execute(&mut **tx).await?;
    for block in &treatment.blocks {
        let id = identity(&[&revision.to_string(), "block", &block.block_id.to_string()]);
        sqlx::query("insert into knowledge_structured_block(block_id,workspace_id,library_id,document_id,revision_id,ordinal,block_kind,text,normalized_text,heading_trail,section_path,page_number,span_start,span_end,parent_block_id,table_coordinates_json,code_language,created_at,updated_at) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,now(),now())")
            .bind(id).bind(workspace).bind(library).bind(document).bind(revision).bind(block.ordinal)
            .bind(block.block_kind.as_str()).bind(&block.text).bind(&block.normalized_text)
            .bind(&block.heading_trail).bind(&block.section_path).bind(block.page_number)
            .bind(block.source_span.as_ref().map(|s| s.start_offset)).bind(block.source_span.as_ref().map(|s| s.end_offset))
            .bind(None::<Uuid>).bind(block.table_coordinates.as_ref().map(serde_json::to_value).transpose()?)
            .bind(&block.code_language).execute(&mut **tx).await?;
    }
    // Canonical validation permits forward parent references. Populate them only
    // after all blocks exist, still inside the single unpublished transaction.
    for block in &treatment.blocks {
        if let Some(parent) = block.parent_block_id {
            sqlx::query(
                "update knowledge_structured_block set parent_block_id=$1 where block_id=$2",
            )
            .bind(identity(&[&revision.to_string(), "block", &parent.to_string()]))
            .bind(identity(&[&revision.to_string(), "block", &block.block_id.to_string()]))
            .execute(&mut **tx)
            .await?;
        }
    }
    for chunk in &treatment.chunks {
        let id = chunk_id(revision, chunk);
        let supports: Vec<_> = chunk
            .support_block_ids
            .iter()
            .map(|block| identity(&[&revision.to_string(), "block", &block.to_string()]))
            .collect();
        sqlx::query("insert into knowledge_chunk(chunk_id,workspace_id,library_id,document_id,revision_id,chunk_index,chunk_kind,content_text,normalized_text,support_block_ids,section_path,heading_trail,literal_digest,chunk_state,quality_score,window_text,token_count) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,'ready',$14,$15,$16)")
            .bind(id).bind(workspace).bind(library).bind(document).bind(revision).bind(chunk.chunk_index)
            .bind(chunk.chunk_kind.as_str()).bind(&chunk.content_text).bind(&chunk.normalized_text)
            .bind(supports).bind(&chunk.section_path).bind(&chunk.heading_trail).bind(&chunk.literal_digest)
            .bind(chunk.quality_score).bind(&chunk.window_text).bind(chunk.token_count).execute(&mut **tx).await?;
    }
    seed_technical_facts(tx, seed).await
}

async fn seed_technical_facts(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    seed: DocumentSeed<'_>,
) -> Result<()> {
    // The production support owner replaces a revision (DELETE then INSERT).
    // This fresh-only seed must never replace a scope, so bind its canonical
    // columns directly inside the existing unpublished transaction instead.
    for original in &seed.corpus.case.technical_facts {
        let fact = remap_fact(seed, original);
        let value = fact.normalized_value.normalized_string();
        let exact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
        sqlx::query("insert into knowledge_technical_fact(fact_id,workspace_id,library_id,document_id,revision_id,fact_kind,canonical_value_text,canonical_value_exact,canonical_value_json,display_value,qualifiers_json,support_block_ids,support_chunk_ids,confidence,extraction_kind,conflict_group_id,created_at,updated_at) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$17)")
            .bind(fact.fact_id).bind(fact.workspace_id).bind(fact.library_id).bind(fact.document_id).bind(fact.revision_id)
            .bind(fact.fact_kind.as_str()).bind(value).bind(exact).bind(serde_json::to_value(&fact.normalized_value)?)
            .bind(&fact.display_value).bind(serde_json::to_value(&fact.qualifiers)?)
            .bind(&fact.support_block_ids).bind(&fact.support_chunk_ids).bind(fact.confidence)
            .bind(&fact.extraction_kind).bind(&fact.conflict_group_id).bind(fact.created_at)
            .execute(&mut **tx).await?;
    }
    Ok(())
}

fn chunk_id(revision: Uuid, chunk: &StructuredChunkWindow) -> Uuid {
    identity(&[
        &revision.to_string(),
        "chunk",
        &chunk.chunk_index.to_string(),
        &digest(chunk.normalized_text.as_bytes()),
    ])
}

fn validate_derived_identities(export: &Export, arms: &[Arm<'_>], export_pin: &str) -> Result<()> {
    let mut ids = HashSet::new();
    ensure!(
        ids.insert(identity(&[&export.source_sha, export_pin, "workspace"])),
        "workspace UUID collision"
    );
    for arm in arms {
        let library = identity(&[
            &export.source_sha,
            export_pin,
            &export.input_manifest_sha256,
            arm.treatment_id,
            arm.profile_id,
            "library",
        ]);
        ensure!(ids.insert(library), "library UUID collision");
        for CorpusDocument { case, treatment } in &arm.documents {
            let document =
                identity(&[&library.to_string(), &case.case_id, &case.source_sha256, "document"]);
            let revision = identity(&[&document.to_string(), &case.normalized_sha256, "revision"]);
            ensure!(
                ids.insert(document) && ids.insert(revision),
                "document/revision UUID collision"
            );
            for block in &treatment.blocks {
                ensure!(
                    ids.insert(identity(&[
                        &revision.to_string(),
                        "block",
                        &block.block_id.to_string()
                    ])),
                    "block UUID collision"
                );
            }
            for chunk in &treatment.chunks {
                ensure!(ids.insert(chunk_id(revision, chunk)), "chunk UUID collision");
            }
            for fact in &case.technical_facts {
                ensure!(
                    ids.insert(identity(&[
                        &revision.to_string(),
                        "fact",
                        &fact.fact_id.to_string()
                    ])),
                    "fact UUID collision"
                );
            }
        }
    }
    Ok(())
}

async fn prepare_database(target: &url::Url, output: &mut File) -> Result<PgPool> {
    let pool = fresh_pool(target, output).await?;
    let started = Instant::now();
    sqlx::migrate!("./migrations").run(&pool).await?;
    record(
        output,
        json!({"event":"migrations","elapsedMs":started.elapsed().as_secs_f64()*1000.0,
        "indexTimingScope":"ordinary migration includes empty-index creation; seed timing includes generated FTS and incremental index maintenance"}),
    )?;
    let settings: Value = sqlx::query_scalar("select jsonb_object_agg(name,setting) from pg_settings where name in ('server_version','server_version_num','default_text_search_config','server_encoding','lc_ctype','lc_collate','shared_buffers','work_mem','maintenance_work_mem','effective_cache_size','random_page_cost','jit','max_parallel_workers_per_gather')")
        .fetch_one(&pool).await?;
    let version: String = sqlx::query_scalar("select version()").fetch_one(&pool).await?;
    let locales: Value = sqlx::query_scalar("select jsonb_build_object('collation',datcollate,'ctype',datctype) from pg_database where datname=current_database()")
        .fetch_one(&pool).await?;
    let extensions: Value =
        sqlx::query_scalar("select jsonb_object_agg(extname,extversion) from pg_extension")
            .fetch_one(&pool)
            .await?;
    let migrations: Value = sqlx::query_scalar("select jsonb_agg(jsonb_build_object('version',version,'checksumHex',encode(checksum,'hex'),'success',success) order by version) from _sqlx_migrations")
        .fetch_one(&pool).await?;
    record(
        output,
        json!({"event":"postgres","version":version,"settings":settings,"databaseLocale":locales,
        "extensions":extensions,"migrations":migrations,
        "configurationScope":"listed effective planner/memory/locale settings and exact migrated schema; not a dump of secret-bearing server settings",
        "generatedSearchConfig":"simple","queryTextSearchConfig":null,"queryOrigin":"UserQuery",
        "documentLanguageCode":null,"languageScope":"pinned questions unchanged; no query translation"}),
    )?;
    Ok(pool)
}

async fn seed_arms<'a>(
    pool: &PgPool,
    export: &Export,
    arms: &[Arm<'a>],
    export_pin: &str,
    output: &mut File,
) -> Result<SeededCorpus<'a>> {
    let workspace = identity(&[&export.source_sha, export_pin, "workspace"]);
    let mut hit_map = HashMap::new();
    let mut document_map = HashMap::new();
    let mut libraries = Vec::new();
    let mut tx = pool.begin().await?;
    sqlx::query("insert into catalog_workspace(id,slug,display_name) values($1,'irg256','IRG-256 disposable replay')")
        .bind(workspace).execute(&mut *tx).await?;
    for (arm_index, arm) in arms.iter().enumerate() {
        let library = identity(&[
            &export.source_sha,
            export_pin,
            &export.input_manifest_sha256,
            arm.treatment_id,
            arm.profile_id,
            "library",
        ]);
        libraries.push(library);
        let started = Instant::now();
        sqlx::query(
            "insert into catalog_library(id,workspace_id,slug,display_name) values($1,$2,$3,$3)",
        )
        .bind(library)
        .bind(workspace)
        .bind(format!("arm-{arm_index}"))
        .execute(&mut *tx)
        .await?;
        let mut documents = Vec::new();
        for &corpus in &arm.documents {
            let CorpusDocument { case, treatment } = corpus;
            let document =
                identity(&[&library.to_string(), &case.case_id, &case.source_sha256, "document"]);
            let revision = identity(&[&document.to_string(), &case.normalized_sha256, "revision"]);
            let seed = DocumentSeed { workspace, library, document, revision, corpus };
            seed_document(&mut tx, seed).await?;
            ensure!(
                document_map.insert(document, seed).is_none(),
                "derived document UUID collision"
            );
            for chunk in &treatment.chunks {
                let id = chunk_id(revision, chunk);
                ensure!(
                    hit_map.insert(id, IndexedChunk { seed, chunk }).is_none(),
                    "derived chunk UUID collision"
                );
            }
            documents.push(json!({"caseId":case.case_id,"documentId":document,"revisionId":revision,
                "category":case.category,"sourceSha256":case.source_sha256,"normalizedSha256":case.normalized_sha256,
                "sourceTreatmentId":treatment.treatment_id,"profileId":treatment.profile_id,
                "blockCount":treatment.blocks.len(),"chunkCount":treatment.chunks.len(),"preparationMs":treatment.preparation_ms}));
        }
        record(
            output,
            json!({"event":"armSeed","armIndex":arm_index,"treatmentId":arm.treatment_id,"profileId":arm.profile_id,
            "libraryId":library,"elapsedMs":started.elapsed().as_secs_f64()*1000.0,
            "timingScope":"uncommitted inserts, generated FTS and shared-index maintenance; commit measured separately",
            "comparisonScope":if arm.pdf_only {"PDF-only treatment with same-profile structural non-PDF distractors; not a four-document global treatment"} else {"four-document global treatment"},
            "documents":documents}),
        )?;
    }
    let started = Instant::now();
    tx.commit().await?;
    record(
        output,
        json!({"event":"seedCommit","elapsedMs":started.elapsed().as_secs_f64()*1000.0}),
    )?;
    Ok(SeededCorpus { libraries, hit_map, documents: document_map })
}

async fn observe_corpus(
    pool: &PgPool,
    arms: &[Arm<'_>],
    seeded: &SeededCorpus<'_>,
    output: &mut File,
) -> Result<()> {
    let SeededCorpus { libraries, hit_map, documents } = seeded;
    let published: i64 =
        sqlx::query_scalar("select count(*) from content_current_revision").fetch_one(pool).await?;
    let chunks: i64 =
        sqlx::query_scalar("select count(*) from knowledge_chunk").fetch_one(pool).await?;
    let mismatches: i64 = sqlx::query_scalar("select count(*) from knowledge_structured_revision r where r.chunk_count != (select count(*) from knowledge_chunk c where c.revision_id=r.revision_id) or r.block_count != (select count(*) from knowledge_structured_block b where b.revision_id=r.revision_id) or r.typed_fact_count != (select count(*) from knowledge_technical_fact f where f.revision_id=r.revision_id)")
        .fetch_one(pool).await?;
    let facts: i64 =
        sqlx::query_scalar("select count(*) from knowledge_technical_fact").fetch_one(pool).await?;
    let expected_facts: usize =
        documents.values().map(|seed| seed.corpus.case.technical_facts.len()).sum();
    let invalid_fact_scopes: i64 = sqlx::query_scalar("select count(*) from knowledge_technical_fact f left join content_current_revision r on r.revision_id=f.revision_id and r.document_id=f.document_id and r.library_id=f.library_id and r.workspace_id=f.workspace_id where r.revision_id is null or exists(select 1 from unnest(f.support_block_ids) id where not exists(select 1 from knowledge_structured_block b where b.block_id=id and b.revision_id=f.revision_id and b.document_id=f.document_id and b.library_id=f.library_id and b.workspace_id=f.workspace_id)) or exists(select 1 from unnest(f.support_chunk_ids) id where not exists(select 1 from knowledge_chunk c where c.chunk_id=id and c.revision_id=f.revision_id and c.document_id=f.document_id and c.library_id=f.library_id and c.workspace_id=f.workspace_id))")
        .fetch_one(pool).await?;
    ensure!(
        facts == expected_facts as i64 && invalid_fact_scopes == 0,
        "persisted fact counts/source scopes mismatch"
    );
    for seed in documents.values() {
        let counts: (i64, i64, i64) = sqlx::query_as("select (select count(*) from knowledge_structured_block where revision_id=$1), (select count(*) from knowledge_chunk where revision_id=$1), (select count(*) from knowledge_technical_fact where revision_id=$1)")
            .bind(seed.revision).fetch_one(pool).await?;
        ensure!(
            counts
                == (
                    seed.corpus.treatment.blocks.len() as i64,
                    seed.corpus.treatment.chunks.len() as i64,
                    seed.corpus.case.technical_facts.len() as i64
                ),
            "persisted per-revision counts mismatch"
        );
    }
    record(
        output,
        json!({"event":"sourceCounts","technicalFacts":facts,"factScopeMismatches":invalid_fact_scopes,"perRevisionCountsVerified":true}),
    )?;
    ensure!(
        published == (arms.len() * 4) as i64 && chunks == hit_map.len() as i64 && mismatches == 0,
        "publication or structured counts mismatch"
    );
    let started = Instant::now();
    sqlx::raw_sql("analyze knowledge_chunk; analyze knowledge_document; analyze knowledge_revision; analyze content_document; analyze content_revision; analyze content_document_head; analyze knowledge_structured_block; analyze knowledge_technical_fact;").execute(pool).await?;
    record(
        output,
        json!({"event":"published","documents":published,"chunks":chunks,"structuredCountMismatches":mismatches,"analyzeMs":started.elapsed().as_secs_f64()*1000.0}),
    )?;
    let physical: Value = sqlx::query_scalar("select jsonb_agg(jsonb_build_object('relation',relname,'totalBytes',pg_total_relation_size(relid),'indexBytes',pg_indexes_size(relid))) from pg_stat_user_tables where schemaname='public'")
        .fetch_one(pool).await?;
    record(
        output,
        json!({"event":"physicalStorage","scope":if arms.len()==1 {
            "fresh database containing exactly one treatment arm and all four corpus documents; actual physical table/index allocation"
        } else { "full experiment, all libraries; shared indexes are NOT attributable per arm" },
        "isolatedArm":arms.len()==1,"relations":physical}),
    )?;
    for (index, library) in libraries.iter().enumerate() {
        let rows: Value = sqlx::query_scalar("select jsonb_build_object('chunkRows',count(*),'chunkRowDatumBytes',coalesce(sum(pg_column_size(c)),0)) from knowledge_chunk c where library_id=$1")
            .bind(library).fetch_one(pool).await?;
        record(
            output,
            json!({"event":"armStorage","armIndex":index,"libraryId":library,"rows":rows,
            "scope":"pg_column_size row datums, not disk/index allocation or provider tokens"}),
        )?;
        let sources: Value = sqlx::query_scalar("select jsonb_build_object('blockRows',(select count(*) from knowledge_structured_block where library_id=$1),'blockRowDatumBytes',(select coalesce(sum(pg_column_size(b)),0) from knowledge_structured_block b where library_id=$1),'technicalFactRows',(select count(*) from knowledge_technical_fact where library_id=$1),'technicalFactRowDatumBytes',(select coalesce(sum(pg_column_size(f)),0) from knowledge_technical_fact f where library_id=$1))")
            .bind(library).fetch_one(pool).await?;
        record(
            output,
            json!({"event":"armSourceStorage","armIndex":index,"libraryId":library,"rows":sources,"scope":"pg_column_size row datums, not disk/index allocation or provider tokens"}),
        )?;
    }
    Ok(())
}

struct SearchReplay<'a, 'corpus> {
    store: PgSearchStore,
    export: &'a Export,
    seeded: &'a SeededCorpus<'corpus>,
    lane: RetrievalLane,
}

impl SearchReplay<'_, '_> {
    async fn query(
        &self,
        arm_index: usize,
        arm: &Arm<'_>,
        library: &Uuid,
        question: &Question,
        output: &mut File,
    ) -> Result<()> {
        for call_index in 0..4 {
            let started = Instant::now();
            let result = match self.lane {
                RetrievalLane::Chunks => self
                    .store
                    .search_chunks(ChunkLexicalQuery {
                        library_id: *library,
                        query: &question.question,
                        query_origin: LexicalQueryOrigin::UserQuery,
                        limit: 10,
                        temporal_start: None,
                        temporal_end: None,
                        text_search_config: None,
                    })
                    .await
                    .map(NativeHits::Chunks),
                RetrievalLane::StructuredBlocks => self
                    .store
                    .search_structured_blocks(*library, &question.question, 10)
                    .await
                    .map(NativeHits::StructuredBlocks),
                RetrievalLane::TechnicalFacts => self
                    .store
                    .search_technical_facts(*library, &question.question, 10)
                    .await
                    .map(NativeHits::TechnicalFacts),
            };
            let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
            let hits = match result {
                Ok(hits) => hits,
                Err(error) => {
                    record(
                        output,
                        json!({"event":"callFailure","armIndex":arm_index,"queryId":question.id,
                            "callIndex":call_index,"latencyMs":latency_ms,"error":error.to_string()}),
                    )?;
                    return Err(error);
                }
            };
            // The common timer covers only the actual native search call in
            // every lane; bounded evidence hydration is deliberately outside it.
            let observations = match hits {
                NativeHits::Chunks(hits) => {
                    ensure!(hits.len() <= 10, "production store exceeded requested limit");
                    self.observations(library, &hits)?
                }
                NativeHits::StructuredBlocks(hits) => {
                    ensure!(hits.len() <= 10, "production store exceeded requested limit");
                    self.block_observations(*library, &hits).await?
                }
                NativeHits::TechnicalFacts(hits) => {
                    ensure!(hits.len() <= 10, "production store exceeded requested limit");
                    self.fact_observations(*library, &hits).await?
                }
            };
            record(
                output,
                json!({"event":"call","armIndex":arm_index,"treatmentId":arm.treatment_id,"profileId":arm.profile_id,
                    "queryId":question.id,"query":question.question,"targetCaseId":question.document_id,
                    "targetInTreatmentScope":!arm.pdf_only || self.export.cases.iter().any(|c|c.case_id==question.document_id && c.category=="pdf_tables"),
                    "callIndex":call_index,"phase":if call_index==0 {"warmup"} else {"measured"},
                    "limit":10,"metricCutoffs":[5,10],"latencyMs":latency_ms,"hits":observations,
                    "contextScope":if self.lane == RetrievalLane::Chunks {"retrieved chunk/window character and whitespace-unit counts, not assembled answer context or billing tokens"} else {"native search hits plus bounded persisted source evidence; availability only, not semantic truth, condition fidelity or answer correctness; source anchors do not validate generated claims"}}),
            )?;
        }
        Ok(())
    }

    fn source_seed(
        &self,
        workspace: Uuid,
        library: Uuid,
        document: Uuid,
        revision: Uuid,
        requested_library: Uuid,
    ) -> Result<DocumentSeed<'_>> {
        let seed =
            *self.seeded.documents.get(&document).context("unknown native source document")?;
        ensure!(
            library == requested_library
                && seed.workspace == workspace
                && seed.library == library
                && seed.document == document
                && seed.revision == revision,
            "native hit escaped source scope"
        );
        Ok(seed)
    }

    async fn hydrate_supports(&self, seed: DocumentSeed<'_>, ids: &[Uuid]) -> Result<Value> {
        let selected: Vec<_> = ids.iter().take(SUPPORT_LIMIT).copied().collect();
        let rows: Vec<(Uuid, Value)> = sqlx::query_as("select b.block_id, to_jsonb(b) from knowledge_structured_block b join content_current_revision r on r.revision_id=b.revision_id and r.document_id=b.document_id and r.library_id=b.library_id and r.workspace_id=b.workspace_id where b.workspace_id=$1 and b.library_id=$2 and b.document_id=$3 and b.revision_id=$4 and b.block_id=any($5)")
            .bind(seed.workspace).bind(seed.library).bind(seed.document).bind(seed.revision).bind(&selected).fetch_all(&self.store.pool).await?;
        let rows: HashMap<_, _> = rows.into_iter().collect();
        ensure!(rows.len() == selected.len(), "unresolved persisted source supports");
        let mut supports = Vec::new();
        for id in &selected {
            let original = seed
                .corpus
                .case
                .blocks
                .iter()
                .find(|block| {
                    identity(&[&seed.revision.to_string(), "block", &block.block_id.to_string()])
                        == *id
                })
                .context("native support has no original canonical source block")?;
            let persisted = rows.get(id).context("missing persisted support")?;
            ensure!(
                persisted["text"].as_str() == Some(original.text.as_str())
                    && persisted["normalized_text"].as_str()
                        == Some(original.normalized_text.as_str()),
                "persisted source support text changed"
            );
            supports.push(json!({"originalBlockId":original.block_id,"block":persisted}));
        }
        let payload = json!({"blocks":supports,"totalSupportBlocks":ids.len(),"returnedSupportBlocks":selected.len(),"supportLimit":SUPPORT_LIMIT,"truncated":ids.len()>SUPPORT_LIMIT,"payloadByteLimit":SUPPORT_PAYLOAD_CAP,"textClipped":false});
        ensure!(
            serde_json::to_vec(&payload)?.len() <= SUPPORT_PAYLOAD_CAP,
            "source hydration exceeds 1 MiB per hit; refusing without clipping source text"
        );
        Ok(payload)
    }

    fn source_identity(seed: DocumentSeed<'_>) -> Value {
        json!({"sourceCase":seed.corpus.case.case_id,"normalizedSourceSha256":seed.corpus.case.normalized_sha256,
            "sourceSha256":seed.corpus.case.source_sha256,"sourceTreatmentId":seed.corpus.treatment.treatment_id,"profileId":seed.corpus.treatment.profile_id})
    }

    async fn block_observations(
        &self,
        library: Uuid,
        hits: &[KnowledgeStructuredBlockSearchRow],
    ) -> Result<Vec<Value>> {
        let mut observations = Vec::new();
        for (rank, hit) in hits.iter().enumerate() {
            let seed = self.source_seed(
                hit.workspace_id,
                hit.library_id,
                hit.document_id,
                hit.revision_id,
                library,
            )?;
            ensure!(hit.score.is_finite(), "nonfinite native score");
            let source = self.hydrate_supports(seed, &[hit.block_id]).await?;
            ensure!(
                source["blocks"][0]["block"]["text"].as_str() == Some(hit.text.as_str())
                    && source["blocks"][0]["block"]["normalized_text"].as_str()
                        == Some(hit.normalized_text.as_str()),
                "native block hit text changed"
            );
            let observation = json!({"rank":rank+1,"nativeHit":hit,"sourceIdentity":Self::source_identity(seed),"sourceEvidence":source});
            ensure!(
                serde_json::to_vec(&observation)?.len() <= SUPPORT_PAYLOAD_CAP,
                "block/source payload exceeds 1 MiB per hit; refusing without clipping source text"
            );
            observations.push(observation);
        }
        Ok(observations)
    }

    async fn fact_observations(
        &self,
        library: Uuid,
        hits: &[KnowledgeTechnicalFactSearchRow],
    ) -> Result<Vec<Value>> {
        let mut observations = Vec::new();
        for (rank, hit) in hits.iter().enumerate() {
            let seed = self.source_seed(
                hit.workspace_id,
                hit.library_id,
                hit.document_id,
                hit.revision_id,
                library,
            )?;
            ensure!(hit.score.is_finite(), "nonfinite native score");
            let original = seed
                .corpus
                .case
                .technical_facts
                .iter()
                .find(|fact| remap_fact(seed, fact).fact_id == hit.fact_id)
                .context("unknown native fact hit")?;
            let mapped = remap_fact(seed, original);
            let (fact, supports): (Value, Vec<Uuid>) = sqlx::query_as("select to_jsonb(f), f.support_block_ids from knowledge_technical_fact f join content_current_revision r on r.revision_id=f.revision_id and r.document_id=f.document_id and r.library_id=f.library_id and r.workspace_id=f.workspace_id where f.fact_id=$1 and f.workspace_id=$2 and f.library_id=$3 and f.document_id=$4 and f.revision_id=$5")
                .bind(hit.fact_id).bind(seed.workspace).bind(seed.library).bind(seed.document).bind(seed.revision).fetch_one(&self.store.pool).await?;
            ensure!(
                supports == mapped.support_block_ids
                    && fact["qualifiers_json"] == serde_json::to_value(&mapped.qualifiers)?
                    && fact["canonical_value_json"]
                        == serde_json::to_value(&mapped.normalized_value)?
                    && hit.normalized_value_text == mapped.normalized_value.normalized_string()
                    && hit.display_value == mapped.display_value
                    && hit.fact_kind == mapped.fact_kind.as_str(),
                "persisted fact evidence changed"
            );
            let source = self.hydrate_supports(seed, &supports).await?;
            let observation = json!({"rank":rank+1,"nativeHit":hit,"originalFactId":original.fact_id,"persistedFact":fact,"sourceIdentity":Self::source_identity(seed),"sourceEvidence":source});
            ensure!(
                serde_json::to_vec(&observation)?.len() <= SUPPORT_PAYLOAD_CAP,
                "fact/source payload exceeds 1 MiB per hit; refusing without clipping qualifiers or source text"
            );
            observations.push(observation);
        }
        Ok(observations)
    }

    fn observations(&self, library: &Uuid, hits: &[KnowledgeChunkSearchRow]) -> Result<Vec<Value>> {
        let mut observations = Vec::new();
        for (rank, hit) in hits.iter().enumerate() {
            let IndexedChunk { seed, chunk } =
                self.seeded.hit_map.get(&hit.chunk_id).context("unknown search hit")?;
            let DocumentSeed {
                library: owner,
                document,
                revision,
                corpus: CorpusDocument { case, treatment },
                ..
            } = seed;
            ensure!(
                owner == library && hit.library_id == *library && hit.revision_id == *revision,
                "hit escaped arm/revision scope"
            );
            ensure!(
                hit.content_text == chunk.content_text
                    && hit.normalized_text == chunk.normalized_text,
                "stored chunk text changed"
            );
            ensure!(hit.score.is_finite(), "nonfinite native score");
            observations.push(json!({"rank":rank+1,"chunkId":hit.chunk_id,"documentId":document,"revisionId":revision,
                "caseId":case.case_id,"sourceTreatmentId":treatment.treatment_id,"profileId":treatment.profile_id,
                "chunkIndex":chunk.chunk_index,"literalDigest":chunk.literal_digest,"score":hit.score,
                "contentSha256":digest(chunk.content_text.as_bytes()),"normalizedSha256":digest(chunk.normalized_text.as_bytes()),
                "normalizedChars":chunk.normalized_text.chars().count(),"normalizedWhitespaceUnits":chunk.normalized_text.split_whitespace().count(),
                "windowChars":chunk.window_text.as_ref().map(|s|s.chars().count()),
                "windowWhitespaceUnits":chunk.window_text.as_ref().map(|s|s.split_whitespace().count())}));
        }
        Ok(observations)
    }
}

struct ReplayTarget<'a> {
    endpoint: &'a url::Url,
    lane: RetrievalLane,
}

async fn replay(
    target: ReplayTarget<'_>,
    export: &Export,
    annotations: &Annotations,
    arms: &[Arm<'_>],
    export_pin: &str,
    output: &mut File,
) -> Result<()> {
    let pool = prepare_database(target.endpoint, output).await?;
    let seeded = seed_arms(&pool, export, arms, export_pin, output).await?;
    observe_corpus(&pool, arms, &seeded, output).await?;
    let search = SearchReplay {
        store: PgSearchStore { pool: pool.clone() },
        export,
        seeded: &seeded,
        lane: target.lane,
    };
    for (arm_index, (arm, library)) in arms.iter().zip(&seeded.libraries).enumerate() {
        for question in &annotations.cases {
            search.query(arm_index, arm, library, question, output).await?;
        }
    }
    record(
        output,
        json!({"event":"complete","arms":arms.len(),"queries":annotations.cases.len(),
        "calls":arms.len()*annotations.cases.len()*4,"providerCalls":0,"answerQualityScored":false,
        "databaseRetained":true,"retryCount":0}),
    )?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires pinned treatment/annotation files, explicit fresh loopback DB and create-new output; no providers"]
async fn native_category_retrieval_replay() -> Result<()> {
    let lane = match std::env::var("ASSERTION_RETRIEVAL_LANE") {
        Ok(value) => RetrievalLane::parse(Some(&value))?,
        Err(std::env::VarError::NotPresent) => RetrievalLane::parse(None)?,
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("invalid non-Unicode ASSERTION_RETRIEVAL_LANE")
        }
    };
    let dsn = std::env::var("CHUNKING_RETRIEVAL_DSN").context("explicit replay DSN missing")?;
    let target = validate_dsn(&dsn)?;
    let treatment_path = std::env::var("CHUNKING_TREATMENT_EXPORT")?;
    let treatment_pin = std::env::var("CHUNKING_TREATMENT_EXPORT_SHA256")?;
    let annotation_path = std::env::var("CHUNKING_QUERY_ANNOTATIONS")?;
    let annotation_pin = std::env::var("CHUNKING_QUERY_ANNOTATIONS_SHA256")?;
    let export: Export =
        serde_json::from_slice(&pinned(Path::new(&treatment_path), &treatment_pin)?)?;
    let annotations: Annotations =
        serde_json::from_slice(&pinned(Path::new(&annotation_path), &annotation_pin)?)?;
    let arms = validate_inputs(&export, &annotations)?;
    validate_derived_identities(&export, &arms, &treatment_pin)?;
    let selected = std::env::var("CHUNKING_RETRIEVAL_ARM").ok();
    let arms = select_arm(arms, selected.as_deref())?;
    validate_lane_arms(lane, &arms)?;
    let output_path = std::env::var("CHUNKING_RETRIEVAL_EXPORT")?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(output_path)?;
    record(
        &mut output,
        json!({"event":"run","protocol":"irg256-native-retrieval-replay/1","encoding":"JSONL",
        "sourceSha":export.source_sha,"inputManifestSha256":export.input_manifest_sha256,
        "treatmentExportSha256":treatment_pin,"queryAnnotationsSha256":annotation_pin,
        "selectedArm":selected,
        "retrievalLane":lane.name(),
        "sourceHydration":{"maxHits":10,"maxBlocksPerHit":SUPPORT_LIMIT,"maxPayloadBytesPerHit":SUPPORT_PAYLOAD_CAP,"timerScope":"native search only; hydration outside timer","meaning":"evidence availability only; not truth or condition fidelity"},
        "guards":{"dsn":"explicit password-free postgres@127.0.0.1, non-5432 port, irg256 name, sslmode=disable only",
            "files":"bounded regular files; SHA256 pins verified before DB access",
            "corpus":"protocol, hashes, duplicate IDs, canonical blocks/supports, contiguous chunks, limits checked before DB access",
            "database":"must not exist; create once via same endpoint postgres DB; reject any user objects before ordinary migrations; reject PG* environment; never read .pgpass",
            "mutation":"no drop, truncate, reset, retries, existing database reuse or application DSN fallback"},
        "measurement":{"warmups":1,"repetitions":3,"limit":10,"cutoffs":[5,10],"providerCalls":0,
            "lane":match lane { RetrievalLane::Chunks => "actual PgSearchStore::search_chunks, UserQuery, no raw-query arm", RetrievalLane::StructuredBlocks => "actual PgSearchStore::search_structured_blocks", RetrievalLane::TechnicalFacts => "actual PgSearchStore::search_technical_facts"}}}),
    )?;
    let result = replay(
        ReplayTarget { endpoint: &target, lane },
        &export,
        &annotations,
        &arms,
        &treatment_pin,
        &mut output,
    )
    .await;
    if let Err(error) = &result {
        record(
            &mut output,
            json!({"event":"failure","error":error.to_string(),"databaseRetained":true,"retryCount":0}),
        )?;
    }
    result
}

#[test]
fn retrieval_lane_is_exact_and_defaults_without_database_access() {
    assert_eq!(RetrievalLane::parse(None).unwrap(), RetrievalLane::Chunks);
    assert_eq!(RetrievalLane::parse(Some("chunks")).unwrap(), RetrievalLane::Chunks);
    assert_eq!(
        RetrievalLane::parse(Some("structured_blocks")).unwrap(),
        RetrievalLane::StructuredBlocks
    );
    assert_eq!(
        RetrievalLane::parse(Some("technical_facts")).unwrap(),
        RetrievalLane::TechnicalFacts
    );
    for value in ["", "technical-facts", "technical_facts ", "Chunks", " chunks", "all"] {
        assert!(RetrievalLane::parse(Some(value)).is_err());
    }
}

fn fact_test_case() -> Result<Case> {
    let block = identity(&["fixture", "block"]);
    Ok(serde_json::from_value(json!({
        "caseId":"fixture", "category":"technical", "sourceSha256":"a".repeat(64),
        "normalizedSha256":digest(b"Use port 8080 only locally."),
        "normalizedText":"Use port 8080 only locally.",
        "blocks":[{"blockId":block,"ordinal":0,"blockKind":"paragraph",
            "text":"Use port 8080 only locally.","normalizedText":"Use port 8080 only locally.",
            "headingTrail":["Local setup"],"sectionPath":["Local setup"],"pageNumber":1,
            "sourceSpan":{"startOffset":0,"endOffset":26},"parentBlockId":null,
            "tableCoordinates":null,"codeLanguage":null,"isBoilerplate":false}],
        "technicalFacts":[{"factId":identity(&["fixture", "fact"]),
            "revisionId":identity(&["original", "revision"]),"documentId":identity(&["original", "document"]),
            "workspaceId":identity(&["original", "workspace"]),"libraryId":identity(&["original", "library"]),
            "factKind":"port","canonicalValue":{"value_type":"integer","value":8080},
            "displayValue":"8080","qualifiers":[{"key":"scope","value":"only locally"}],
            "supportBlockIds":[block],"supportChunkIds":[],"confidence":0.9,
            "extractionKind":"deterministic","conflictGroupId":null,"createdAt":"2026-10-26T00:00:00Z"}],
        "treatments":[]
    }))?)
}

#[test]
fn structured_lane_rejects_derived_blocks_before_database_access() -> Result<()> {
    let case = fact_test_case()?;
    let mut treatment = Treatment {
        treatment_id: "recursive".into(),
        profile_id: "test".into(),
        max_chars: 100,
        overlap_chars: 0,
        preparation_ms: 0.0,
        blocks: case.blocks.clone(),
        chunks: Vec::new(),
    };
    let check = |treatment: &Treatment, lane| {
        validate_lane_arms(
            lane,
            &[Arm {
                treatment_id: &treatment.treatment_id,
                profile_id: &treatment.profile_id,
                pdf_only: false,
                documents: vec![CorpusDocument { case: &case, treatment }],
            }],
        )
    };
    check(&treatment, RetrievalLane::StructuredBlocks)?;
    let mut derived = case.blocks[0].clone();
    derived.block_id = identity(&["derived fragment"]);
    derived.parent_block_id = Some(case.blocks[0].block_id);
    derived.ordinal = 1;
    treatment.blocks.push(derived);
    assert!(check(&treatment, RetrievalLane::StructuredBlocks).is_err());
    check(&treatment, RetrievalLane::Chunks)?;
    check(&treatment, RetrievalLane::TechnicalFacts)?;
    Ok(())
}

#[test]
fn fact_input_rejects_unresolved_duplicate_and_nonfinite_evidence() -> Result<()> {
    let mut case = fact_test_case()?;
    validate_facts(&case)?;
    let fact = case.technical_facts[0].clone();
    case.technical_facts.push(fact.clone());
    assert!(validate_facts(&case).is_err());
    case.technical_facts.truncate(1);
    case.technical_facts[0].support_block_ids.push(fact.support_block_ids[0]);
    assert!(validate_facts(&case).is_err());
    case.technical_facts[0] = fact.clone();
    case.technical_facts[0].support_block_ids[0] = identity(&["missing block"]);
    assert!(validate_facts(&case).is_err());
    case.technical_facts[0] = fact.clone();
    case.technical_facts[0].support_chunk_ids.push(identity(&["unresolvable original chunk"]));
    assert!(validate_facts(&case).is_err());
    for confidence in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        case.technical_facts[0] = fact.clone();
        case.technical_facts[0].confidence = Some(confidence);
        assert!(validate_facts(&case).is_err());
    }
    case.technical_facts[0] = fact.clone();
    let mut other = fact;
    other.fact_id = identity(&["second fact"]);
    other.document_id = identity(&["foreign document"]);
    case.technical_facts.push(other);
    assert!(validate_facts(&case).is_err());
    Ok(())
}

#[test]
fn fact_remapping_is_deterministic_scoped_and_preserves_conditions() -> Result<()> {
    let case = fact_test_case()?;
    let original = &case.technical_facts[0];
    let treatment = Treatment {
        treatment_id: "structural".into(),
        profile_id: "test".into(),
        max_chars: 100,
        overlap_chars: 0,
        preparation_ms: 0.0,
        blocks: case.blocks.clone(),
        chunks: vec![StructuredChunkWindow {
            chunk_index: 0,
            chunk_kind: case.blocks[0].block_kind,
            support_block_ids: original.support_block_ids.clone(),
            content_text: case.normalized_text.clone(),
            normalized_text: case.normalized_text.clone(),
            heading_trail: vec![],
            section_path: vec![],
            token_count: None,
            literal_digest: None,
            quality_score: 1.0,
            simhash_fingerprint: None,
            is_near_duplicate: false,
            window_text: None,
        }],
    };
    let seed = DocumentSeed {
        workspace: identity(&["fresh workspace"]),
        library: identity(&["fresh library"]),
        document: identity(&["fresh document"]),
        revision: identity(&["fresh revision"]),
        corpus: CorpusDocument { case: &case, treatment: &treatment },
    };
    let mapped = remap_fact(seed, original);
    assert_eq!(serde_json::to_value(&mapped)?, serde_json::to_value(remap_fact(seed, original))?);
    assert_eq!(
        (mapped.workspace_id, mapped.library_id, mapped.document_id, mapped.revision_id),
        (seed.workspace, seed.library, seed.document, seed.revision)
    );
    assert_ne!(mapped.fact_id, original.fact_id);
    assert_ne!(mapped.support_block_ids, original.support_block_ids);
    assert_eq!(mapped.support_chunk_ids, vec![chunk_id(seed.revision, &treatment.chunks[0])]);
    assert_original_fact_fields(&mapped, original);
    let other = remap_fact(
        DocumentSeed { revision: identity(&["another fresh revision"]), ..seed },
        original,
    );
    assert_ne!(mapped.fact_id, other.fact_id);
    assert_ne!(mapped.support_block_ids, other.support_block_ids);
    assert_ne!(mapped.support_chunk_ids, other.support_chunk_ids);
    Ok(())
}

fn assert_original_fact_fields(mapped: &TypedTechnicalFact, original: &TypedTechnicalFact) {
    assert_eq!(mapped.normalized_value, original.normalized_value);
    assert_eq!(mapped.qualifiers, original.qualifiers);
    assert_eq!(mapped.display_value, original.display_value);
    assert_eq!(mapped.extraction_kind, original.extraction_kind);
    assert_eq!(mapped.confidence, original.confidence);
    assert_eq!(mapped.conflict_group_id, original.conflict_group_id);
    assert_eq!(mapped.created_at, original.created_at);
}

#[test]
fn isolated_arm_selection_requires_an_exact_pinned_identity() {
    let arm = || Arm {
        treatment_id: "recursive",
        profile_id: "chars2800-overlap280",
        pdf_only: false,
        documents: Vec::new(),
    };
    assert!(select_arm(vec![arm()], Some("recursive/chars2800-overlap280")).is_ok());
    assert!(select_arm(vec![arm()], Some("structural/chars2800-overlap280")).is_err());
    assert!(select_arm(vec![arm(), arm()], Some("recursive/chars2800-overlap280")).is_err());
}

#[test]
fn replay_dsn_guard_rejects_unsafe_targets() {
    assert!(
        validate_dsn("postgresql://postgres@127.0.0.1:15432/irg256_test?sslmode=disable").is_ok()
    );
    for dsn in [
        "postgresql://postgres@127.0.0.1:5432/irg256_test?sslmode=disable",
        "postgresql://postgres:secret@127.0.0.1:15432/irg256_test?sslmode=disable",
        "postgresql://postgres@localhost:15432/irg256_test?sslmode=disable",
        "postgresql://postgres@127.0.0.1:15432/production?sslmode=disable",
        "postgresql://postgres@127.0.0.1:15432/irg256_test?sslmode=disable&host=other",
        "postgresql://postgres@127.0.0.1:15432/irg256_test?sslmode=disable&sslmode=disable",
        "postgresql://postgres@127.0.0.1:15432/irg256_test",
        "postgresql://app@127.0.0.1:15432/irg256_test?sslmode=disable",
        "postgres://postgres@127.0.0.1:15432/irg256_test?sslmode=disable",
        "postgresql://postgres@127.0.0.1:15432/irg256_%22oops?sslmode=disable",
    ] {
        assert!(validate_dsn(dsn).is_err());
    }
}

#[test]
fn replay_database_guard_rejects_reuse_and_objects() {
    assert!(require_fresh_database(false, 0).is_ok());
    assert!(require_fresh_database(true, 0).is_err());
    assert!(require_fresh_database(false, 1).is_err());
    assert!(require_fresh_database(true, 1).is_err());
}

#[test]
fn replay_file_pin_and_identity_boundaries() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let path = fixture.path().join("replay-pin");
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(b"pinned fixture bytes")?;
    assert!(pinned(&path, &digest(b"pinned fixture bytes")).is_ok());
    assert!(pinned(&path, &digest(b"different bytes")).is_err());
    assert!(pinned(&path, "not-a-hash").is_err());
    file.set_len(INPUT_CAP + 1)?;
    assert!(pinned(&path, &digest(b"pinned fixture bytes")).is_err());
    assert_eq!(identity(&["source", "arm", "chunk"]), identity(&["source", "arm", "chunk"]));
    assert_ne!(identity(&["ab", "c"]), identity(&["a", "bc"]));
    assert_ne!(identity(&["source", "arm-a", "chunk"]), identity(&["source", "arm-b", "chunk"]));
    Ok(())
}
