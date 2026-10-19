//! Bounded periodic sampler for non-terminal records (IRG-204).
//!
//! Every table listed here has a state machine whose non-terminal rows are
//! expected to leave those states within a bounded time. A growing backlog is
//! otherwise invisible until an operator reads the queue; the gauge turns it
//! into a time series, bucketed by the age of the oldest record in each
//! `(table, state)` group so a stale record cannot hide behind a fresh one.
//!
//! The sample is deliberately coarse and bounded: one aggregate query, one row
//! per `(table, state, age_bucket)`, no identifiers as labels. It runs only in
//! the maintenance-scheduler role and at most once per
//! [`NONTERMINAL_RECORD_SAMPLE_INTERVAL`].

use std::time::Duration;

use sqlx::PgPool;
use tracing::warn;

use crate::observability::metrics::{AgeBucket, record_nonterminal_records};

/// Minimum cadence between two samples. The scheduler samples only when a tick
/// is at least this old, so a fast tick interval cannot inflate the gauge.
pub(super) const NONTERMINAL_RECORD_SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// One aggregated non-terminal group: table, state, age bucket and count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NonterminalSample {
    pub table: &'static str,
    pub state: String,
    pub age_bucket: AgeBucket,
    pub count: u64,
}

/// One row per `(table, state, age_bucket)` over every tracked state machine.
/// The table names are literals and only bounded enum/state values appear in
/// the output, so the exported label set cannot grow with the data.
///
/// `$1` is the class interval in seconds — the cooldown on the changed-library
/// arm of the shared lease gate. `maintenance_job_run` is the only table that
/// uses it: the counted pending set is exactly the lease's own pending
/// predicate, the gate alone, so a pending row that is due but withheld by an
/// unchanged library is not reported as work, and a row the gate would lease is
/// reported as work the moment it becomes eligible. `leased` rows are always
/// work in progress and stay counted.
pub(super) fn nonterminal_records_sql() -> &'static str {
    static SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        format!(
            "\
select 'content_mutation' as table_name, mutation_state::text as state, \
       case when now() - requested_at < interval '5 minutes' then '<5m' \
            when now() - requested_at < interval '1 hour' then '<1h' \
            when now() - requested_at < interval '24 hours' then '<24h' \
            else '>=24h' end as age_bucket, \
       count(*)::bigint as record_count \
  from content_mutation where mutation_state in ('accepted','running') group by 2, 3 \
union all \
select 'ops_async_operation', status::text, \
       case when now() - created_at < interval '5 minutes' then '<5m' \
            when now() - created_at < interval '1 hour' then '<1h' \
            when now() - created_at < interval '24 hours' then '<24h' \
            else '>=24h' end, \
       count(*)::bigint \
  from ops_async_operation where status in ('accepted','processing') group by 2, 3 \
union all \
select 'ingest_job', queue_state::text, \
       case when now() - coalesce(queue_leased_at, queued_at) < interval '5 minutes' then '<5m' \
            when now() - coalesce(queue_leased_at, queued_at) < interval '1 hour' then '<1h' \
            when now() - coalesce(queue_leased_at, queued_at) < interval '24 hours' then '<24h' \
            else '>=24h' end, \
       count(*)::bigint \
  from ingest_job where queue_state in ('queued','leased','paused') group by 2, 3 \
union all \
select 'ingest_attempt', attempt_state::text, \
       case when now() - started_at < interval '5 minutes' then '<5m' \
            when now() - started_at < interval '1 hour' then '<1h' \
            when now() - started_at < interval '24 hours' then '<24h' \
            else '>=24h' end, \
       count(*)::bigint \
  from ingest_attempt where attempt_state in ('leased','running') group by 2, 3 \
union all \
select 'runtime_execution', lifecycle_state::text, \
       case when now() - accepted_at < interval '5 minutes' then '<5m' \
            when now() - accepted_at < interval '1 hour' then '<1h' \
            when now() - accepted_at < interval '24 hours' then '<24h' \
            else '>=24h' end, \
       count(*)::bigint \
  from runtime_execution where lifecycle_state in ('accepted','running') group by 2, 3 \
union all \
select 'runtime_graph_extraction', status, \
       case when now() - created_at < interval '5 minutes' then '<5m' \
            when now() - created_at < interval '1 hour' then '<1h' \
            when now() - created_at < interval '24 hours' then '<24h' \
            else '>=24h' end, \
       count(*)::bigint \
  from runtime_graph_extraction where status = 'processing' group by 2, 3 \
union all \
select 'maintenance_job_run', state::text, \
       case when now() - coalesce(heartbeat_at, updated_at, created_at) < interval '5 minutes' then '<5m' \
            when now() - coalesce(heartbeat_at, updated_at, created_at) < interval '1 hour' then '<1h' \
            when now() - coalesce(heartbeat_at, updated_at, created_at) < interval '24 hours' then '<24h' \
            else '>=24h' end, \
       count(*) filter (where state = 'leased' or {gate})::bigint \
  from maintenance_job_run where state in ('pending','leased') group by 2, 3",
            gate = crate::services::maintenance::lease::gate_eligible_predicate("$1")
        )
    });
    &SQL
}

