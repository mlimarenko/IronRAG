use crate::shared::provider_failure::{ProviderFailure, ProviderFailureKind};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(test)]
use std::future::Future;

use anyhow::{Context, Result as AnyhowResult, anyhow};
use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::{
        knowledge_plane::ActiveEmbeddingProfileSource, knowledge_rows::KNOWLEDGE_CHUNK_VECTOR_KIND,
        repositories::catalog_repository,
    },
    services::{
        ai_catalog_service::{EmbeddingDimensions, ResolvedRuntimeBinding},
        query::error::QueryServiceError,
    },
};

mod inventory;
#[cfg(test)]
mod tests;
mod validation;

pub(crate) use inventory::{
    ensure_active_embedding_profile_key, ensure_embedding_profile_inventory_version_current,
    ensure_library_embedding_profile_indexed, inspect_library_embedding_profile_indexed_uncached,
    load_embedding_profile_inventory_version, validate_active_embedding_profile_key,
};
pub(crate) use validation::{
    validate_embedding_batch_dimensions, validate_embedding_vector_dimensions,
};

const DIMENSION_SUCCESS_TTL: Duration = Duration::from_secs(60);
const DIMENSION_ERROR_TTL: Duration = Duration::from_secs(2);
const DIMENSION_CACHE_MAX_ENTRIES: usize = 512;
// Exact inventory performs a full current chunk/vector reconciliation. The
// cache key is fenced by both vector generation and source-truth version, and
// controlled vector mutations invalidate it explicitly, so a one-minute
// success TTL reduces repeated COUNT pressure without permitting stale reuse.
const PROFILE_INVENTORY_SUCCESS_TTL: Duration = Duration::from_secs(60);
const PROFILE_INVENTORY_ERROR_TTL: Duration = Duration::from_secs(1);
const PROFILE_INVENTORY_CACHE_MAX_ENTRIES: usize = 512;

/// Exact vector-inventory state for the active embedding execution profile.
///
/// `Empty` is a healthy lexical-only state: there is no current readable
/// chunk to vectorize and therefore query-time code must not call the
/// embedding provider or an ANN relation. `Ready` guarantees one non-zero
/// dimension and one active vector for every current readable chunk.
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
/// inventory. `source_truth_version` covers current document changes that
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
    result: Result<EmbeddingDimensionState, CachedQueryFailureSnapshot>,
    expires_at: Instant,
}

#[derive(Debug)]
struct CachedProfileInventoryResolution {
    result: Result<EmbeddingProfileIndexState, CachedQueryFailureSnapshot>,
    expires_at: Instant,
}

#[derive(Debug)]
enum CachedQueryFailureSnapshot {
    StateConflict {
        message: String,
    },
    BindingNotConfigured {
        message: String,
    },
    /// Cloneable provider snapshot; quota exhaustion is deliberately NOT
    /// snapshotted (it falls through to `Opaque`) so a replenished account is
    /// never blocked by a stale cached quota verdict.
    Provider(ProviderFailure),
    Opaque {
        message: String,
    },
}

impl CachedQueryFailureSnapshot {
    fn from_error(error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<QueryServiceError>() {
            match error {
                QueryServiceError::StateConflict { message } => {
                    return Self::StateConflict { message: message.clone() };
                }
                QueryServiceError::BindingNotConfigured { message } => {
                    return Self::BindingNotConfigured { message: message.clone() };
                }
                QueryServiceError::Provider(failure)
                    if failure.kind() == ProviderFailureKind::Unavailable =>
                {
                    return Self::Provider(failure.clone());
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
            Self::Provider(failure) => {
                anyhow::Error::new(QueryServiceError::Provider(failure.clone()))
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
    let result = result.map_err(CachedQueryFailureSnapshot::from_error);
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
