//! Shared transaction skeleton of the additive live baseline transitions.
//!
//! `docs/ru/CLI.md` forbids a second migration file, so an
//! existing database reaches a newer baseline only through an explicit operator
//! command that applies additive DDL and then compare-and-sets the recorded
//! baseline checksum. Every such command performs the same steps — bound the
//! lock wait, take one advisory lock, read the recorded checksum, resolve it
//! against the revision chain derived from the immutable specs, at the exact
//! predecessor validate the object groups its predecessor revision declares,
//! apply the delta, validate the object groups its target revision declares,
//! and only then stamp — so they live here once. A delta is an immutable
//! per-hop specification: it supplies its DDL, its exact predecessor revision,
//! its frozen target revision, the object groups its predecessor and its target
//! declare, and the predicates that describe its precondition and its
//! already-applied state.
//!
//! The recorded checksum is resolved against the chain derived from the spec
//! registry, never against the shipped baseline file:
//!
//! - the delta's predecessor — apply the delta and stamp its target revision;
//! - the delta's target or any later chain revision — validate the recorded
//!   revision's own object groups and report `already_current`, without
//!   stamping;
//! - anything else — refuse.
//!
//! Dry-run and apply agree: both evaluate the precondition, the revision
//! position, the predecessor revision's prerequisite object groups, and the
//! already-applied predicate, so dry-run never reports pending for a state
//! apply would reject. Only the predecessor can stamp, and the stamp is gated
//! by validating the target revision's own object groups.

#[cfg(test)]
use sha2::{Digest as _, Sha384};
use sqlx::{PgPool, Row};
use std::{future::Future, pin::Pin};

use crate::infra::persistence::validate_baseline_object_groups_on;

#[cfg(test)]
pub(crate) const BASELINE_SQL: &str = include_str!("../../../migrations/0001_init.sql");

/// Test-only digest of the shipped baseline. Production derives nothing from
/// the mutable file: the chain's last node is a frozen spec target.
#[cfg(test)]
pub(crate) fn shipped_baseline_sha384_hex() -> String {
    hex::encode(Sha384::digest(BASELINE_SQL.as_bytes()))
}

