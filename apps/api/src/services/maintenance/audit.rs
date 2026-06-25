//! `audit.*` read-only sweepers.
//!
//! The PostgreSQL knowledge plane is the only runtime store, so there is no
//! separate knowledge-plane footprint to audit for orphan libraries.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use tracing::info;
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories::catalog_repository};

/// Full audit report. Kept as the operator-facing contract for
/// `audit orphan-libraries`; with PostgreSQL as the single knowledge plane,
/// the audit is not applicable and always returns an empty orphan set.
#[derive(Debug, Default, Clone, Serialize)]
pub struct OrphanLibrariesAudit {
    pub orphan_libraries: Vec<OrphanLibraryEntry>,
    pub totals: BTreeMap<String, u64>,
    pub live_library_count: usize,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrphanLibraryEntry {
    pub library_id: String,
    pub collections: BTreeMap<String, u64>,
}

/// PostgreSQL-only orphan-library audit.
pub async fn orphan_libraries(state: &AppState) -> anyhow::Result<OrphanLibrariesAudit> {
    let live_libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let note = "not applicable for postgres knowledge plane: separate orphan-library audit skipped";
    info!("skipping orphan-library audit on postgres knowledge plane");
    Ok(OrphanLibrariesAudit {
        orphan_libraries: Vec::new(),
        totals: BTreeMap::new(),
        live_library_count: live_libraries.len(),
        note: Some(note.to_string()),
    })
}

/// Set of orphan library ids parsed from an [`OrphanLibrariesAudit`].
#[must_use]
pub fn orphan_library_ids(audit: &OrphanLibrariesAudit) -> BTreeSet<Uuid> {
    audit
        .orphan_libraries
        .iter()
        .filter_map(|entry| Uuid::parse_str(&entry.library_id).ok())
        .collect()
}
