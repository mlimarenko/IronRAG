use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;

const BACKEND_LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn wait_for_backend_lock(
    pool: &PgPool,
    pid: i32,
    query_fragment: &str,
) -> Result<()> {
    tokio::time::timeout(BACKEND_LOCK_WAIT_TIMEOUT, async {
        loop {
            let activity = sqlx::query_as::<_, (Option<String>, String)>(
                "select wait_event_type, query
                 from pg_stat_activity
                 where pid = $1",
            )
            .bind(pid)
            .fetch_optional(pool)
            .await?;
            if activity.as_ref().is_some_and(|(wait_event_type, query)| {
                wait_event_type.as_deref() == Some("Lock") && query.contains(query_fragment)
            }) {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .with_context(|| {
        format!("backend {pid} did not block in expected query fragment {query_fragment:?}")
    })??;
    Ok(())
}
