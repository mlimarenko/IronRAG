use anyhow::Context;

const PGVECTOR_HNSW_VECTOR_MAX_DIM: u64 = 2_000;
const PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES: u64 = 3_000_000_000;
const PG_HNSW_MIN_M: u64 = 8;
const PG_HNSW_MID_M: u64 = 16;
const PG_HNSW_LARGE_M: u64 = 24;

#[derive(Debug, Clone, Copy)]
pub(crate) enum PgVectorStorage {
    Vector,
    Halfvec,
}

impl PgVectorStorage {
    pub(crate) const fn for_dim(dim: u64) -> Self {
        if dim > PGVECTOR_HNSW_VECTOR_MAX_DIM { Self::Halfvec } else { Self::Vector }
    }

    pub(crate) fn column_type(self, dim: i32) -> String {
        match self {
            Self::Vector => format!("vector({dim})"),
            Self::Halfvec => format!("halfvec({dim})"),
        }
    }

    pub(crate) const fn cast_type(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Halfvec => "halfvec",
        }
    }

    pub(crate) const fn cosine_ops(self) -> &'static str {
        match self {
            Self::Vector => "vector_cosine_ops",
            Self::Halfvec => "halfvec_cosine_ops",
        }
    }

    const fn bytes_per_component(self) -> u64 {
        match self {
            Self::Vector => 4,
            Self::Halfvec => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PgHnswIndexParams {
    pub(crate) m: u64,
    pub(crate) ef_construction: u64,
}

pub(crate) fn pg_hnsw_index_params(
    row_count: u64,
    dim: i32,
    storage: PgVectorStorage,
) -> anyhow::Result<PgHnswIndexParams> {
    let dim = u64::try_from(dim).context("vector dimension must be positive")?;
    let configured_m = read_env_u64("IRONRAG_PG_HNSW_M");
    let configured_ef_construction = read_env_u64("IRONRAG_PG_HNSW_EF_CONSTRUCTION");
    let m = configured_m.map_or_else(
        || memory_safe_hnsw_m(row_count, dim, storage),
        |value| value.clamp(PG_HNSW_MIN_M, PG_HNSW_LARGE_M),
    );
    let ef_construction = configured_ef_construction.unwrap_or_else(|| m.saturating_mul(4)).max(m);
    Ok(PgHnswIndexParams { m, ef_construction })
}

fn memory_safe_hnsw_m(row_count: u64, dim: u64, storage: PgVectorStorage) -> u64 {
    let target = if row_count >= 100_000 {
        PG_HNSW_LARGE_M
    } else if row_count >= 1_000 {
        PG_HNSW_MID_M
    } else {
        PG_HNSW_MIN_M
    };
    let budget = pg_hnsw_build_budget_bytes();
    [target, PG_HNSW_MID_M, PG_HNSW_MIN_M]
        .into_iter()
        .find(|&m| estimated_hnsw_build_bytes(row_count, dim, storage, m) <= budget)
        .unwrap_or(PG_HNSW_MIN_M)
}

fn estimated_hnsw_build_bytes(row_count: u64, dim: u64, storage: PgVectorStorage, m: u64) -> u128 {
    let rows = u128::from(row_count.max(1));
    let vector_bytes = u128::from(dim) * u128::from(storage.bytes_per_component());
    let graph_bytes = u128::from(m) * 16;
    rows * (vector_bytes.saturating_mul(2) + graph_bytes)
}

fn pg_hnsw_build_budget_bytes() -> u128 {
    u128::from(
        read_env_u64("IRONRAG_PG_HNSW_BUILD_BUDGET_BYTES")
            .or_else(|| read_env_u64("IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES"))
            .unwrap_or(PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES),
    )
}

pub(crate) fn read_env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            trimmed.parse::<u64>().ok().filter(|value| *value > 0)
        }
    })
}
