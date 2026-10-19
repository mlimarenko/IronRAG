// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

use super::*;
use maintenance_retention as retention;

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
pub(super) async fn run_retention(pool: &PgPool, command: RetentionCommand) -> Result<()> {
    match command {
        RetentionCommand::StageEvents { older_than_days, json } => {
            let older_than = Duration::from_secs(older_than_days.saturating_mul(86_400));
            let report = retention::stage_events(pool, older_than).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "retention.stage-events  rows_removed={}  batches={}  older_than_days={}",
                    report.rows_removed, report.batches, older_than_days,
                );
            }
        }
    }
    Ok(())
}
