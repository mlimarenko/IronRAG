// ---------------------------------------------------------------------------
// lease
// ---------------------------------------------------------------------------

use super::*;
use maintenance_lease as lease;

#[derive(Debug, Serialize)]
struct LeaseSummaryRow {
    class: String,
    pending: i64,
    leased: i64,
    dead_letter: i64,
}

pub(super) async fn run_lease(pool: &PgPool, command: LeaseCommand) -> Result<()> {
    match command {
        LeaseCommand::Show { class, state, limit, json } => {
            lease_commands::show(pool, class, state.as_deref(), limit, json).await
        }
        LeaseCommand::Summary { json } => lease_commands::summary(pool, json).await,
        LeaseCommand::ClearFailure { class, library } => {
            lease_commands::clear_failure(pool, class, library).await
        }
        LeaseCommand::ReapStale { stale_after_secs } => {
            lease_commands::reap_stale(pool, stale_after_secs).await
        }
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
pub(super) mod lease_commands {
    use super::*;

    pub(super) async fn show(
        pool: &PgPool,
        class: Option<MaintenanceClass>,
        state: Option<&str>,
        limit: i64,
        json: bool,
    ) -> Result<()> {
        let rows = lease_show(pool, class, state, limit).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!(
                "{:<32} {:<10} {:<38} {:<14} {:>8} {:<24} error_code",
                "class", "scope", "scope_id", "state", "attempts", "next_due_at",
            );
            for row in rows {
                println!(
                    "{:<32} {:<10} {:<38} {:<14} {:>8} {:<24} {}",
                    row.class,
                    row.scope_kind,
                    row.scope_id.map_or_else(|| "-".to_string(), |id| id.to_string()),
                    format!("{:?}", row.state),
                    row.attempts,
                    row.next_due_at.to_rfc3339(),
                    row.error_code.unwrap_or_else(|| "-".to_string()),
                );
            }
        }
        Ok(())
    }

    pub(super) async fn summary(pool: &PgPool, json: bool) -> Result<()> {
        let rows = lease_summary(pool).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            println!("{:<32} {:>10} {:>10} {:>12}", "class", "pending", "leased", "dead_letter");
            for row in rows {
                println!(
                    "{:<32} {:>10} {:>10} {:>12}",
                    row.class, row.pending, row.leased, row.dead_letter,
                );
            }
        }
        Ok(())
    }

    pub(super) async fn clear_failure(
        pool: &PgPool,
        class: MaintenanceClass,
        library: Option<Uuid>,
    ) -> Result<()> {
        let scope = scope_for_clear_failure(library);
        let cleared = lease::clear_dead_letter(pool, class, scope).await?;
        if cleared {
            println!("cleared dead-letter for {} {:?}", class.as_str(), scope);
            Ok(())
        } else {
            bail!("no dead-letter row found for {} {:?}", class.as_str(), scope);
        }
    }

    pub(crate) fn scope_for_clear_failure(library: Option<Uuid>) -> Scope {
        library.map_or(Scope::Instance, Scope::Library)
    }

    pub(super) async fn reap_stale(pool: &PgPool, stale_after_secs: u64) -> Result<()> {
        let reaped = lease::reap_stale_leases(pool, Duration::from_secs(stale_after_secs)).await?;
        println!("reaped {reaped} stale leases");
        Ok(())
    }
}

async fn lease_show(
    pool: &PgPool,
    class: Option<MaintenanceClass>,
    state: Option<&str>,
    limit: i64,
) -> Result<Vec<MaintenanceJobRun>> {
    let rows = sqlx::query_as::<_, MaintenanceJobRun>(
        "select * from maintenance_job_run \
         where ($1::text is null or class = $1) \
           and ($2::text is null or state::text = $2) \
         order by next_due_at asc \
         limit $3",
    )
    .bind(class.map(MaintenanceClass::as_str))
    .bind(state)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn lease_summary(pool: &PgPool) -> Result<Vec<LeaseSummaryRow>> {
    let mut out = Vec::new();
    for class in [MaintenanceClass::GcSupersededVectors, MaintenanceClass::GcRuntimeGraph] {
        let StateCounts { pending, leased, dead_letter } =
            lease::count_by_state(pool, class).await?;
        if pending + leased + dead_letter > 0 {
            out.push(LeaseSummaryRow {
                class: class.as_str().to_string(),
                pending,
                leased,
                dead_letter,
            });
        }
    }
    Ok(out)
}