/// The labels of a `CREATE TYPE public.<type_name> AS ENUM (...)` declaration, in
/// declaration order. Shared by the schema-module tests so the baseline's
/// enum order is read one way, not re-implemented per delta.
#[cfg(test)]
pub(crate) fn enum_labels_in_order(sql: &str, type_name: &str) -> Vec<String> {
    let marker = format!("CREATE TYPE public.{type_name} AS ENUM (");
    let body = sql
        .split_once(marker.as_str())
        .and_then(|(_, suffix)| suffix.split_once(");"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("{type_name} must be declared as an enum"));
    body.split('\'').skip(1).step_by(2).map(str::to_string).collect()
}

/// SHA-384 of the baseline revision at IRG-75 (`b092f2f4`): the revision the
/// live database started from and the integration-provisioning predecessor.
pub(crate) const BASELINE_REVISION_IRG75_SHA384: &str = "8396274aa8c5d8af9f6d87d5622660fb69755dceb86476fc0fffab4475ae10898dc7be7df5f80a8f80739e7fb3614abe";
/// SHA-384 of the baseline revision the integration-provisioning delta
/// produces (BOT-24, `c3df7ba7`/`f7202d8b`). Frozen: the exact digest that
/// historical target revision produced.
pub(crate) const BASELINE_REVISION_PROVISIONING_SHA384: &str = "076b343d29f7938cefe595ddba393065024270f81b3908d552c2021def71e38d52ac4e95dcb53756fa9201a961767264";
/// SHA-384 of the baseline revision after the IRG-112 FK indexes
/// (`efdc413a`). Public: it is the ops-failure-disposition delta's predecessor,
/// and the integration test reproduces that exact revision. Frozen: a later
/// baseline edit appends a new revision; it never moves this one.
pub const BASELINE_REVISION_IRG112_SHA384: &str = "5ebfcb9fe4aee59941f5e28c0eddf22e63f1d93a38fc489612ed1e20cca0a90d08aebdb31f9be6880725cff7554d66cf";
/// SHA-384 of the baseline revision the IRG-173 `failure_disposition` delta
/// produces (`1308ab74`): the deployed baseline after integration provisioning
/// and the IRG-112 FK indexes, plus the nullable
/// `ops_async_operation.failure_disposition` column. It is the query-rerank
/// delta's predecessor. Frozen: the exact digest that historical revision
/// produced; a later baseline edit appends a new revision and never moves this
/// one.
pub const BASELINE_REVISION_IRG173_SHA384: &str = "c536329d4b0fdb9ea5f04b8dd548502a2a1b085de5c501926698b2e9f32334ab81d9699cddb5bee1c1c79503057837cd";
/// SHA-384 of the baseline revision the `query_rerank` purpose produces
/// (IRG-163): the merged tree's baseline, carrying both the IRG-173
/// `failure_disposition` column and the `query_rerank` enum value. Frozen: the
/// exact digest of the actual combined revision. A later baseline edit appends
/// a new revision; it never moves this one.
pub const BASELINE_REVISION_QUERY_RERANK_SHA384: &str = "ac2514b1ccab29db99df1b3227be814639eba6f4bd8d73172b496c661ba1b6664132cc6b36d79aaf1556e860654bf3c8";
/// SHA-384 of the baseline revision the decision-catalog delta produces
/// (IRG-182): the merged shipped baseline, carrying `graph_entity_type` and
/// `typesafe_system_one` on top of the IRG-163 `query_rerank` value. It is the
/// tree's current revision. Frozen: the exact digest of the actual combined
/// revision; a later baseline edit appends a new revision and never moves this
/// one.
pub const BASELINE_REVISION_DECISION_CATALOG_SHA384: &str = "57fad28db286b0031ae8266ad48cea23c3da63117f88dd47a6240ca007d5eba6da95fae03d179722c1237aa37c27519f";
/// SHA-384 of the baseline revision the admission-visibility-policy delta
/// produces (IRG-92): the shipped baseline with the
/// `public.admission_visibility_policy` enum and the
/// `content_mutation.visibility_policy` column on top of the decision-catalog
/// revision. It is the tree's current revision. Frozen: the exact digest of the
/// actual combined revision; a later baseline edit appends a new revision and
/// never moves this one.
pub const BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384: &str = "bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb";

/// SHA-384 of the content visibility intent baseline, after the frozen policy hop.
pub const BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384: &str = "47e765cbe7705219d6590fc2870971f19fc06d12f380de220ac90c6484e6ec98b78b28112a65d71fa5a370f0e1ff7c58";

/// Serialises every baseline transition in the instance. One key, not one per
/// delta: two different transitions racing on the same checksum row would each
/// read a predecessor the other is about to replace.
const BASELINE_TRANSITION_LOCK_KEY: i64 = 902_093;

/// Bounds every lock wait an operator transaction performs.
///
/// These commands run against a live instance, where waiting is not a private
/// delay: a statement parked in front of `iam_service_identity` or the
/// `_sqlx_migrations` row queues everything that arrives after it, so an
/// unbounded wait turns maintenance into an outage.
///
/// Measured in piping-eu on 2026-09-12 (BOT-24): the upgrade queued behind a
/// long-running delete, and its backend went on holding the advisory lock for
/// about 25 minutes after the pod that started it was deleted.
pub(crate) async fn bound_operator_lock_waits(
    transaction: &mut sqlx::PgConnection,
) -> anyhow::Result<()> {
    sqlx::query("set local lock_timeout = '5s'").execute(transaction).await?;
    Ok(())
}

/// Frozen object predicates shared by a recorded revision and its owning spec.
type BaselineObjectCertificate = &'static [&'static [(&'static str, &'static str)]];

