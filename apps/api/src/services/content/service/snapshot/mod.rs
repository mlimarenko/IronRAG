//! Library snapshot — streaming tar.zst export and import.
//!
//! The archive layout is:
//!
//! ```text
//! manifest.json                         # first — declares include kinds and table list
//! postgres/<table>/part-NNNNNN.ndjson   # chunked per table, 64 MiB cap per part
//! blobs/<escaped-storage-key>           # raw bytes, one entry per content blob
//! summary.json                          # last — row counts observed during export
//! ```
//!
//! Export is a single tar stream wrapped in zstd. The `async_tar::Builder`
//! writes into a `ZstdEncoder` which writes into a `tokio::io::DuplexStream`
//! write half; the HTTP layer reads the other half as a response body
//! stream. Back-pressure is natural — if the client stops reading, the
//! exporter task blocks on the next `builder.append` and Postgres cursors
//! pause with it.
//!
//! Import takes the raw request body as an async stream, wraps it in a
//! zstd decoder, hands it to `async_tar::Archive`, and processes entries
//! in their serialized order after spooling and validating the complete inventory
//! and schema-4 revision admissions before any target mutation. Source-owned blobs
//! include restricted and pending revisions; backup is not a public-read surface.
//!
//! The `include` query parameter on export selects which families of
//! entities end up in the archive. Import does NOT take an include filter
//! — it trusts the manifest that the archive itself carries, which is the
//! authoritative source of what was exported.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::infra::postgres::pg_search_store::{
    CHUNK_VECTOR_RELATION_PREFIX, list_physical_vector_relations_in_transaction,
    selectable_chunk_vector_rows_sql,
};
use anyhow::{Context, anyhow, bail};
use async_compression::tokio::{bufread::ZstdDecoder, write::ZstdEncoder};
use async_tar::{Archive, Builder, EntryType, Header};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgPool, Row};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        postgres::pg_vector_config::{
            PgVectorStorage, ensure_vector_relation_library_cascade, pg_hnsw_index_params,
        },
        repositories::content_mutation_repository,
    },
    services::content::{error::ContentServiceError, storage::StashedContentDirectory},
};

mod archive_io;
mod batch;
mod content_closure;
mod descriptors;
mod export;
mod import;
mod integrity;
mod model;
mod postgres_export;
mod postgres_restore;
mod restore_target;
mod storage;
mod vector_restore;
mod workspace;

pub use export::export_library_archive;
pub use import::restore_library_archive;
pub(crate) use integrity::MAX_SNAPSHOT_ARCHIVE_BYTES;
pub use integrity::{SnapshotEntryDigest, SnapshotInventory};
pub use model::{
    IncludeKind, OverwriteMode, SNAPSHOT_SCHEMA_VERSION, SnapshotImportError,
    SnapshotImportFailureKind, SnapshotImportReport, SnapshotManifest, SnapshotSummary,
    VectorShardEntry,
};
pub use workspace::{
    WorkspaceLibraryImportReport, WorkspaceManifestLibrary, WorkspaceSnapshotImportReport,
    WorkspaceSnapshotInventory, WorkspaceSnapshotManifest, WorkspaceSnapshotSummary,
    export_workspace_archive, restore_workspace_archive,
};

use archive_io::*;
use batch::*;
use content_closure::*;
use descriptors::*;
use import::*;
use integrity::*;
use model::*;
use postgres_export::*;
use postgres_restore::*;
use restore_target::*;
use storage::*;
use vector_restore::*;

#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod content_closure_tests;
#[cfg(test)]
mod descriptors_tests;
#[cfg(test)]
mod export_tests;
#[cfg(test)]
mod integrity_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod postgres_restore_tests;
#[cfg(test)]
mod restore_target_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod vector_restore_tests;
#[cfg(test)]
mod workspace_tests;
