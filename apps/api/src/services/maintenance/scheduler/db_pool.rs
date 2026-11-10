//! Bounded USE sample for the process PostgreSQL pools (IRG-344).
//!
//! sqlx exposes live pool counters without issuing a query. The maintenance
//! scheduler publishes them on its sample cadence so pool saturation becomes
//! visible before a caller waits on `acquire_timeout`. Labels stay bounded to
//! the pool identity (`main`, `heartbeat`, `scheduler`) and connection state
//! (`active`, `idle`, `max`); no connection, database or user identity is
//! exported.

use crate::infra::persistence::Persistence;
use crate::observability::metrics::record_db_pool_connections;

pub(super) fn publish(persistence: &Persistence) {
    let pools = [
        ("main", &persistence.postgres),
        ("heartbeat", &persistence.heartbeat_postgres),
        ("scheduler", &persistence.scheduler_postgres),
    ];
    for (label, pool) in pools {
        let size = pool.size();
        let idle = u32::try_from(pool.num_idle()).unwrap_or(u32::MAX);
        let active = size.saturating_sub(idle);
        record_db_pool_connections(label, "max", u64::from(pool.options().get_max_connections()));
        record_db_pool_connections(label, "active", u64::from(active));
        record_db_pool_connections(label, "idle", u64::from(idle));
    }
}