/// One immutable additive baseline delta.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AdditiveBaselineDeltaSpec {
    /// User-visible operator command, e.g.
    /// `ironrag-maintenance migrate query-rerank-binding-purpose`.
    pub(crate) operator_command: &'static str,
    /// Exact SHA-384 of the revision this delta starts from.
    pub(crate) predecessor_sha384_hex: &'static str,
    /// Exact SHA-384 of the revision this delta produces.
    pub(crate) target_sha384_hex: &'static str,
    /// Object groups the target revision declares; validated before stamping.
    pub(crate) required_object_groups: BaselineObjectCertificate,
    /// Object groups the delta's predecessor revision declares: already present
    /// before the delta may run. Validated in both modes at the exact
    /// predecessor, before a dry run may report pending, so a corrupt
    /// predecessor is refused identically by dry-run and apply.
    pub(crate) prerequisite_object_groups: BaselineObjectCertificate,
    /// Additive DDL, idempotent or guarded by `delta_present`.
    pub(crate) additive_sql: &'static str,
    /// `(predicate, message)` required in both dry-run and apply mode.
    pub(crate) precondition: Option<(&'static str, &'static str)>,
    /// `(predicate, message)`; `true` with a stale checksum means the DDL is
    /// partially or fully pre-applied and must be inspected, never stamped.
    pub(crate) delta_present: (&'static str, &'static str),
    /// Refusal text when the recorded checksum is neither the predecessor nor a
    /// revision this delta already passed.
    pub(crate) predecessor_error: &'static str,
}

/// The ordered registry of additive deltas. Spec consts live in their owning
/// schema modules.
pub(crate) const BASELINE_DELTA_SPECS: &[AdditiveBaselineDeltaSpec] = &[
    crate::services::maintenance::service_identity_schema::INTEGRATION_PROVISIONING_DELTA,
    crate::services::maintenance::ops_failure_disposition_schema::OPS_FAILURE_DISPOSITION_DELTA,
    crate::services::maintenance::ai_binding_purpose_schema::QUERY_RERANK_DELTA,
    crate::services::maintenance::decision_catalog_schema::DECISION_CATALOG_DELTA,
    crate::services::maintenance::admission_visibility_policy_schema::ADMISSION_VISIBILITY_POLICY_DELTA,
    crate::services::maintenance::content_visibility_intent_schema::CONTENT_VISIBILITY_INTENT_DELTA,
];

/// Derives the ordered revision chain from the specs alone. A predecessor that
/// is not the previous spec's target becomes its own node — that is how the
/// IRG-112 revision enters between integration provisioning and `query_rerank`.
fn revision_chain_for(specs: &[AdditiveBaselineDeltaSpec]) -> Vec<&'static str> {
    let mut chain: Vec<&'static str> = Vec::new();
    for spec in specs {
        if chain.last() != Some(&spec.predecessor_sha384_hex) {
            chain.push(spec.predecessor_sha384_hex);
        }
        chain.push(spec.target_sha384_hex);
    }
    chain
}

/// The one ordered revision chain. Its index is the revision an additive
/// delta either starts from, targets, or has already passed.
pub(crate) fn baseline_revision_chain() -> Vec<&'static str> {
    revision_chain_for(BASELINE_DELTA_SPECS)
}

/// The registry owns each recorded revision's object certificate. Later revisions
/// may deliberately retire predecessor objects; historical commands must neither
/// resurrect those objects nor skip the recorded revision's current checks.
fn recorded_revision_object_groups(checksum: &str) -> Option<BaselineObjectCertificate> {
    BASELINE_DELTA_SPECS.iter().find_map(|spec| {
        if spec.target_sha384_hex == checksum {
            Some(spec.required_object_groups)
        } else if spec.predecessor_sha384_hex == checksum {
            // Manual chain nodes such as IRG-112 own the next spec's exact
            // prerequisite certificate rather than an automated target spec.
            Some(spec.prerequisite_object_groups)
        } else {
            None
        }
    })
}