/// The bounded `(table, state)` set the sampler exports. It mirrors the state
/// literals in [`nonterminal_records_sql`]; the `tracked_groups_match_sql` test
/// fails when the two diverge. Every pair is crossed with every
/// [`AgeBucket::ALL`] value on each sample, so a group that disappears from the
/// query result is exported as `0` instead of keeping its last non-zero value
/// forever (IRG-204).
const TRACKED_GROUPS: &[(&str, &str)] = &[
    ("content_mutation", "accepted"),
    ("content_mutation", "running"),
    ("ops_async_operation", "accepted"),
    ("ops_async_operation", "processing"),
    ("ingest_job", "queued"),
    ("ingest_job", "leased"),
    ("ingest_job", "paused"),
    ("ingest_attempt", "leased"),
    ("ingest_attempt", "running"),
    ("runtime_execution", "accepted"),
    ("runtime_execution", "running"),
    ("runtime_graph_extraction", "processing"),
    ("maintenance_job_run", "pending"),
    ("maintenance_job_run", "leased"),
];

fn table_name(value: &str) -> Option<&'static str> {
    match value {
        "content_mutation" => Some("content_mutation"),
        "ops_async_operation" => Some("ops_async_operation"),
        "ingest_job" => Some("ingest_job"),
        "ingest_attempt" => Some("ingest_attempt"),
        "runtime_execution" => Some("runtime_execution"),
        "runtime_graph_extraction" => Some("runtime_graph_extraction"),
        "maintenance_job_run" => Some("maintenance_job_run"),
        _ => None,
    }
}

fn age_bucket_from_label(label: &str) -> Option<AgeBucket> {
    match label {
        "<5m" => Some(AgeBucket::Under5Minutes),
        "<1h" => Some(AgeBucket::Under1Hour),
        "<24h" => Some(AgeBucket::Under24Hours),
        ">=24h" => Some(AgeBucket::AtLeast24Hours),
        _ => None,
    }
}

/// Runs the bounded aggregate and returns one sample per group. Unknown table or
/// bucket labels are dropped rather than exported, keeping the label set closed.
/// `class_interval` is bound as `$1` for the changed-library cooldown in the
/// `maintenance_job_run` gate.
pub(super) async fn collect_nonterminal_samples(
    pool: &PgPool,
    class_interval: Duration,
) -> Result<Vec<NonterminalSample>, sqlx::Error> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(nonterminal_records_sql())
        .bind(class_interval.as_secs() as f64)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(table, state, bucket, count)| {
            let table = table_name(&table)?;
            let age_bucket = age_bucket_from_label(&bucket)?;
            let count = u64::try_from(count).ok()?;
            Some(NonterminalSample { table, state, age_bucket, count })
        })
        .collect())
}

/// Expands the present-only sample set into the full bounded cross product of
/// [`TRACKED_GROUPS`] and [`AgeBucket::ALL`], in deterministic order. A group
/// absent from `samples` is emitted with `count = 0`, so a group that vanished
/// is overwritten with zero rather than keeping its last reported value.
fn expand_samples(samples: &[NonterminalSample]) -> Vec<NonterminalSample> {
    let mut expanded = Vec::with_capacity(TRACKED_GROUPS.len() * AgeBucket::ALL.len());
    for &(table, state) in TRACKED_GROUPS {
        for age_bucket in AgeBucket::ALL {
            let count = samples
                .iter()
                .find(|sample| {
                    sample.table == table
                        && sample.state == state
                        && sample.age_bucket == age_bucket
                })
                .map_or(0, |sample| sample.count);
            expanded.push(NonterminalSample { table, state: state.to_owned(), age_bucket, count });
        }
    }
    expanded
}

fn publish(samples: &[NonterminalSample]) {
    for sample in expand_samples(samples) {
        record_nonterminal_records(sample.table, &sample.state, sample.age_bucket, sample.count);
    }
}

