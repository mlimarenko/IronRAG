use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(test)]
use std::future::Future;

use anyhow::{Context, Result as AnyhowResult, anyhow};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::{knowledge_rows::KNOWLEDGE_CHUNK_VECTOR_KIND, repositories::catalog_repository},
    services::{
        ai_catalog_service::{EmbeddingDimensions, ResolvedRuntimeBinding},
        query::error::QueryServiceError,
    },
};

const DIMENSION_SUCCESS_TTL: Duration = Duration::from_secs(60);
const DIMENSION_ERROR_TTL: Duration = Duration::from_secs(2);
const DIMENSION_CACHE_MAX_ENTRIES: usize = 512;
// Exact inventory performs a full canonical chunk/vector reconciliation. The
// cache key is fenced by both vector generation and source-truth version, and
// controlled vector mutations invalidate it explicitly, so a one-minute
// success TTL reduces repeated COUNT pressure without permitting stale reuse.
const PROFILE_INVENTORY_SUCCESS_TTL: Duration = Duration::from_secs(60);
const PROFILE_INVENTORY_ERROR_TTL: Duration = Duration::from_secs(1);
const PROFILE_INVENTORY_CACHE_MAX_ENTRIES: usize = 512;

/// Exact vector-inventory state for the active embedding execution profile.
///
/// `Empty` is a healthy lexical-only state: there is no canonical readable
/// chunk to vectorize and therefore query-time code must not call the
/// embedding provider or an ANN relation. `Ready` guarantees one non-zero
/// dimension and one active vector for every canonical readable chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddingProfileIndexState {
    Empty,
    Ready { dimensions: u64 },
}

/// Dimension evidence available before an embedding request is made.
///
/// `Unobserved` is not an error for a chunk-writing path: its first ordinary
/// real batch may establish the exact-profile manifest claim. Query and
/// entity-only paths must require `Known` and fail closed otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddingDimensionState {
    Known(EmbeddingDimensions),
    Unobserved,
}