/// Refusal text for a recorded checksum this delta can neither start from nor
/// count as passed.
fn refusal(delta: &AdditiveBaselineDeltaSpec) -> String {
    format!(
        "{}; run `{}` from its exact predecessor revision (see docs/ru/CLI.md)",
        delta.predecessor_error, delta.operator_command
    )
}

/// Result of a transition: whether the database already recorded this delta's
/// target revision (or a later one), and whether this run applied the delta and
/// stamped it.
pub(crate) struct AdditiveBaselineTransition {
    pub(crate) already_current: bool,
    pub(crate) schema_applied: bool,
}

/// Transaction-local delta input and its preservation certificate.
/// Preparation runs in dry-run too, at the exact predecessor after object gates.
pub(crate) trait BaselineDeltaSetup: Sync {
    fn prepare<'a>(
        &'a self,
        connection: &'a mut sqlx::PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

    /// Refuses loss or drift caused by the delta before any checksum stamp.
    fn verify<'a>(
        &'a self,
        connection: &'a mut sqlx::PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;
}

async fn lock_visibility_predecessor_owners(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    current_hex: &str,
    delta: &AdditiveBaselineDeltaSpec,
) -> anyhow::Result<()> {
    if current_hex != delta.predecessor_sha384_hex
        || delta.target_sha384_hex != BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384
    {
        return Ok(());
    }
    // Certify historical closure while every participating row owner is
    // quiesced. The existing operator lock_timeout bounds these table locks.
    sqlx::raw_sql("LOCK TABLE public.content_mutation, public.content_mutation_item, public.content_document_head, public.content_web_ingest_run, public.ingest_job, public.ingest_attempt, public.content_document, public.content_revision, public.knowledge_document IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut **transaction).await?;
    Ok(())
}

pub(crate) async fn apply_additive_baseline_delta(
    postgres: &PgPool,
    delta: &AdditiveBaselineDeltaSpec,
    apply: bool,
) -> anyhow::Result<AdditiveBaselineTransition> {
    apply_additive_baseline_delta_with_setup(postgres, delta, apply, None).await
}

pub(crate) async fn apply_additive_baseline_delta_with_setup(
    postgres: &PgPool,
    delta: &AdditiveBaselineDeltaSpec,
    apply: bool,
    setup: Option<&dyn BaselineDeltaSetup>,
) -> anyhow::Result<AdditiveBaselineTransition> {
    let chain = baseline_revision_chain();
    let mut transaction = postgres.begin().await?;
    bound_operator_lock_waits(&mut transaction).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "select pg_advisory_xact_lock({BASELINE_TRANSITION_LOCK_KEY})"
    )))
    .execute(&mut *transaction)
    .await?;

    let (current_checksum, current_hex) =
        validate_delta_precondition(&mut transaction, delta).await?;

    if delta_is_already_current(&chain, &current_hex, delta)? {
        let recorded_groups = recorded_revision_object_groups(&current_hex)
            .ok_or_else(|| anyhow::anyhow!("{}", refusal(delta)))?;
        validate_baseline_object_groups_on(&mut transaction, recorded_groups).await?;
        transaction.commit().await?;
        return Ok(AdditiveBaselineTransition { already_current: true, schema_applied: false });
    }

    // The exact predecessor's own revision declares the objects this delta
    // builds on. Validate them in both modes, before a dry run may report
    // pending or the DDL may run, so a predecessor missing an earlier delta's
    // object is refused identically by dry-run and apply — and never stamped.
    validate_baseline_object_groups_on(&mut transaction, delta.prerequisite_object_groups).await?;

    // A pre-applied DDL under a stale checksum is inspected, never stamped —
    // and dry-run refuses it identically to apply.
    verify_delta_absent(&mut transaction, delta).await?;

    if let Some(setup) = setup {
        setup.prepare(&mut transaction).await?;
    }

    if !apply {
        // A genuine pending state: the exact predecessor with no DDL present.
        transaction.rollback().await?;
        return Ok(AdditiveBaselineTransition { already_current: false, schema_applied: false });
    }

    apply_prepared_delta(transaction, delta, setup, &current_checksum).await
}