/// Samples once and publishes every group. A query failure is logged and the
/// next scheduled sample retries; it must never stop the scheduler tick.
pub(super) async fn sample_and_publish(
    state: &crate::app::state::AppState,
    class_interval: Duration,
) {
    match collect_nonterminal_samples(&state.persistence.postgres, class_interval).await {
        Ok(samples) => publish(&samples),
        Err(error) => {
            warn!(?error, "non-terminal record sampler failed; continuing");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_interval_is_at_least_sixty_seconds() {
        assert!(NONTERMINAL_RECORD_SAMPLE_INTERVAL >= Duration::from_secs(60));
    }

    #[test]
    fn age_bucket_labels_map_to_every_bucket() {
        assert_eq!(age_bucket_from_label("<5m"), Some(AgeBucket::Under5Minutes));
        assert_eq!(age_bucket_from_label("<1h"), Some(AgeBucket::Under1Hour));
        assert_eq!(age_bucket_from_label("<24h"), Some(AgeBucket::Under24Hours));
        assert_eq!(age_bucket_from_label(">=24h"), Some(AgeBucket::AtLeast24Hours));
        assert_eq!(age_bucket_from_label("weird"), None);
    }

    #[test]
    fn every_tracked_table_is_queried_and_closed() {
        for table in [
            "content_mutation",
            "ops_async_operation",
            "ingest_job",
            "ingest_attempt",
            "runtime_execution",
            "runtime_graph_extraction",
            "maintenance_job_run",
        ] {
            assert!(nonterminal_records_sql().contains(table), "sampler must cover {table}");
            assert_eq!(table_name(table), Some(table));
        }
        assert_eq!(table_name("some_other_table"), None);
    }

    fn sample(
        table: &'static str,
        state: &str,
        age_bucket: AgeBucket,
        count: u64,
    ) -> NonterminalSample {
        NonterminalSample { table, state: state.to_owned(), age_bucket, count }
    }

    fn count_of(
        expanded: &[NonterminalSample],
        table: &str,
        state: &str,
        age_bucket: AgeBucket,
    ) -> Option<u64> {
        expanded
            .iter()
            .find(|sample| {
                sample.table == table && sample.state == state && sample.age_bucket == age_bucket
            })
            .map(|sample| sample.count)
    }

    #[test]
    fn present_group_is_reported_and_absent_groups_are_zeroed() {
        let samples = vec![sample("ingest_job", "queued", AgeBucket::Under5Minutes, 4)];
        let expanded = expand_samples(&samples);

        assert_eq!(
            expanded.len(),
            TRACKED_GROUPS.len() * AgeBucket::ALL.len(),
            "expansion must be the full tracked cross product"
        );
        assert_eq!(expanded.len(), 56);
        assert_eq!(count_of(&expanded, "ingest_job", "queued", AgeBucket::Under5Minutes), Some(4));
        assert_eq!(count_of(&expanded, "ingest_job", "queued", AgeBucket::AtLeast24Hours), Some(0));
        assert_eq!(
            count_of(&expanded, "content_mutation", "accepted", AgeBucket::Under5Minutes),
            Some(0)
        );
    }

    #[test]
    fn disappeared_group_is_zeroed_on_next_sample() {
        let present =
            expand_samples(&[sample("ingest_job", "queued", AgeBucket::Under5Minutes, 4)]);
        assert_eq!(count_of(&present, "ingest_job", "queued", AgeBucket::Under5Minutes), Some(4));

        let next = expand_samples(&[]);
        assert_eq!(
            count_of(&next, "ingest_job", "queued", AgeBucket::Under5Minutes),
            Some(0),
            "a group absent from the query result must be re-reported as zero"
        );
    }

    #[test]
    fn tracked_groups_match_sql() {
        use std::collections::{BTreeMap, BTreeSet};

        let pattern =
            regex::Regex::new(r"from\s+(\w+)\s+where\s+\w+\s+(?:in\s*\(([^)]*)\)|=\s*'([^']*)')")
                .unwrap();

        let mut sql_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for capture in pattern.captures_iter(nonterminal_records_sql()) {
            let table = capture.get(1).unwrap().as_str().to_owned();
            let states: Vec<String> = match capture.get(2) {
                Some(list) => list
                    .as_str()
                    .split(',')
                    .map(|state| state.trim().trim_matches('\'').to_owned())
                    .collect(),
                None => vec![capture.get(3).unwrap().as_str().to_owned()],
            };
            for state in states {
                sql_groups.entry(table.clone()).or_default().insert(state);
            }
        }

        let mut rust_groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for &(table, state) in TRACKED_GROUPS {
            rust_groups.entry(table.to_owned()).or_default().insert(state.to_owned());
        }

        assert_eq!(
            sql_groups, rust_groups,
            "TRACKED_GROUPS must mirror the per-table states in nonterminal_records_sql()"
        );

        let sql_union: BTreeSet<(String, String)> = sql_groups
            .iter()
            .flat_map(|(table, states)| {
                states.iter().map(move |state| (table.clone(), state.clone()))
            })
            .collect();
        let rust_union: BTreeSet<(String, String)> = rust_groups
            .iter()
            .flat_map(|(table, states)| {
                states.iter().map(move |state| (table.clone(), state.clone()))
            })
            .collect();
        assert_eq!(sql_union, rust_union, "the tracked state union must match the SQL union");
    }
}