/// Monotonic-enough source identity used to fence a cached exact vector
/// inventory. `source_truth_version` covers canonical document changes that
/// may not increase the library-wide maximum revision number. Staged vector
/// rebuilds advance it in the atomic promotion transaction, while destructive
/// writes fence before deletion and controlled writes invalidate process-local
/// entries eagerly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EmbeddingProfileInventoryVersion {
    pub(crate) active_vector_generation: i64,
    pub(crate) source_truth_version: i64,
    pub(crate) has_ready_vector: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DimensionCacheKey {
    library_id: Uuid,
    embedding_profile_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProfileInventoryCacheKey {
    library_id: Uuid,
    active_vector_generation: i64,
    source_truth_version: i64,
    has_ready_vector: bool,
    embedding_profile_key: String,
}

#[derive(Debug)]
struct CachedDimensionResolution {
    result: Result<EmbeddingDimensionState, CachedDimensionFailure>,
    expires_at: Instant,
}

#[derive(Debug)]
struct CachedProfileInventoryResolution {
    result: Result<EmbeddingProfileIndexState, CachedProfileInventoryFailure>,
    expires_at: Instant,
}

#[derive(Debug)]
enum CachedProfileInventoryFailure {
    StateConflict { message: String },
    BindingNotConfigured { message: String },
    ProviderUnavailable { message: String },
    Opaque { message: String },
}

impl CachedProfileInventoryFailure {
    fn from_error(error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<QueryServiceError>() {
            match error {
                QueryServiceError::StateConflict { message } => {
                    return Self::StateConflict { message: message.clone() };
                }
                QueryServiceError::BindingNotConfigured { message } => {
                    return Self::BindingNotConfigured { message: message.clone() };
                }
                QueryServiceError::ProviderUnavailable { message } => {
                    return Self::ProviderUnavailable { message: message.clone() };
                }
                _ => {}
            }
        }
        Self::Opaque { message: error.to_string() }
    }

    fn to_error(&self) -> anyhow::Error {
        match self {
            Self::StateConflict { message } => {
                anyhow::Error::new(QueryServiceError::StateConflict { message: message.clone() })
            }
            Self::BindingNotConfigured { message } => {
                anyhow::Error::new(QueryServiceError::BindingNotConfigured {
                    message: message.clone(),
                })
            }
            Self::ProviderUnavailable { message } => {
                anyhow::Error::new(QueryServiceError::ProviderUnavailable {
                    message: message.clone(),
                })
            }
            Self::Opaque { message } => anyhow!(message.clone()),
        }
    }
}

#[derive(Debug)]
enum CachedDimensionFailure {
    BindingNotConfigured { message: String },
    ProviderUnavailable { message: String },
    Opaque { message: String },
}

impl CachedDimensionFailure {
    fn from_error(error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<QueryServiceError>() {
            match error {
                QueryServiceError::BindingNotConfigured { message } => {
                    return Self::BindingNotConfigured { message: message.clone() };
                }
                QueryServiceError::ProviderUnavailable { message } => {
                    return Self::ProviderUnavailable { message: message.clone() };
                }
                _ => {}
            }
        }
        Self::Opaque { message: error.to_string() }
    }

    fn to_error(&self) -> anyhow::Error {
        match self {
            Self::BindingNotConfigured { message } => {
                anyhow::Error::new(QueryServiceError::BindingNotConfigured {
                    message: message.clone(),
                })
            }
            Self::ProviderUnavailable { message } => {
                anyhow::Error::new(QueryServiceError::ProviderUnavailable {
                    message: message.clone(),
                })
            }
            Self::Opaque { message } => anyhow!(message.clone()),
        }
    }
}

/// Bounded cache mapping `(library_id, exact embedding execution profile) ->
/// dimension evidence`. A binding change must never reuse the prior profile's
/// cached dimension merely because the library id stayed the same.
type DimensionCell = Arc<tokio::sync::OnceCell<CachedDimensionResolution>>;
type ProfileInventoryCell = Arc<tokio::sync::OnceCell<CachedProfileInventoryResolution>>;

/// Process-local singleflight registry for dimension discovery.
///
/// Keeping the in-flight cell in the same map as completed values prevents a
/// cold burst from issuing duplicate typed-catalog or exact-profile manifest
/// reads.
/// `OnceCell::get_or_try_init` deliberately leaves the cell empty after an
/// error, so a transient dependency failure is retried by the next caller.
fn library_dim_cache() -> &'static Mutex<HashMap<DimensionCacheKey, DimensionCell>> {
    static CACHE: OnceLock<Mutex<HashMap<DimensionCacheKey, DimensionCell>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn profile_inventory_cache()
-> &'static Mutex<HashMap<ProfileInventoryCacheKey, ProfileInventoryCell>> {
    static CACHE: OnceLock<Mutex<HashMap<ProfileInventoryCacheKey, ProfileInventoryCell>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn dimension_cell(library_id: Uuid, embedding_profile_key: &str) -> DimensionCell {
    let mut cache = library_dim_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    cache.retain(|_, cell| match cell.get() {
        Some(cached) => cached.expires_at > now,
        None => Arc::strong_count(cell) > 1,
    });
    let cache_key =
        DimensionCacheKey { library_id, embedding_profile_key: embedding_profile_key.to_string() };
    if let Some(cell) = cache.get(&cache_key) {
        return Arc::clone(cell);
    }
    let cell = Arc::new(tokio::sync::OnceCell::new());
    if cache.len() < DIMENSION_CACHE_MAX_ENTRIES {
        cache.insert(cache_key, Arc::clone(&cell));
    }
    cell
}

fn profile_inventory_cell(
    library_id: Uuid,
    version: EmbeddingProfileInventoryVersion,
    embedding_profile_key: &str,
) -> ProfileInventoryCell {
    let mut cache =
        profile_inventory_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    cache.retain(|_, cell| match cell.get() {
        Some(cached) => cached.expires_at > now,
        None => Arc::strong_count(cell) > 1,
    });
    let cache_key = ProfileInventoryCacheKey {
        library_id,
        active_vector_generation: version.active_vector_generation,
        source_truth_version: version.source_truth_version,
        has_ready_vector: version.has_ready_vector,
        embedding_profile_key: embedding_profile_key.to_string(),
    };
    if let Some(cell) = cache.get(&cache_key) {
        return Arc::clone(cell);
    }
    let cell = Arc::new(tokio::sync::OnceCell::new());
    if cache.len() < PROFILE_INVENTORY_CACHE_MAX_ENTRIES {
        cache.insert(cache_key, Arc::clone(&cell));
    }
    cell
}

pub(crate) fn invalidate_library_vector_index_dimensions(library_id: Uuid) {
    library_dim_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|key, _| key.library_id != library_id);
    invalidate_library_embedding_profile_inventory(library_id);
}

pub(crate) fn invalidate_vector_index_dimension_cache() {
    library_dim_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    profile_inventory_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}

pub(crate) fn invalidate_library_embedding_profile_inventory(library_id: Uuid) {
    profile_inventory_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|key, _| key.library_id != library_id);
}

/// Require dimensions for the exact already-resolved embedding profile from
/// typed configuration or its durable exact-profile manifest claim. This
/// function never calls a provider; chunk creation uses the state-returning
/// variant below when a first real batch may establish the claim.
pub(crate) async fn library_vector_index_dimensions_for_binding(
    state: &AppState,
    library_id: Uuid,
    binding: &ResolvedRuntimeBinding,
) -> AnyhowResult<u64> {
    require_observed_dimensions(
        library_id,
        library_vector_index_dimension_state_for_binding(state, library_id, binding).await?,
    )
}