/// Apply and validate the prepared delta before its checksum can be committed.
async fn apply_prepared_delta(
    mut transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    delta: &AdditiveBaselineDeltaSpec,
    setup: Option<&dyn BaselineDeltaSetup>,
    current_checksum: &[u8],
) -> anyhow::Result<AdditiveBaselineTransition> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(delta.additive_sql)).execute(&mut *transaction).await?;

    if let Some(setup) = setup {
        setup.verify(&mut transaction).await?;
    }

    // This command applies one delta, not the whole chain, so it validates only
    // the groups its target revision declares before stamping that revision.
    // Demanding a later revision's objects here would make the documented
    // provisioning-then-rerank order impossible: the enum cannot exist before
    // the delta that creates it runs.
    validate_baseline_object_groups_on(&mut transaction, delta.required_object_groups).await?;

    // Compare-and-set rejects a concurrent revision change instead of
    // certifying a database that moved after the checksum was read.
    stamp_delta(&mut transaction, delta, current_checksum).await?;
    transaction.commit().await?;

    Ok(AdditiveBaselineTransition { already_current: false, schema_applied: true })
}

async fn verify_delta_absent(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    delta: &AdditiveBaselineDeltaSpec,
) -> anyhow::Result<()> {
    let delta_present: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(delta.delta_present.0))
        .fetch_one(&mut **transaction)
        .await?;
    anyhow::ensure!(!delta_present, "{}", delta.delta_present.1);
    Ok(())
}

async fn stamp_delta(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    delta: &AdditiveBaselineDeltaSpec,
    current_checksum: &[u8],
) -> anyhow::Result<()> {
    let updated = sqlx::query(
        "update _sqlx_migrations
         set checksum = $1
         where version = 1 and success = true and checksum = $2",
    )
    .bind(hex::decode(delta.target_sha384_hex)?)
    .bind(current_checksum)
    .execute(&mut **transaction)
    .await?;
    anyhow::ensure!(
        updated.rows_affected() == 1,
        "baseline checksum update did not affect version 1"
    );
    Ok(())
}

/// Read and lock the recorded state before checking its delta-specific closure.
/// Dry-run uses the same precondition as apply, never a weaker readiness claim.
async fn validate_delta_precondition(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    delta: &AdditiveBaselineDeltaSpec,
) -> anyhow::Result<(Vec<u8>, String)> {
    let history = sqlx::query("select checksum, success from _sqlx_migrations where version = 1")
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or_else(|| anyhow::anyhow!("baseline migration version 1 is absent"))?;
    anyhow::ensure!(history.get::<bool, _>("success"), "baseline migration version 1 is dirty");
    let checksum: Vec<u8> = history.get("checksum");
    let current_hex = hex::encode(&checksum);
    lock_visibility_predecessor_owners(transaction, &current_hex, delta).await?;
    if let Some((predicate, message)) = delta.precondition {
        let satisfied: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(predicate))
            .fetch_one(&mut **transaction)
            .await?;
        anyhow::ensure!(satisfied, "{message}");
    }
    Ok((checksum, current_hex))
}

