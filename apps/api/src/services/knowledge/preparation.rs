//! Durable, library-scoped coordination for full graph topology preparation.

use std::{
    collections::HashSet,
    io,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    services::{
        knowledge::graph_stream::artifact::{ArtifactManifest, artifact_key},
        maintenance::lease::MaintenanceClass,
    },
};

const FORMAT_VERSION: i32 = 1;
const POLL_INTERVAL_MS: i64 = 1_000;
const LEASE_SECONDS: i64 = 120;
// Cleanup waits beyond the worker's bounded upload deadline after lease loss.
const UPLOAD_GRACE_SECONDS: i64 = 3600;
const MAX_ATTEMPTS: i32 = 3;
const MAX_RECORDS: usize = 64;
const MAX_DEBTS: usize = 32;
const TERMINAL_HISTORY: usize = 8;
// Leave room for PostgreSQL's jsonb text formatting in the SQL-side limit.
const MAX_CURSOR_BYTES: usize = 196_608;
const OUTPUT_PERMISSION_SECONDS: u64 = 90;
const CLAIM_GUARD: i64 = 0x4952_4733_3532;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphTopologyIdentity {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub projection_version: i64,
    pub topology_generation: i64,
    pub source_truth_version: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreparationStatus {
    Queued,
    Validating,
    Building,
    Publishing,
    Ready,
    Retrying,
    Failed,
    Superseded,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeGraphPreparationResponse {
    pub id: Uuid,
    pub library_id: Uuid,
    pub format_version: i32,
    pub projection_version: i64,
    pub topology_generation: i64,
    pub source_truth_version: i64,
    pub status: PreparationStatus,
    pub attempts: i32,
    pub retry_after_ms: i64,
    #[schema(required = true)]
    pub expires_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparationRecord {
    pub id: Uuid,
    pub identity: GraphTopologyIdentity,
    pub artifact: Option<ArtifactManifest>,
    status: PreparationStatus,
    attempts: i32,
    retry_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    error_code: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Claim {
    pub row_id: Uuid,
    pub record: PreparationRecord,
    pub attempt_id: Uuid,
    output_deadline: Arc<Mutex<Option<Instant>>>,
}

impl Claim {
    /// This local fence also works on a blocking renderer when Tokio is stalled.
    pub fn output_is_permitted(&self) -> bool {
        self.output_deadline
            .lock()
            .is_ok_and(|deadline| deadline.is_some_and(|until| Instant::now() < until))
    }

    fn renew_output_permission(&self, request_started: Instant) {
        if let Ok(mut deadline) = self.output_deadline.lock() {
            let renewed =
                request_started + std::time::Duration::from_secs(OUTPUT_PERMISSION_SECONDS);
            if let Some(current) = deadline.as_mut() {
                *current = (*current).max(renewed);
            }
        }
    }

    fn revoke_output_permission(&self) {
        if let Ok(mut deadline) = self.output_deadline.lock() {
            *deadline = None;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    #[error("preparation not found")]
    NotFound,
    #[error("preparation expired")]
    Expired,
    #[error("preparation not ready")]
    NotReady,
    #[error("preparation capacity is awaiting cleanup")]
    CapacityExhausted,
    #[error("preparation failed: {0}")]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for PreparationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(error.into())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Active {
    preparation_id: Uuid,
    attempt_id: Uuid,
    heartbeat_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Debt {
    workspace_id: Uuid,
    library_id: Uuid,
    preparation_id: Uuid,
    attempt_id: Uuid,
    key: String,
    not_before: DateTime<Utc>,
    cleanup: Option<CleanupLease>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CleanupLease {
    id: Uuid,
    until: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cursor {
    version: u8,
    desired: Uuid,
    records: Vec<PreparationRecord>,
    active: Option<Active>,
    debts: Vec<Debt>,
}

impl Cursor {
    fn decode(value: serde_json::Value) -> Result<Self, PreparationError> {
        let records = value
            .get("records")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("invalid topology coordinator records"))?;
        let debts = value
            .get("debts")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("invalid topology coordinator debts"))?;
        if records.len() > MAX_RECORDS || debts.len() > MAX_DEBTS {
            return Err(anyhow!("topology coordinator capacity exceeded").into());
        }
        check_cursor_size(&value)?;
        let cursor: Self =
            serde_json::from_value(value).context("invalid topology coordinator cursor")?;
        let mut ids = std::collections::HashSet::new();
        if cursor.version != 1
            || !cursor.records.iter().any(|r| r.id == cursor.desired)
            || cursor
                .active
                .as_ref()
                .is_some_and(|a| !cursor.records.iter().any(|r| r.id == a.preparation_id))
            || cursor.records.iter().any(|r| {
                !ids.insert(r.id)
                    || r.identity.projection_version <= 0
                    || r.identity.topology_generation < 0
                    || r.identity.source_truth_version <= 0
                    || (r.identity.topology_generation == 0 && r.identity.projection_version != 1)
                    || r.attempts < 0
                    || r.attempts > MAX_ATTEMPTS
            })
            || cursor.debts.iter().any(|d| {
                cursor.records.iter().find(|r| r.id == d.preparation_id).is_none_or(|r| {
                    r.identity.workspace_id != d.workspace_id
                        || r.identity.library_id != d.library_id
                        || artifact_key(&r.identity, r.id, d.attempt_id) != d.key
                })
            })
            || cursor.records.iter().any(|r| {
                r.artifact.as_ref().is_some_and(|artifact| {
                    !cursor
                        .debts
                        .iter()
                        .any(|d| d.preparation_id == r.id && d.key == artifact.storage_key)
                })
            })
            || cursor.active.as_ref().is_some_and(|a| {
                !cursor
                    .debts
                    .iter()
                    .any(|d| d.preparation_id == a.preparation_id && d.attempt_id == a.attempt_id)
            })
        {
            return Err(anyhow!("invalid topology coordinator identity").into());
        }
        Ok(cursor)
    }
    fn record(&self, id: Uuid) -> Result<&PreparationRecord, PreparationError> {
        self.records.iter().find(|r| r.id == id).ok_or(PreparationError::NotFound)
    }
    fn record_mut(&mut self, id: Uuid) -> Result<&mut PreparationRecord, PreparationError> {
        self.records.iter_mut().find(|r| r.id == id).ok_or(PreparationError::NotFound)
    }

    fn prune_history(&mut self) {
        let mut retained: HashSet<Uuid> =
            self.debts.iter().map(|debt| debt.preparation_id).collect();
        retained.insert(self.desired);
        if let Some(active) = &self.active {
            retained.insert(active.preparation_id);
        }
        let history: Vec<Uuid> = self
            .records
            .iter()
            .rev()
            .filter(|r| !retained.contains(&r.id))
            .take(TERMINAL_HISTORY)
            .map(|r| r.id)
            .collect();
        retained.extend(history);
        self.records.retain(|record| retained.contains(&record.id));
    }

    fn reject_claim_when_debt_full(&mut self) -> Result<bool, PreparationError> {
        if self.debts.len() < MAX_DEBTS {
            return Ok(false);
        }
        let desired = self.desired;
        let record = self.record_mut(desired)?;
        record.status = PreparationStatus::Failed;
        record.error_code = Some("cleanup_capacity".to_owned());
        Ok(true)
    }
}

fn check_cursor_size(value: &serde_json::Value) -> Result<(), PreparationError> {
    struct Budget(usize);
    impl io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("cursor too large"))?;
            if self.0 > MAX_CURSOR_BYTES {
                return Err(io::Error::other("cursor too large"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(0), value)
        .context("topology coordinator cursor exceeds byte budget")?;
    Ok(())
}

fn response(
    record: &PreparationRecord,
    current: bool,
    now: DateTime<Utc>,
) -> KnowledgeGraphPreparationResponse {
    let status = if now >= record.expires_at {
        PreparationStatus::Expired
    } else if !current {
        PreparationStatus::Superseded
    } else {
        record.status
    };
    KnowledgeGraphPreparationResponse {
        id: record.id,
        library_id: record.identity.library_id,
        format_version: FORMAT_VERSION,
        projection_version: record.identity.projection_version,
        topology_generation: record.identity.topology_generation,
        source_truth_version: record.identity.source_truth_version,
        status,
        attempts: record.attempts,
        retry_after_ms: match status {
            PreparationStatus::Queued
            | PreparationStatus::Validating
            | PreparationStatus::Building
            | PreparationStatus::Publishing => POLL_INTERVAL_MS,
            PreparationStatus::Retrying => {
                (record.retry_at - now).num_milliseconds().max(POLL_INTERVAL_MS)
            }
            PreparationStatus::Ready
            | PreparationStatus::Failed
            | PreparationStatus::Superseded
            | PreparationStatus::Expired => 0,
        },
        expires_at: Some(record.expires_at),
        error_code: record.error_code.clone(),
    }
}

const IDENTITY_SQL: &str = "select l.workspace_id, l.id, l.source_truth_version, \
    s.graph_status, s.projection_version, s.topology_generation, s.node_count, s.edge_count \
    from catalog_library l left join runtime_graph_snapshot s on s.library_id = l.id where l.id = $1";

async fn desired_identity(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<GraphTopologyIdentity, PreparationError> {
    type Row =
        (Uuid, Uuid, i64, Option<String>, Option<i64>, Option<i64>, Option<i32>, Option<i32>);
    let row: Row = sqlx::query_as(IDENTITY_SQL)
        .bind(library_id)
        .fetch_optional(pool)
        .await?
        .ok_or(PreparationError::NotFound)?;
    let (
        workspace_id,
        library_id,
        source_truth_version,
        status,
        projection,
        generation,
        nodes,
        edges,
    ) = row;
    let (projection_version, topology_generation) =
        match (status.as_deref(), projection, generation, nodes, edges) {
            (None, None, None, None, None) => (1, 0),
            (Some("ready"), Some(p), Some(g), Some(n), Some(e))
                if p > 0 && g > 0 && n >= 0 && e >= 0 =>
            {
                (p, g)
            }
            (Some("empty"), Some(p), Some(g), Some(0), Some(0)) if p > 0 && g > 0 => (p, g),
            _ => return Err(PreparationError::NotReady),
        };
    Ok(GraphTopologyIdentity {
        workspace_id,
        library_id,
        projection_version,
        topology_generation,
        source_truth_version,
    })
}

const CURRENT_SQL: &str = "select exists(select 1 from catalog_library l left join runtime_graph_snapshot s on s.library_id=l.id \
         where l.id=$1 and l.workspace_id=$2 and l.source_truth_version=$3 and \
         ((s.library_id is null and $4=1 and $5=0) or \
          (s.projection_version=$4 and s.topology_generation=$5 and $4>0 and $5>0 and \
           ((s.graph_status='ready' and s.node_count>=0 and s.edge_count>=0) or \
            (s.graph_status='empty' and s.node_count=0 and s.edge_count=0)))))";

pub async fn identity_is_current(
    pool: &PgPool,
    identity: &GraphTopologyIdentity,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(CURRENT_SQL)
        .bind(identity.library_id)
        .bind(identity.workspace_id)
        .bind(identity.source_truth_version)
        .bind(identity.projection_version)
        .bind(identity.topology_generation)
        .fetch_one(pool)
        .await
}

async fn save(
    tx: &mut Transaction<'_, Postgres>,
    row_id: Uuid,
    cursor: &Cursor,
) -> Result<(), PreparationError> {
    let value = serde_json::to_value(cursor).context("serialize topology coordinator")?;
    if cursor.records.len() > MAX_RECORDS || cursor.debts.len() > MAX_DEBTS {
        return Err(anyhow!("topology coordinator capacity exceeded").into());
    }
    check_cursor_size(&value)?;
    sqlx::query("update maintenance_job_run set cursor_json=$2, updated_at=now() where id=$1")
        .bind(row_id)
        .bind(value)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn lock_row(
    tx: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> Result<Option<(Uuid, Cursor)>, PreparationError> {
    let row: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select id,case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where class=$1 and scope_kind='library' and scope_id=$2 for update")
        .bind(MaintenanceClass::GraphTopologyPreparation.as_str()).bind(library_id).fetch_optional(&mut **tx).await?;
    row.map(|(id, value)| Ok((id, Cursor::decode(value)?))).transpose()
}

pub async fn admit(
    state: &AppState,
    library_id: Uuid,
) -> Result<KnowledgeGraphPreparationResponse, PreparationError> {
    let pool = &state.persistence.postgres;
    let identity = desired_identity(pool, library_id).await?;
    let now = Utc::now();
    let record = PreparationRecord {
        id: Uuid::now_v7(),
        identity,
        artifact: None,
        status: PreparationStatus::Queued,
        attempts: 0,
        retry_at: now,
        expires_at: now + Duration::hours(24),
        error_code: None,
    };
    let mut tx = pool.begin().await?;
    let initial = Cursor {
        version: 1,
        desired: record.id,
        records: vec![record.clone()],
        active: None,
        debts: vec![],
    };
    sqlx::query("insert into maintenance_job_run(class,scope_kind,scope_id,cursor_json) values($1,'library',$2,$3) \
        on conflict (class,scope_kind,coalesce(scope_id,'00000000-0000-0000-0000-000000000000'::uuid)) do nothing")
        .bind(MaintenanceClass::GraphTopologyPreparation.as_str()).bind(library_id)
        .bind(serde_json::to_value(&initial).context("serialize initial topology coordinator")?).execute(&mut *tx).await?;
    let (row_id, mut cursor) =
        lock_row(&mut tx, library_id).await?.ok_or(PreparationError::NotFound)?;
    let current: bool = sqlx::query_scalar(CURRENT_SQL)
        .bind(identity.library_id)
        .bind(identity.workspace_id)
        .bind(identity.source_truth_version)
        .bind(identity.projection_version)
        .bind(identity.topology_generation)
        .fetch_one(&mut *tx)
        .await?;
    if !current {
        return Err(PreparationError::NotReady);
    }
    let previous = cursor.record(cursor.desired)?;
    let reuse = previous.identity == identity
        && previous.expires_at > now
        && !matches!(
            previous.status,
            PreparationStatus::Failed | PreparationStatus::Superseded | PreparationStatus::Expired
        );
    if !reuse {
        if cursor.debts.len() >= MAX_DEBTS {
            return Err(PreparationError::CapacityExhausted);
        }
        cursor.prune_history();
        if cursor.records.len() >= MAX_RECORDS {
            return Err(PreparationError::CapacityExhausted);
        }
        let previous_id = cursor.desired;
        let old = cursor.record_mut(previous_id)?;
        if old.expires_at <= now {
            old.status = PreparationStatus::Expired;
        } else if !matches!(old.status, PreparationStatus::Failed | PreparationStatus::Expired) {
            old.status = PreparationStatus::Superseded;
        }
        cursor.desired = record.id;
        cursor.records.push(record);
        save(&mut tx, row_id, &cursor).await?;
    }
    let result = response(cursor.record(cursor.desired)?, true, now);
    tx.commit().await?;
    Ok(result)
}

async fn lookup(
    state: &AppState,
    library_id: Uuid,
    id: Uuid,
) -> Result<PreparationRecord, PreparationError> {
    let value: Option<serde_json::Value> = sqlx::query_scalar("select case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where class=$1 and scope_kind='library' and scope_id=$2")
        .bind(MaintenanceClass::GraphTopologyPreparation.as_str()).bind(library_id).fetch_optional(&state.persistence.postgres).await?;
    let cursor = Cursor::decode(value.ok_or(PreparationError::NotFound)?)?;
    let record = cursor.record(id)?.clone();
    let workspace: Option<Uuid> =
        sqlx::query_scalar("select workspace_id from catalog_library where id=$1")
            .bind(library_id)
            .fetch_optional(&state.persistence.postgres)
            .await?;
    if record.identity.library_id != library_id || workspace != Some(record.identity.workspace_id) {
        return Err(PreparationError::NotFound);
    }
    Ok(record)
}

pub async fn status(
    state: &AppState,
    library_id: Uuid,
    preparation_id: Uuid,
) -> Result<KnowledgeGraphPreparationResponse, PreparationError> {
    let record = lookup(state, library_id, preparation_id).await?;
    let current = identity_is_current(&state.persistence.postgres, &record.identity).await?;
    Ok(response(&record, current, Utc::now()))
}

pub async fn result_record(
    state: &AppState,
    library_id: Uuid,
    preparation_id: Uuid,
) -> Result<PreparationRecord, PreparationError> {
    let record = lookup(state, library_id, preparation_id).await?;
    if Utc::now() >= record.expires_at {
        return Err(PreparationError::Expired);
    }
    if record.status != PreparationStatus::Ready
        || record.artifact.is_none()
        || !identity_is_current(&state.persistence.postgres, &record.identity).await?
    {
        return Err(PreparationError::NotReady);
    }
    Ok(record)
}

fn release_attempt(
    cursor: &mut Cursor,
    now: DateTime<Utc>,
    error: &str,
) -> Result<(), PreparationError> {
    let Some(active) = cursor.active.take() else {
        return Ok(());
    };
    for debt in &mut cursor.debts {
        if debt.attempt_id == active.attempt_id {
            debt.not_before = now + Duration::seconds(UPLOAD_GRACE_SECONDS);
        }
    }
    let desired = cursor.desired;
    let record = cursor.record_mut(active.preparation_id)?;
    if record.id != desired || record.status == PreparationStatus::Superseded {
        record.status = PreparationStatus::Superseded;
    } else if now >= record.expires_at {
        record.status = PreparationStatus::Expired;
    } else if record.attempts >= MAX_ATTEMPTS {
        record.status = PreparationStatus::Failed;
        record.error_code = Some(error.to_owned());
    } else {
        record.status = PreparationStatus::Retrying;
        record.retry_at = now + Duration::seconds(if record.attempts == 1 { 60 } else { 300 });
        record.error_code = Some(error.to_owned());
    }
    Ok(())
}

pub async fn claim_next(state: &AppState) -> Result<Option<Claim>, PreparationError> {
    let request_started = Instant::now();
    let mut tx = state.persistence.postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock($1)").bind(CLAIM_GUARD).execute(&mut *tx).await?;
    let class = MaintenanceClass::GraphTopologyPreparation.as_str();
    let now = Utc::now();
    // The instance guard makes this check and selection one cluster-wide decision.
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select id,case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where class=$1 and scope_kind='library' \
         and cursor_json->'active' <> 'null'::jsonb for update")
        .bind(class).fetch_all(&mut *tx).await?;
    for (id, value) in rows {
        let mut cursor = Cursor::decode(value)?;
        if let Some(active) = &cursor.active {
            if active.heartbeat_at + Duration::seconds(LEASE_SECONDS) > now {
                tx.commit().await?;
                return Ok(None);
            }
            release_attempt(&mut cursor, now, "lease_expired")?;
            save(&mut tx, id, &cursor).await?;
        }
    }
    let candidate: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select id,case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where class=$1 and scope_kind='library' \
         and exists (select 1 from jsonb_array_elements(cursor_json->'records') r \
             where r->>'id'=cursor_json->>'desired' and r->>'status' in ('queued','retrying') \
             and (r->>'retry_at')::timestamptz <= now()) \
         order by updated_at limit 1 for update")
        .bind(class).fetch_optional(&mut *tx).await?;
    let Some((row_id, value)) = candidate else {
        tx.commit().await?;
        return Ok(None);
    };
    let mut cursor = Cursor::decode(value)?;
    if cursor.reject_claim_when_debt_full()? {
        save(&mut tx, row_id, &cursor).await?;
        tx.commit().await?;
        return Ok(None);
    }
    let desired = cursor.desired;
    let record = cursor.record_mut(desired)?;
    if now >= record.expires_at {
        record.status = PreparationStatus::Expired;
        save(&mut tx, row_id, &cursor).await?;
        tx.commit().await?;
        return Ok(None);
    }
    record.attempts += 1;
    record.status = PreparationStatus::Validating;
    record.error_code = None;
    let record = record.clone();
    let attempt_id = Uuid::now_v7();
    let key = artifact_key(&record.identity, record.id, attempt_id);
    cursor.debts.push(Debt {
        workspace_id: record.identity.workspace_id,
        library_id: record.identity.library_id,
        preparation_id: record.id,
        attempt_id,
        key,
        not_before: record.expires_at,
        cleanup: None,
    });
    cursor.active = Some(Active { preparation_id: record.id, attempt_id, heartbeat_at: now });
    save(&mut tx, row_id, &cursor).await?;
    tx.commit().await?;
    Ok(Some(Claim {
        row_id,
        record,
        attempt_id,
        output_deadline: Arc::new(Mutex::new(Some(
            request_started + std::time::Duration::from_secs(OUTPUT_PERMISSION_SECONDS),
        ))),
    }))
}

async fn lock_claim<'a>(
    tx: &mut Transaction<'a, Postgres>,
    claim: &Claim,
) -> Result<Option<Cursor>, PreparationError> {
    let value: Option<serde_json::Value> = sqlx::query_scalar(
        "select case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where id=$1 and class=$2 for update")
        .bind(claim.row_id).bind(MaintenanceClass::GraphTopologyPreparation.as_str()).fetch_optional(&mut **tx).await?;
    let Some(value) = value else {
        return Ok(None);
    };
    let cursor = Cursor::decode(value)?;
    let valid = cursor.active.as_ref().is_some_and(|active| {
        active.attempt_id == claim.attempt_id
            && active.preparation_id == claim.record.id
            && active.heartbeat_at + Duration::seconds(LEASE_SECONDS) > Utc::now()
    });
    Ok(valid.then_some(cursor))
}

pub async fn heartbeat(state: &AppState, claim: &Claim) -> Result<bool, PreparationError> {
    let request_started = Instant::now();
    let mut tx = state.persistence.heartbeat_postgres.begin().await?;
    let Some(mut cursor) = lock_claim(&mut tx, claim).await? else {
        claim.revoke_output_permission();
        return Ok(false);
    };
    if let Some(active) = &mut cursor.active {
        active.heartbeat_at = Utc::now();
    }
    save(&mut tx, claim.row_id, &cursor).await?;
    tx.commit().await?;
    claim.renew_output_permission(request_started);
    Ok(true)
}

pub async fn set_phase(
    state: &AppState,
    claim: &Claim,
    status: PreparationStatus,
) -> Result<bool, PreparationError> {
    if !matches!(
        status,
        PreparationStatus::Validating | PreparationStatus::Building | PreparationStatus::Publishing
    ) {
        return Err(anyhow!("invalid preparation phase").into());
    }
    let mut tx = state.persistence.postgres.begin().await?;
    let Some(mut cursor) = lock_claim(&mut tx, claim).await? else {
        return Ok(false);
    };
    if cursor.desired != claim.record.id {
        return Ok(false);
    }
    cursor.record_mut(claim.record.id)?.status = status;
    save(&mut tx, claim.row_id, &cursor).await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn finish_success(
    state: &AppState,
    claim: &Claim,
    artifact: ArtifactManifest,
) -> Result<bool, PreparationError> {
    claim.revoke_output_permission();
    let mut tx = state.persistence.postgres.begin().await?;
    let Some(mut cursor) = lock_claim(&mut tx, claim).await? else {
        return Ok(false);
    };
    let expected = artifact_key(&claim.record.identity, claim.record.id, claim.attempt_id);
    if artifact.storage_key != expected {
        return Err(anyhow!("artifact key does not match attempt").into());
    }
    // Lock the catalog row so source writers cannot advance its receipt between
    // the currentness check and publishing the descriptor.
    let locked: Option<Uuid> =
        sqlx::query_scalar("select id from catalog_library where id=$1 for update")
            .bind(claim.record.identity.library_id)
            .fetch_optional(&mut *tx)
            .await?;
    let identity = &claim.record.identity;
    let current = if locked.is_some() {
        sqlx::query_scalar::<_, bool>(CURRENT_SQL)
            .bind(identity.library_id)
            .bind(identity.workspace_id)
            .bind(identity.source_truth_version)
            .bind(identity.projection_version)
            .bind(identity.topology_generation)
            .fetch_one(&mut *tx)
            .await?
    } else {
        false
    };
    let now = Utc::now();
    if cursor
        .active
        .as_ref()
        .is_none_or(|active| active.heartbeat_at + Duration::seconds(LEASE_SECONDS) <= now)
    {
        return Ok(false);
    }
    if cursor.desired != claim.record.id || !current || now >= claim.record.expires_at {
        release_attempt(&mut cursor, now, "superseded")?;
        if !current {
            cursor.record_mut(claim.record.id)?.status = PreparationStatus::Superseded;
        }
        save(&mut tx, claim.row_id, &cursor).await?;
        tx.commit().await?;
        return Ok(false);
    }
    let record = cursor.record_mut(claim.record.id)?;
    record.artifact = Some(artifact);
    record.status = PreparationStatus::Ready;
    record.expires_at = now + Duration::hours(24);
    let expires_at = record.expires_at;
    for debt in &mut cursor.debts {
        if debt.attempt_id == claim.attempt_id {
            debt.not_before = expires_at;
        }
    }
    cursor.active = None;
    save(&mut tx, claim.row_id, &cursor).await?;
    tx.commit().await?;
    Ok(true)
}

pub async fn finish_error(
    state: &AppState,
    claim: &Claim,
    code: &str,
) -> Result<(), PreparationError> {
    claim.revoke_output_permission();
    let mut tx = state.persistence.postgres.begin().await?;
    let Some(mut cursor) = lock_claim(&mut tx, claim).await? else {
        return Ok(());
    };
    let code: String = code.chars().take(80).collect();
    release_attempt(&mut cursor, Utc::now(), &code)?;
    save(&mut tx, claim.row_id, &cursor).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn fail_result(
    state: &AppState,
    identity: &GraphTopologyIdentity,
    preparation_id: Uuid,
    code: &str,
) -> Result<(), PreparationError> {
    let mut tx = state.persistence.postgres.begin().await?;
    let Some((row_id, mut cursor)) = lock_row(&mut tx, identity.library_id).await? else {
        return Ok(());
    };
    let Ok(record) = cursor.record_mut(preparation_id) else {
        return Ok(());
    };
    if record.identity == *identity && record.status == PreparationStatus::Ready {
        record.status = PreparationStatus::Failed;
        record.error_code = Some(code.chars().take(80).collect());
        save(&mut tx, row_id, &cursor).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Deletes a bounded number of exact owned keys. Debt remains durable on every
/// error and its lease can be reclaimed if the deleting process crashes.
pub async fn cleanup_due(state: &AppState) -> Result<(), PreparationError> {
    for _ in 0..16 {
        let mut tx = state.persistence.postgres.begin().await?;
        let row: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
            "select id,case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where class=$1 and scope_kind='library' \
             and exists (select 1 from jsonb_array_elements(cursor_json->'debts') d \
               where (d->>'not_before')::timestamptz <= now() \
               and (d->'cleanup' = 'null'::jsonb or (d->'cleanup'->>'until')::timestamptz <= now()) \
               and (cursor_json->'active' = 'null'::jsonb or d->>'attempt_id' <> cursor_json->'active'->>'attempt_id')) \
             order by updated_at limit 1 for update skip locked")
            .bind(MaintenanceClass::GraphTopologyPreparation.as_str()).fetch_optional(&mut *tx).await?;
        let Some((row_id, value)) = row else {
            tx.commit().await?;
            break;
        };
        let mut cursor = Cursor::decode(value)?;
        let now = Utc::now();
        let active = cursor.active.as_ref().map(|a| a.attempt_id);
        let Some(debt) = cursor.debts.iter_mut().find(|d| {
            d.not_before <= now
                && Some(d.attempt_id) != active
                && d.cleanup.as_ref().is_none_or(|lease| lease.until <= now)
        }) else {
            return Err(anyhow!("invalid cleanup selection").into());
        };
        let cleanup_id = Uuid::now_v7();
        debt.cleanup = Some(CleanupLease { id: cleanup_id, until: now + Duration::minutes(2) });
        let debt = debt.clone();
        // Reject a damaged cursor rather than letting it target another key.
        let record = cursor.record(debt.preparation_id)?;
        if record.identity.workspace_id != debt.workspace_id
            || record.identity.library_id != debt.library_id
            || artifact_key(&record.identity, record.id, debt.attempt_id) != debt.key
        {
            return Err(anyhow!("invalid topology cleanup debt").into());
        }
        save(&mut tx, row_id, &cursor).await?;
        tx.commit().await?;
        let deleted = delete_debt_artifact(state, &debt).await;

        let mut tx = state.persistence.postgres.begin().await?;
        let value: Option<serde_json::Value> = sqlx::query_scalar("select case when octet_length(cursor_json::text) <= 262144 then cursor_json else 'null'::jsonb end from maintenance_job_run where id=$1 and class=$2 for update")
            .bind(row_id).bind(MaintenanceClass::GraphTopologyPreparation.as_str()).fetch_optional(&mut *tx).await?;
        if let Some(value) = value {
            let mut cursor = Cursor::decode(value)?;
            let owns_cleanup = cursor.debts.iter().any(|d| {
                d.attempt_id == debt.attempt_id
                    && d.cleanup.as_ref().is_some_and(|c| c.id == cleanup_id)
            });
            if deleted && owns_cleanup {
                expire_deleted_artifact(&mut cursor, &debt);
                cursor.debts.retain(|d| {
                    !(d.attempt_id == debt.attempt_id
                        && d.cleanup.as_ref().is_some_and(|c| c.id == cleanup_id))
                });
                let desired = cursor.desired;
                let active = cursor.active.as_ref().map(|a| a.preparation_id);
                let debt_ids: Vec<Uuid> = cursor.debts.iter().map(|d| d.preparation_id).collect();
                cursor.records.retain(|r| {
                    r.id == desired
                        || Some(r.id) == active
                        || debt_ids.contains(&r.id)
                        || r.expires_at > Utc::now()
                });
            } else if let Some(debt) = cursor.debts.iter_mut().find(|d| {
                d.attempt_id == debt.attempt_id
                    && d.cleanup.as_ref().is_some_and(|c| c.id == cleanup_id)
            }) {
                debt.cleanup = None;
                debt.not_before = Utc::now() + Duration::minutes(1);
            }
            save(&mut tx, row_id, &cursor).await?;
        }
        tx.commit().await?;
        if !deleted {
            break;
        }
    }
    Ok(())
}

async fn delete_debt_artifact(state: &AppState, debt: &Debt) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            state.content_storage.delete_graph_topology_artifact(
                debt.workspace_id,
                debt.library_id,
                &debt.key
            )
        )
        .await,
        Ok(Ok(_))
    )
}

fn expire_deleted_artifact(cursor: &mut Cursor, debt: &Debt) {
    if let Ok(record) = cursor.record_mut(debt.preparation_id)
        && record.artifact.as_ref().is_some_and(|artifact| artifact.storage_key == debt.key)
    {
        record.artifact = None;
        if record.status == PreparationStatus::Ready {
            record.status = PreparationStatus::Expired;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Cursor, DateTime<Utc>) {
        let now = Utc::now();
        let identity = GraphTopologyIdentity {
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            projection_version: 1,
            topology_generation: 0,
            source_truth_version: 1,
        };
        let record = PreparationRecord {
            id: Uuid::now_v7(),
            identity,
            artifact: None,
            status: PreparationStatus::Building,
            attempts: 1,
            retry_at: now,
            expires_at: now + Duration::hours(24),
            error_code: None,
        };
        let attempt_id = Uuid::now_v7();
        let cursor = Cursor {
            version: 1,
            desired: record.id,
            active: Some(Active { preparation_id: record.id, attempt_id, heartbeat_at: now }),
            debts: vec![Debt {
                workspace_id: identity.workspace_id,
                library_id: identity.library_id,
                preparation_id: record.id,
                attempt_id,
                key: artifact_key(&identity, record.id, attempt_id),
                not_before: record.expires_at,
                cleanup: None,
            }],
            records: vec![record],
        };
        (cursor, now)
    }

    #[test]
    fn response_poll_interval_tracks_active_and_terminal_states() {
        let (cursor, now) = fixture();
        let mut record = cursor.records[0].clone();
        record.retry_at = now - Duration::seconds(10);
        for status in [
            PreparationStatus::Queued,
            PreparationStatus::Validating,
            PreparationStatus::Building,
            PreparationStatus::Publishing,
            PreparationStatus::Retrying,
        ] {
            record.status = status;
            assert_eq!(response(&record, true, now).retry_after_ms, POLL_INTERVAL_MS);
        }
        record.status = PreparationStatus::Retrying;
        record.retry_at = now + Duration::seconds(60);
        assert_eq!(response(&record, true, now).retry_after_ms, 60_000);
        for status in [
            PreparationStatus::Ready,
            PreparationStatus::Failed,
            PreparationStatus::Superseded,
            PreparationStatus::Expired,
        ] {
            record.status = status;
            assert_eq!(response(&record, true, now).retry_after_ms, 0);
        }
        record.status = PreparationStatus::Building;
        assert_eq!(response(&record, false, now).retry_after_ms, 0);
        assert_eq!(response(&record, true, record.expires_at).retry_after_ms, 0);
    }

    #[test]
    fn expired_attempt_retries_then_exhausts_without_losing_debt() {
        let (mut cursor, now) = fixture();
        release_attempt(&mut cursor, now, "lease_expired").unwrap();
        let record = cursor.record(cursor.desired).unwrap();
        assert_eq!(record.status, PreparationStatus::Retrying);
        assert_eq!(record.retry_at, now + Duration::minutes(1));
        assert_eq!(cursor.debts.len(), 1);
        assert_eq!(cursor.debts[0].not_before, now + Duration::seconds(UPLOAD_GRACE_SECONDS));
        let id = cursor.desired;
        cursor.record_mut(id).unwrap().attempts = 3;
        cursor.active =
            Some(Active { preparation_id: id, attempt_id: Uuid::now_v7(), heartbeat_at: now });
        release_attempt(&mut cursor, now, "lease_expired").unwrap();
        assert_eq!(cursor.record(id).unwrap().status, PreparationStatus::Failed);
    }

    #[test]
    fn successor_does_not_erase_active_attempt_or_cleanup_debt() {
        let (mut cursor, now) = fixture();
        let original = cursor.desired;
        let active = cursor.active.clone().unwrap();
        let mut successor = cursor.record(original).unwrap().clone();
        successor.id = Uuid::now_v7();
        successor.identity.source_truth_version += 1;
        successor.status = PreparationStatus::Queued;
        successor.attempts = 0;
        cursor.desired = successor.id;
        cursor.records.push(successor);
        assert_eq!(cursor.active.as_ref().unwrap().attempt_id, active.attempt_id);
        release_attempt(&mut cursor, now, "lease_expired").unwrap();
        assert_eq!(cursor.record(original).unwrap().status, PreparationStatus::Superseded);
        assert_eq!(cursor.record(cursor.desired).unwrap().status, PreparationStatus::Queued);
        assert_eq!(cursor.debts.len(), 1);
    }

    #[test]
    fn corrupt_cursor_cannot_be_reset_to_empty() {
        assert!(Cursor::decode(serde_json::json!({})).is_err());
    }

    #[test]
    fn superseded_history_is_bounded_without_losing_active_or_owned_debt() {
        let (mut cursor, _) = fixture();
        let active_id = cursor.desired;
        for _ in 0..200 {
            let mut next = cursor.record(cursor.desired).unwrap().clone();
            next.id = Uuid::now_v7();
            next.status = PreparationStatus::Queued;
            next.attempts = 0;
            cursor.prune_history();
            cursor.desired = next.id;
            cursor.records.push(next);
        }
        cursor.prune_history();
        assert!(cursor.records.len() <= TERMINAL_HISTORY + 2);
        assert!(cursor.record(active_id).is_ok());
        assert_eq!(cursor.debts.len(), 1);
        assert!(Cursor::decode(serde_json::to_value(&cursor).unwrap()).is_ok());
    }

    #[test]
    fn full_debt_fails_claim_without_discarding_keys() {
        let (mut cursor, _) = fixture();
        let original = cursor.debts[0].clone();
        while cursor.debts.len() < MAX_DEBTS {
            let mut debt = original.clone();
            debt.attempt_id = Uuid::now_v7();
            debt.key =
                artifact_key(&cursor.records[0].identity, debt.preparation_id, debt.attempt_id);
            cursor.debts.push(debt);
        }
        assert!(cursor.reject_claim_when_debt_full().unwrap());
        assert_eq!(cursor.record(cursor.desired).unwrap().status, PreparationStatus::Failed);
        assert_eq!(cursor.debts.len(), MAX_DEBTS);
        assert_eq!(cursor.debts[0].key, original.key);
        assert!(Cursor::decode(serde_json::to_value(&cursor).unwrap()).is_ok());
        cursor.debts.push(original);
        assert!(Cursor::decode(serde_json::to_value(&cursor).unwrap()).is_err());
    }

    #[test]
    fn output_permission_is_shared_and_revocation_cannot_be_renewed() {
        let (cursor, _) = fixture();
        let claim = Claim {
            row_id: Uuid::now_v7(),
            record: cursor.records[0].clone(),
            attempt_id: cursor.active.unwrap().attempt_id,
            output_deadline: Arc::new(Mutex::new(Some(
                Instant::now() + std::time::Duration::from_secs(1),
            ))),
        };
        let renderer_claim = claim.clone();
        assert!(renderer_claim.output_is_permitted());
        claim.revoke_output_permission();
        claim.renew_output_permission(Instant::now());
        assert!(!renderer_claim.output_is_permitted());
    }

    #[test]
    fn output_permission_expires_without_a_successful_heartbeat() {
        let (cursor, _) = fixture();
        let claim = Claim {
            row_id: Uuid::now_v7(),
            record: cursor.records[0].clone(),
            attempt_id: cursor.active.unwrap().attempt_id,
            output_deadline: Arc::new(Mutex::new(Some(
                Instant::now() - std::time::Duration::from_secs(1),
            ))),
        };
        assert!(!claim.output_is_permitted());
    }

    #[test]
    fn oversized_cursor_is_rejected() {
        let (cursor, _) = fixture();
        let mut value = serde_json::to_value(cursor).unwrap();
        value["records"][0]["error_code"] = serde_json::Value::String("x".repeat(MAX_CURSOR_BYTES));
        assert!(Cursor::decode(value).is_err());
    }
}