/// Resolve typed dimension evidence without calling an embedding provider.
/// Only an exact execution-profile manifest claim may supplement the binding
/// or model catalog; live vector inventory and legacy model keys are not
/// dimension sources.
pub(crate) async fn library_vector_index_dimension_state_for_binding(
    state: &AppState,
    library_id: Uuid,
    binding: &ResolvedRuntimeBinding,
) -> AnyhowResult<EmbeddingDimensionState> {
    let embedding_profile_key = binding.embedding_execution_profile_key();
    let cell = dimension_cell(library_id, &embedding_profile_key);
    let cached = cell
        .get_or_init(|| async {
            cached_dimension_resolution(
                resolve_library_vector_index_dimensions(
                    state,
                    library_id,
                    binding,
                    &embedding_profile_key,
                )
                .await,
            )
        })
        .await;
    cached_dimension_state(cached)
}

fn cached_dimension_resolution(
    result: AnyhowResult<EmbeddingDimensionState>,
) -> CachedDimensionResolution {
    let result = result.map_err(CachedDimensionFailure::from_error);
    let ttl = match &result {
        Ok(EmbeddingDimensionState::Known(_)) => DIMENSION_SUCCESS_TTL,
        Ok(EmbeddingDimensionState::Unobserved) | Err(_) => DIMENSION_ERROR_TTL,
    };
    CachedDimensionResolution { result, expires_at: Instant::now() + ttl }
}

fn cached_dimension_state(
    cached: &CachedDimensionResolution,
) -> AnyhowResult<EmbeddingDimensionState> {
    match &cached.result {
        Ok(state) => Ok(*state),
        Err(error) => Err(error.to_error()),
    }
}

fn require_observed_dimensions(
    library_id: Uuid,
    dimension_state: EmbeddingDimensionState,
) -> AnyhowResult<u64> {
    match dimension_state {
        EmbeddingDimensionState::Known(dimensions) => Ok(dimensions.get()),
        EmbeddingDimensionState::Unobserved => {
            Err(anyhow::Error::new(QueryServiceError::StateConflict {
                message: format!(
                    "embedding dimensions are not declared and library {library_id} has no exact-profile dimension claim; create chunk vectors before query or entity-only vector work"
                ),
            }))
        }
    }
}

async fn resolve_library_vector_index_dimensions(
    state: &AppState,
    library_id: Uuid,
    binding: &ResolvedRuntimeBinding,
    embedding_profile_key: &str,
) -> AnyhowResult<EmbeddingDimensionState> {
    let configured_dimensions = binding.effective_embedding_dimensions;
    let manifest_dimension_claim = if configured_dimensions.is_none() {
        state
            .search_store
            .read_vector_profile_dimension_claim(
                library_id,
                embedding_profile_key,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to read exact-profile vector dimension claim for library {library_id}"
                )
            })?
    } else {
        None
    };

    select_declared_or_manifest_dimension(configured_dimensions, manifest_dimension_claim)
}

fn select_declared_or_manifest_dimension(
    configured_dimensions: Option<EmbeddingDimensions>,
    manifest_dimension_claim: Option<u64>,
) -> AnyhowResult<EmbeddingDimensionState> {
    if let Some(dimensions) = configured_dimensions {
        return Ok(EmbeddingDimensionState::Known(dimensions));
    }
    manifest_dimension_claim.map_or(Ok(EmbeddingDimensionState::Unobserved), |dimensions| {
        EmbeddingDimensions::try_from(dimensions)
            .map(EmbeddingDimensionState::Known)
            .map_err(anyhow::Error::from)
            .context("exact-profile manifest contains an unsupported embedding dimension")
    })
}

/// Return a typed empty/ready preflight for the exact active profile and fail
/// closed for every partial or mixed inventory. An actually empty library is
/// healthy and skips provider embedding plus ANN. A same-model or
/// same-dimension lane is intentionally not a fallback: operators must rebuild
/// before semantic lookup resumes.
pub(crate) async fn ensure_library_embedding_profile_indexed(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    version: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let cell = profile_inventory_cell(library_id, version, embedding_profile_key);
    let cached = cell
        .get_or_init(|| async {
            cached_profile_inventory_resolution(
                resolve_embedding_profile_index_state(
                    state,
                    library_id,
                    embedding_profile_key,
                    version,
                )
                .await,
            )
        })
        .await;
    cached_profile_inventory_result(cached)
}