/// Returns false only for this delta's exact predecessor, never an earlier hop.
fn delta_is_already_current(
    chain: &[&str],
    current: &str,
    delta: &AdditiveBaselineDeltaSpec,
) -> anyhow::Result<bool> {
    let current_index = chain
        .iter()
        .position(|revision| *revision == current)
        .ok_or_else(|| anyhow::anyhow!(refusal(delta)))?;
    let target_index = chain
        .iter()
        .position(|revision| *revision == delta.target_sha384_hex)
        .ok_or_else(|| anyhow::anyhow!(refusal(delta)))?;
    anyhow::ensure!(target_index > 0, "{}", refusal(delta));
    anyhow::ensure!(chain[target_index - 1] == delta.predecessor_sha384_hex, "{}", refusal(delta));
    if current_index >= target_index {
        return Ok(true);
    }
    anyhow::ensure!(current_index == target_index - 1, "{}", refusal(delta));
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{
        AdditiveBaselineDeltaSpec, BASELINE_DELTA_SPECS,
        BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384,
        BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384,
        BASELINE_REVISION_DECISION_CATALOG_SHA384, BASELINE_REVISION_IRG75_SHA384,
        BASELINE_REVISION_IRG112_SHA384, BASELINE_REVISION_IRG173_SHA384,
        BASELINE_REVISION_PROVISIONING_SHA384, BASELINE_REVISION_QUERY_RERANK_SHA384,
        baseline_revision_chain, recorded_revision_object_groups, revision_chain_for,
        shipped_baseline_sha384_hex,
    };
    use crate::infra::persistence::REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK;

    #[test]
    fn chain_is_derived_from_frozen_specs() {
        let chain = baseline_revision_chain();
        assert_eq!(
            chain,
            vec![
                BASELINE_REVISION_IRG75_SHA384,
                BASELINE_REVISION_PROVISIONING_SHA384,
                BASELINE_REVISION_IRG112_SHA384,
                BASELINE_REVISION_IRG173_SHA384,
                BASELINE_REVISION_QUERY_RERANK_SHA384,
                BASELINE_REVISION_DECISION_CATALOG_SHA384,
                BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384,
                BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384,
            ]
        );
        let shipped = shipped_baseline_sha384_hex();
        assert_eq!(
            chain.last().copied(),
            Some(shipped.as_str()),
            "the tree's baseline file must be the current revision",
        );
    }

    #[test]
    fn every_spec_starts_at_the_revision_before_its_target() {
        let chain = baseline_revision_chain();
        for spec in BASELINE_DELTA_SPECS {
            let target_index = chain
                .iter()
                .position(|revision| *revision == spec.target_sha384_hex)
                .expect("every spec's target must be a chain node");
            assert!(target_index > 0, "a target must have a predecessor");
            assert_eq!(
                chain[target_index - 1],
                spec.predecessor_sha384_hex,
                "the chain node before a target must be its exact predecessor",
            );
        }
    }

    #[test]
    fn each_specs_target_retains_its_prerequisites_except_the_retired_policy() {
        for spec in BASELINE_DELTA_SPECS {
            assert!(
                !spec.prerequisite_object_groups.is_empty(),
                "{}: a delta must declare the predecessor groups it builds on",
                spec.operator_command,
            );
            if spec.target_sha384_hex == BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384 {
                use crate::infra::persistence::{
                    REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY,
                    REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG,
                };
                assert_eq!(
                    spec.prerequisite_object_groups,
                    REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY
                );
                assert!(
                    spec.required_object_groups
                        .starts_with(REQUIRED_BASELINE_OBJECT_GROUPS_DECISION_CATALOG)
                );
                let retired_policy =
                    spec.prerequisite_object_groups.last().expect("certified policy group");
                assert!(!spec.required_object_groups.contains(retired_policy));
                assert!(spec.additive_sql.contains(
                    "ALTER TABLE public.content_mutation DROP COLUMN visibility_policy;"
                ));
                assert!(
                    spec.additive_sql.contains("DROP TYPE public.admission_visibility_policy;")
                );
            } else {
                assert!(
                    spec.required_object_groups.starts_with(spec.prerequisite_object_groups),
                    "{}: the predecessor's object groups must lead its target's groups",
                    spec.operator_command,
                );
            }
        }
    }

    #[test]
    fn specs_are_ordered_and_do_not_overlap() {
        let mut predecessors = Vec::new();
        let mut targets = Vec::new();
        let mut previous_target_index = None;
        let chain = baseline_revision_chain();
        for spec in BASELINE_DELTA_SPECS {
            assert_ne!(spec.predecessor_sha384_hex, spec.target_sha384_hex);
            assert!(
                !predecessors.contains(&spec.predecessor_sha384_hex),
                "predecessors must be unique",
            );
            assert!(!targets.contains(&spec.target_sha384_hex), "targets must be unique");
            predecessors.push(spec.predecessor_sha384_hex);
            targets.push(spec.target_sha384_hex);
            let target_index = chain
                .iter()
                .position(|revision| *revision == spec.target_sha384_hex)
                .expect("every spec's target must be a chain node");
            if let Some(previous) = previous_target_index {
                assert!(target_index > previous, "target chain indices must strictly increase");
            }
            previous_target_index = Some(target_index);
        }
    }

    #[test]
    fn targets_are_frozen_constants_not_derived_from_the_shipped_file() {
        for spec in BASELINE_DELTA_SPECS {
            assert_eq!(
                recorded_revision_object_groups(spec.target_sha384_hex),
                Some(spec.required_object_groups)
            );
            assert_eq!(
                recorded_revision_object_groups(spec.predecessor_sha384_hex),
                Some(spec.prerequisite_object_groups)
            );
        }
        assert!(recorded_revision_object_groups("unknown-checksum").is_none());
        let provisioning = BASELINE_DELTA_SPECS
            .iter()
            .find(|spec| spec.operator_command.contains("integration-provisioning"))
            .expect("the provisioning spec must be registered");
        assert_eq!(provisioning.target_sha384_hex, BASELINE_REVISION_PROVISIONING_SHA384);

        let failure_disposition = BASELINE_DELTA_SPECS
            .iter()
            .find(|spec| spec.operator_command.contains("ops-failure-disposition"))
            .expect("the ops-failure-disposition spec must be registered");
        assert_eq!(failure_disposition.target_sha384_hex, BASELINE_REVISION_IRG173_SHA384);
        assert_eq!(failure_disposition.predecessor_sha384_hex, BASELINE_REVISION_IRG112_SHA384);

        let query_rerank = BASELINE_DELTA_SPECS
            .iter()
            .find(|spec| spec.operator_command.contains("query-rerank"))
            .expect("the query-rerank spec must be registered");
        assert_eq!(query_rerank.target_sha384_hex, BASELINE_REVISION_QUERY_RERANK_SHA384);
        assert_eq!(query_rerank.predecessor_sha384_hex, BASELINE_REVISION_IRG173_SHA384);

        let decision_catalog = BASELINE_DELTA_SPECS
            .iter()
            .find(|spec| spec.operator_command.contains("decision-catalog"))
            .expect("the decision-catalog spec must be registered");
        assert_eq!(decision_catalog.target_sha384_hex, BASELINE_REVISION_DECISION_CATALOG_SHA384);
        assert_eq!(decision_catalog.predecessor_sha384_hex, BASELINE_REVISION_QUERY_RERANK_SHA384);
    }

    #[test]
    fn appending_a_later_spec_does_not_move_an_earlier_target() {
        const FUTURE_TARGET: &str = "000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
        const FUTURE_SPEC: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
            operator_command: "ironrag-maintenance migrate future-delta",
            predecessor_sha384_hex: BASELINE_REVISION_QUERY_RERANK_SHA384,
            target_sha384_hex: FUTURE_TARGET,
            required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK,
            prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_QUERY_RERANK,
            additive_sql: "",
            precondition: None,
            delta_present: ("select false", "unused"),
            predecessor_error: "unused",
        };

        let base_chain = baseline_revision_chain();
        let mut extended: Vec<AdditiveBaselineDeltaSpec> = BASELINE_DELTA_SPECS.to_vec();
        extended.push(FUTURE_SPEC);
        let grown_chain = revision_chain_for(&extended);

        assert_eq!(&grown_chain[..base_chain.len()], &base_chain[..]);
        for spec in BASELINE_DELTA_SPECS {
            let base_index = base_chain
                .iter()
                .position(|revision| *revision == spec.target_sha384_hex)
                .expect("base target node");
            let grown_index = grown_chain
                .iter()
                .position(|revision| *revision == spec.target_sha384_hex)
                .expect("grown target node");
            assert_eq!(base_index, grown_index, "an appended spec must not move an earlier target");
        }
    }
}