pub(crate) async fn load_embedding_profile_inventory_version(
    state: &AppState,
    library_id: Uuid,
) -> AnyhowResult<EmbeddingProfileInventoryVersion> {
    let source_truth_version = async {
        catalog_repository::get_library_source_truth_version(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(anyhow::Error::from)
    };
    let (signals, source_truth_version) = tokio::try_join!(
        state.document_store.aggregate_library_generation_signals(library_id),
        source_truth_version,
    )
    .with_context(|| {
        format!("failed to load exact vector inventory version for library {library_id}")
    })?;
    Ok(EmbeddingProfileInventoryVersion {
        active_vector_generation: signals.active_vector_generation,
        source_truth_version,
        has_ready_vector: signals.has_ready_vector,
    })
}

pub(crate) fn validate_embedding_profile_inventory_version(
    library_id: Uuid,
    expected: EmbeddingProfileInventoryVersion,
    observed: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<()> {
    if expected == observed {
        return Ok(());
    }
    Err(anyhow::Error::new(QueryServiceError::StateConflict {
        message: format!(
            "canonical vector generation changed while preparing a query for library {library_id}; retry the query against the current exact-profile inventory"
        ),
    }))
}

pub(crate) async fn ensure_embedding_profile_inventory_version_current(
    state: &AppState,
    library_id: Uuid,
    expected: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<()> {
    let observed = load_embedding_profile_inventory_version(state, library_id).await?;
    validate_embedding_profile_inventory_version(library_id, expected, observed)
}

async fn resolve_embedding_profile_index_state(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
    version: EmbeddingProfileInventoryVersion,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let index_state = inspect_library_embedding_profile_indexed_uncached(
        state,
        library_id,
        embedding_profile_key,
    )
    .await?;
    validate_index_state_against_generation(library_id, version, index_state)
}

/// Maintenance-only exact proof before revision readiness is promoted. Query
/// paths must use the generation-fenced cached wrapper above.
pub(crate) async fn inspect_library_embedding_profile_indexed_uncached(
    state: &AppState,
    library_id: Uuid,
    embedding_profile_key: &str,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let (inventory, expected_chunk_count) = tokio::try_join!(
        state.search_store.inspect_chunk_vector_profile(
            library_id,
            embedding_profile_key,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
        ),
        state.document_store.count_active_chunks_by_library(library_id),
    )
    .with_context(|| {
        format!(
            "failed to inspect exact embedding-profile vector inventory for library {library_id}"
        )
    })?;
    validate_embedding_profile_inventory(
        library_id,
        &inventory.dimensions,
        expected_chunk_count,
        inventory.active_vector_count,
    )
}

fn validate_index_state_against_generation(
    library_id: Uuid,
    version: EmbeddingProfileInventoryVersion,
    index_state: EmbeddingProfileIndexState,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    match index_state {
        EmbeddingProfileIndexState::Empty => Ok(EmbeddingProfileIndexState::Empty),
        EmbeddingProfileIndexState::Ready { .. }
            if version.has_ready_vector && version.active_vector_generation > 0 =>
        {
            Ok(index_state)
        }
        EmbeddingProfileIndexState::Ready { .. } => {
            Err(anyhow::Error::new(QueryServiceError::StateConflict {
                message: format!(
                    "library {library_id} has canonical vectors for the active embedding profile but no active vector generation; rebuild the vector plane before semantic lookup"
                ),
            }))
        }
    }
}

fn cached_profile_inventory_resolution(
    result: AnyhowResult<EmbeddingProfileIndexState>,
) -> CachedProfileInventoryResolution {
    let result = result.map_err(CachedProfileInventoryFailure::from_error);
    let ttl =
        if result.is_ok() { PROFILE_INVENTORY_SUCCESS_TTL } else { PROFILE_INVENTORY_ERROR_TTL };
    CachedProfileInventoryResolution { result, expires_at: Instant::now() + ttl }
}

fn cached_profile_inventory_result(
    cached: &CachedProfileInventoryResolution,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    match &cached.result {
        Ok(index_state) => Ok(*index_state),
        Err(error) => Err(error.to_error()),
    }
}

pub(crate) fn validate_active_embedding_profile_key(
    library_id: Uuid,
    expected_embedding_profile_key: &str,
    active_embedding_profile_key: &str,
) -> AnyhowResult<()> {
    if expected_embedding_profile_key == active_embedding_profile_key {
        return Ok(());
    }
    Err(anyhow::Error::new(QueryServiceError::StateConflict {
        message: format!(
            "active embedding execution profile changed while preparing a query for library {library_id}; retry the query after the vector index is rebuilt for the active profile"
        ),
    }))
}

/// Re-resolve the canonical `EmbedChunk` binding and fence a long-running
/// operation against a profile change. This is intentionally uncached: the
/// caller uses it at a readiness or ANN boundary where stale identity would
/// make vectors from two execution profiles appear interchangeable.
pub(crate) async fn ensure_active_embedding_profile_key(
    state: &AppState,
    library_id: Uuid,
    expected_embedding_profile_key: &str,
) -> AnyhowResult<()> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .with_context(|| {
            format!("failed to re-resolve active embed_chunk binding for library {library_id}")
        })?
        .ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "active embed_chunk binding disappeared while processing library {library_id}; retry after configuring and rebuilding the vector profile"
            ),
        })?;
    validate_active_embedding_profile_key(
        library_id,
        expected_embedding_profile_key,
        &binding.embedding_execution_profile_key(),
    )
}

fn validate_embedding_profile_inventory(
    library_id: Uuid,
    dimensions: &[u64],
    expected_chunk_count: u64,
    vector_chunk_count: u64,
) -> AnyhowResult<EmbeddingProfileIndexState> {
    let mut nonzero_dimensions =
        dimensions.iter().copied().filter(|dimensions| *dimensions > 0).collect::<Vec<_>>();
    nonzero_dimensions.sort_unstable();
    nonzero_dimensions.dedup();
    if expected_chunk_count == 0 && vector_chunk_count == 0 && nonzero_dimensions.is_empty() {
        return Ok(EmbeddingProfileIndexState::Empty);
    }
    if let [dimensions] = nonzero_dimensions.as_slice()
        && expected_chunk_count > 0
        && vector_chunk_count == expected_chunk_count
    {
        return Ok(EmbeddingProfileIndexState::Ready { dimensions: *dimensions });
    }

    Err(anyhow::Error::new(QueryServiceError::StateConflict {
        message: if nonzero_dimensions.is_empty() || vector_chunk_count == 0 {
            format!(
                "library {library_id} has an active vector generation but no vectors for the active embedding execution profile; run `ironrag-maintenance rebuild vector-plane --source-library {library_id}` before vector search"
            )
        } else if vector_chunk_count != expected_chunk_count {
            format!(
                "library {library_id} has an incomplete vector inventory for the active embedding execution profile ({vector_chunk_count}/{expected_chunk_count} active chunks); run `ironrag-maintenance rebuild vector-plane --source-library {library_id}` before vector search"
            )
        } else {
            format!(
                "library {library_id} has multiple vector dimensions for one embedding execution profile; run `ironrag-maintenance rebuild vector-plane --source-library {library_id}` before vector search"
            )
        },
    }))
}

#[cfg(test)]
async fn resolve_dimension_once_for_test<F, Fut>(
    library_id: Uuid,
    embedding_profile_key: &str,
    resolver: F,
) -> AnyhowResult<u64>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = AnyhowResult<u64>>,
{
    let cell = dimension_cell(library_id, embedding_profile_key);
    let cached = cell
        .get_or_init(|| async {
            cached_dimension_resolution(resolver().await.and_then(|dimensions| {
                EmbeddingDimensions::try_from(dimensions)
                    .map(EmbeddingDimensionState::Known)
                    .map_err(anyhow::Error::from)
            }))
        })
        .await;
    require_observed_dimensions(library_id, cached_dimension_state(cached)?)
}

#[cfg(test)]
async fn resolve_profile_inventory_once_for_test<F, Fut>(
    library_id: Uuid,
    version: EmbeddingProfileInventoryVersion,
    embedding_profile_key: &str,
    resolver: F,
) -> AnyhowResult<EmbeddingProfileIndexState>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = AnyhowResult<EmbeddingProfileIndexState>>,
{
    let cell = profile_inventory_cell(library_id, version, embedding_profile_key);
    let cached =
        cell.get_or_init(|| async { cached_profile_inventory_resolution(resolver().await) }).await;
    cached_profile_inventory_result(cached)
}

pub(crate) fn validate_embedding_vector_dimensions(
    expected_dimensions: u64,
    vector: &[f32],
    vector_context: impl std::fmt::Display,
) -> AnyhowResult<i32> {
    if vector.is_empty() {
        return Err(anyhow!("embedding vector for {vector_context} must not be empty"));
    }
    if !vector.iter().all(|value| value.is_finite()) {
        return Err(anyhow!("embedding vector for {vector_context} contains a non-finite value"));
    }

    let actual_dimensions =
        u64::try_from(vector.len()).context("embedding vector dimension overflowed u64")?;
    EmbeddingDimensions::try_from(actual_dimensions)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("embedding vector for {vector_context} cannot be indexed"))?;
    if actual_dimensions != expected_dimensions {
        return Err(anyhow!(
            "embedding vector dimension mismatch for {vector_context}: expected {expected_dimensions} dimensions from the active library embedding binding, got {actual_dimensions}"
        ));
    }

    i32::try_from(vector.len()).context("embedding vector dimension overflowed i32")
}

/// Validate one provider batch as a single, indexable vector space. This is
/// used both when dimensions are already known and when the first ordinary
/// chunk batch establishes them.
pub(crate) fn validate_embedding_batch_dimensions(
    expected_vector_count: usize,
    reported_dimensions: usize,
    embeddings: &[Vec<f32>],
    expected_dimensions: Option<EmbeddingDimensions>,
    batch_context: impl std::fmt::Display,
) -> AnyhowResult<EmbeddingDimensions> {
    let batch_context = batch_context.to_string();
    if embeddings.len() != expected_vector_count {
        return Err(anyhow!(
            "embedding batch for {batch_context} returned {} vectors for {expected_vector_count} inputs",
            embeddings.len()
        ));
    }
    let first = embeddings
        .first()
        .ok_or_else(|| anyhow!("embedding batch for {batch_context} returned no vectors"))?;
    let dimensions = EmbeddingDimensions::try_from(
        u64::try_from(first.len()).context("embedding vector dimension overflowed u64")?,
    )
    .map_err(anyhow::Error::from)
    .with_context(|| format!("embedding batch for {batch_context} cannot be indexed"))?;
    if reported_dimensions != first.len() {
        return Err(anyhow!(
            "embedding batch for {batch_context} reported {reported_dimensions} dimensions but returned vectors with {} dimensions",
            first.len()
        ));
    }
    for (index, embedding) in embeddings.iter().enumerate() {
        validate_embedding_vector_dimensions(
            dimensions.get(),
            embedding,
            format!("{batch_context} vector {index}"),
        )?;
    }
    if let Some(expected_dimensions) = expected_dimensions
        && expected_dimensions != dimensions
    {
        return Err(anyhow!(
            "embedding batch for {batch_context} returned {} dimensions; expected {} from the exact embedding profile",
            dimensions.get(),
            expected_dimensions.get()
        ));
    }
    Ok(dimensions)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    fn dimensions(value: u64) -> EmbeddingDimensions {
        EmbeddingDimensions::try_from(value).unwrap()
    }

    #[test]
    fn validates_expected_embedding_dimensions() {
        assert_eq!(
            i32::try_from(3usize).unwrap(),
            validate_embedding_vector_dimensions(3, &[0.0, 1.0, 2.0], "test vector").unwrap()
        );
    }

    #[test]
    fn rejects_unexpected_embedding_dimensions() {
        let error = validate_embedding_vector_dimensions(3, &[0.0, 1.0], "test vector")
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected 3 dimensions"));
        assert!(error.contains("got 2"));
    }

    #[test]
    fn dimension_resolution_uses_only_declared_or_exact_manifest_metadata() {
        assert_eq!(
            select_declared_or_manifest_dimension(Some(dimensions(768)), None).unwrap(),
            EmbeddingDimensionState::Known(dimensions(768)),
        );
        assert_eq!(
            select_declared_or_manifest_dimension(None, Some(1536)).unwrap(),
            EmbeddingDimensionState::Known(dimensions(1536)),
        );
        assert_eq!(
            select_declared_or_manifest_dimension(None, None).unwrap(),
            EmbeddingDimensionState::Unobserved,
        );
    }

    #[test]
    fn unobserved_dimension_fails_closed_outside_chunk_creation() {
        let error =
            require_observed_dimensions(Uuid::from_u128(40), EmbeddingDimensionState::Unobserved)
                .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));
    }

    #[test]
    fn rejects_unsupported_manifest_dimension_claims() {
        assert!(select_declared_or_manifest_dimension(None, Some(0)).is_err());
        assert!(select_declared_or_manifest_dimension(None, Some(4_001)).is_err());
    }

    #[test]
    fn embedding_batch_requires_exact_count_uniform_width_and_reported_width() {
        let vectors = vec![vec![0.0, 1.0], vec![2.0, 3.0]];
        assert_eq!(
            validate_embedding_batch_dimensions(2, 2, &vectors, None, "test batch").unwrap(),
            dimensions(2),
        );
        assert!(validate_embedding_batch_dimensions(1, 2, &vectors, None, "test batch").is_err());
        assert!(
            validate_embedding_batch_dimensions(
                2,
                2,
                &[vec![0.0, 1.0], vec![2.0]],
                None,
                "test batch",
            )
            .is_err()
        );
        assert!(validate_embedding_batch_dimensions(2, 3, &vectors, None, "test batch").is_err());
    }

    #[test]
    fn embedding_batch_rejects_empty_non_finite_and_out_of_range_vectors() {
        assert!(validate_embedding_batch_dimensions(0, 0, &[], None, "test batch").is_err());
        assert!(
            validate_embedding_batch_dimensions(1, 2, &[vec![0.0, f32::NAN]], None, "test batch",)
                .is_err()
        );
        assert!(
            validate_embedding_batch_dimensions(1, 4_001, &[vec![0.0; 4_001]], None, "test batch",)
                .is_err()
        );
    }

    #[test]
    fn active_embedding_profile_requires_exact_persisted_vector_inventory() {
        let library_id = Uuid::from_u128(41);
        assert_eq!(
            validate_embedding_profile_inventory(library_id, &[1536], 2, 2).unwrap(),
            EmbeddingProfileIndexState::Ready { dimensions: 1536 }
        );

        let error = validate_embedding_profile_inventory(library_id, &[], 2, 0).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));
        let message = error.to_string();
        assert!(message.contains("rebuild vector-plane"));
        assert!(message.contains("--source-library"));
        assert!(message.contains(&library_id.to_string()));

        let ambiguous =
            validate_embedding_profile_inventory(library_id, &[768, 1536], 2, 2).unwrap_err();
        assert!(matches!(
            ambiguous.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));

        let partial = validate_embedding_profile_inventory(library_id, &[1536], 2, 1).unwrap_err();
        assert!(partial.to_string().contains("incomplete vector inventory"));
    }

    #[test]
    fn empty_library_has_an_explicit_non_vector_search_state() {
        let library_id = Uuid::from_u128(42);

        assert_eq!(
            validate_embedding_profile_inventory(library_id, &[], 0, 0).unwrap(),
            EmbeddingProfileIndexState::Empty
        );
    }

    #[test]
    fn empty_library_rejects_stale_vectors() {
        let library_id = Uuid::from_u128(44);

        let error = validate_embedding_profile_inventory(library_id, &[1536], 0, 1).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));
    }

    #[test]
    fn non_empty_inventory_without_active_generation_fails_closed() {
        let library_id = Uuid::from_u128(45);
        let version = EmbeddingProfileInventoryVersion {
            active_vector_generation: 0,
            source_truth_version: 1,
            has_ready_vector: false,
        };

        let error = validate_index_state_against_generation(
            library_id,
            version,
            EmbeddingProfileIndexState::Ready { dimensions: 1536 },
        )
        .unwrap_err();

        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));
    }

    #[test]
    fn non_empty_library_without_generation_cannot_silently_downgrade_to_empty() {
        let library_id = Uuid::from_u128(49);

        let error = validate_embedding_profile_inventory(library_id, &[], 2, 0).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));
    }

    #[test]
    fn empty_inventory_is_healthy_without_active_generation() {
        let version = EmbeddingProfileInventoryVersion {
            active_vector_generation: 0,
            source_truth_version: 1,
            has_ready_vector: false,
        };

        assert_eq!(
            validate_index_state_against_generation(
                Uuid::from_u128(46),
                version,
                EmbeddingProfileIndexState::Empty,
            )
            .unwrap(),
            EmbeddingProfileIndexState::Empty
        );
    }

    #[test]
    fn inventory_version_fence_rejects_generation_or_source_change() {
        let library_id = Uuid::from_u128(47);
        let expected = EmbeddingProfileInventoryVersion {
            active_vector_generation: 7,
            source_truth_version: 11,
            has_ready_vector: true,
        };

        assert!(
            validate_embedding_profile_inventory_version(library_id, expected, expected).is_ok()
        );
        assert!(
            validate_embedding_profile_inventory_version(
                library_id,
                expected,
                EmbeddingProfileInventoryVersion { active_vector_generation: 8, ..expected },
            )
            .is_err()
        );
        assert!(
            validate_embedding_profile_inventory_version(
                library_id,
                expected,
                EmbeddingProfileInventoryVersion { source_truth_version: 12, ..expected },
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn profile_inventory_cache_is_generation_fenced_and_explicitly_invalidated() {
        let library_id = Uuid::from_u128(48);
        invalidate_library_embedding_profile_inventory(library_id);
        let calls = Arc::new(AtomicUsize::new(0));
        let version = EmbeddingProfileInventoryVersion {
            active_vector_generation: 3,
            source_truth_version: 5,
            has_ready_vector: true,
        };

        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            assert_eq!(
                resolve_profile_inventory_once_for_test(
                    library_id,
                    version,
                    "profile-cache-fence",
                    move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
                    },
                )
                .await
                .unwrap(),
                EmbeddingProfileIndexState::Ready { dimensions: 3 }
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let calls_after_generation = Arc::clone(&calls);
        resolve_profile_inventory_once_for_test(
            library_id,
            EmbeddingProfileInventoryVersion { active_vector_generation: 4, ..version },
            "profile-cache-fence",
            move || async move {
                calls_after_generation.fetch_add(1, Ordering::SeqCst);
                Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
            },
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        invalidate_library_embedding_profile_inventory(library_id);
        let calls_after_invalidation = Arc::clone(&calls);
        resolve_profile_inventory_once_for_test(
            library_id,
            version,
            "profile-cache-fence",
            move || async move {
                calls_after_invalidation.fetch_add(1, Ordering::SeqCst);
                Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
            },
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn profile_inventory_cache_singleflights_concurrent_misses() {
        let library_id = Uuid::from_u128(50);
        invalidate_library_embedding_profile_inventory(library_id);
        let calls = Arc::new(AtomicUsize::new(0));
        let version = EmbeddingProfileInventoryVersion {
            active_vector_generation: 9,
            source_truth_version: 13,
            has_ready_vector: true,
        };
        let futures = (0..16).map(|_| {
            let calls = Arc::clone(&calls);
            async move {
                resolve_profile_inventory_once_for_test(
                    library_id,
                    version,
                    "profile-concurrent-fence",
                    move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok(EmbeddingProfileIndexState::Ready { dimensions: 3 })
                    },
                )
                .await
            }
        });

        let results = futures::future::join_all(futures).await;

        assert!(results.into_iter().all(|result| result.is_ok()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn active_embedding_profile_must_match_the_query_vector_provenance() {
        let library_id = Uuid::from_u128(43);
        assert!(
            validate_active_embedding_profile_key(
                library_id,
                "embedding-profile:v1:active",
                "embedding-profile:v1:active",
            )
            .is_ok()
        );
        let error = validate_active_embedding_profile_key(
            library_id,
            "embedding-profile:v1:query",
            "embedding-profile:v1:active",
        )
        .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::StateConflict { .. })
        ));
    }

    #[test]
    fn negative_cache_preserves_typed_binding_failure() {
        let cached = cached_dimension_resolution(Err(anyhow::Error::new(
            QueryServiceError::BindingNotConfigured {
                message: "query_provider_failed".to_string(),
            },
        )));
        let error = cached_dimension_state(&cached).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::BindingNotConfigured { .. })
        ));
    }

    #[test]
    fn negative_cache_does_not_infer_binding_failure_from_message() {
        let cached = cached_dimension_resolution(Err(anyhow!("query_binding_not_configured")));
        let error = cached_dimension_state(&cached).unwrap_err();

        assert!(error.downcast_ref::<QueryServiceError>().is_none());
    }

    #[test]
    fn negative_cache_preserves_typed_provider_failure() {
        let cached = cached_dimension_resolution(Err(anyhow::Error::new(
            QueryServiceError::ProviderUnavailable {
                message: "query_binding_not_configured".to_string(),
            },
        )));
        let error = cached_dimension_state(&cached).unwrap_err();

        assert!(matches!(
            error.downcast_ref::<QueryServiceError>(),
            Some(QueryServiceError::ProviderUnavailable { .. })
        ));
    }

    #[tokio::test]
    async fn concurrent_cold_dimension_resolution_is_singleflight() {
        let library_id = Uuid::now_v7();
        invalidate_library_vector_index_dimensions(library_id);
        let calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(8));
        let mut tasks = Vec::new();

        for _ in 0..8 {
            let calls = Arc::clone(&calls);
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                resolve_dimension_once_for_test(
                    library_id,
                    "embedding-profile:v1:shared",
                    || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        Ok(1536)
                    },
                )
                .await
            }));
        }

        for task in tasks {
            assert_eq!(task.await.expect("dimension task should join").unwrap(), 1536);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        invalidate_library_vector_index_dimensions(library_id);
    }

    #[tokio::test]
    async fn invalidation_transitions_cached_unobserved_state_to_a_learned_claim() {
        let library_id = Uuid::now_v7();
        let profile = "embedding-profile:v1:learned-transition";
        invalidate_library_vector_index_dimensions(library_id);
        let cell = dimension_cell(library_id, profile);
        let cached = cell
            .get_or_init(|| async {
                cached_dimension_resolution(Ok(EmbeddingDimensionState::Unobserved))
            })
            .await;
        assert_eq!(cached_dimension_state(cached).unwrap(), EmbeddingDimensionState::Unobserved);

        invalidate_library_vector_index_dimensions(library_id);
        assert_eq!(
            resolve_dimension_once_for_test(library_id, profile, || async { Ok(3) }).await.unwrap(),
            3
        );
        invalidate_library_vector_index_dimensions(library_id);
    }

    #[tokio::test]
    async fn failed_dimension_resolution_is_coalesced_and_retryable_after_invalidation() {
        let library_id = Uuid::now_v7();
        invalidate_library_vector_index_dimensions(library_id);
        let first =
            resolve_dimension_once_for_test(library_id, "embedding-profile:v1:shared", || async {
                Err(anyhow!("synthetic transient failure"))
            })
            .await;
        assert!(first.is_err());

        let repeated =
            resolve_dimension_once_for_test(library_id, "embedding-profile:v1:shared", || async {
                Ok(768)
            })
            .await;
        assert!(repeated.is_err(), "the short negative-cache window must coalesce retries");

        invalidate_library_vector_index_dimensions(library_id);
        let recovered =
            resolve_dimension_once_for_test(library_id, "embedding-profile:v1:shared", || async {
                Ok(768)
            })
            .await
            .unwrap();
        assert_eq!(recovered, 768);
        invalidate_library_vector_index_dimensions(library_id);
    }

    #[tokio::test]
    async fn dimension_cache_isolated_by_embedding_execution_profile() {
        let library_id = Uuid::now_v7();
        invalidate_library_vector_index_dimensions(library_id);

        let first =
            resolve_dimension_once_for_test(library_id, "embedding-profile:v1:first", || async {
                Ok(1536)
            })
            .await
            .unwrap();
        let second =
            resolve_dimension_once_for_test(library_id, "embedding-profile:v1:second", || async {
                Ok(768)
            })
            .await
            .unwrap();

        assert_eq!(first, 1536);
        assert_eq!(second, 768);
        invalidate_library_vector_index_dimensions(library_id);
    }
}
