//! PostgreSQL integration coverage for the stranded fan-out parent repair
//! (IRG-218).
//!
//! A batch endpoint creates one parent `ops_async_operation` and fans out to
//! children linked by `parent_async_operation_id`; the parent is the only row a
//! poller watches. The detached worker that owns its terminal transition can be
//! aborted by a release (the runtime dropping it on shutdown) or a panic,
//! leaving the parent `processing` while every child has settled. Nothing else
//! transitions a `library`-subject parent, so the operator repair must derive
//! the parent's terminal state from the persisted children.
//!
//! This suite proves the repair settles exactly that cohort through the shared
//! aggregate, reports it without writing in dry-run, leaves a parent with a live
//! child (or no child) alone, and is idempotent. No provider is called.
//!
//! Run with:
//! `cargo test -p ironrag-backend --features test-support --test it -- --ignored stranded_fan_out_repair::`

use crate::content_lifecycle_support::ContentLifecycleFixture;

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use ironrag_backend::services::maintenance::repair;

const STRANDED_FAN_OUT_OPERATION_KIND: &str = "batch_delete_documents";

/// Plants a fan-out parent in `parent_status` with one child per entry of
/// `child_statuses` — the row shape a batch endpoint leaves behind.
async fn plant_fan_out(
    fixture: &ContentLifecycleFixture,
    operation_kind: &str,
    parent_status: &str,
    child_statuses: &[&str],
) -> Result<Uuid> {
    let pool = &fixture.state.persistence.postgres;
    let parent_id = Uuid::now_v7();
    sqlx::query(
        "insert into ops_async_operation (
             id, workspace_id, library_id, operation_kind, surface_kind,
             status, subject_kind, subject_id
         ) values (
             $1, $2, $3, $4, 'rest',
             $5::ops_async_operation_status, 'library', $3
         )",
    )
    .bind(parent_id)
    .bind(fixture.workspace_id)
    .bind(fixture.library_id)
    .bind(operation_kind)
    .bind(parent_status)
    .execute(pool)
    .await
    .context("failed to plant a fan-out parent")?;
    for child_status in child_statuses {
        sqlx::query(
            "insert into ops_async_operation (
                 id, workspace_id, library_id, operation_kind, surface_kind,
                 status, subject_kind, subject_id, parent_async_operation_id
             ) values (
                 $1, $2, $3, 'delete', 'rest',
                 $4::ops_async_operation_status, 'document', $5, $6
             )",
        )
        .bind(Uuid::now_v7())
        .bind(fixture.workspace_id)
        .bind(fixture.library_id)
        .bind(child_status)
        .bind(Uuid::now_v7())
        .bind(parent_id)
        .execute(pool)
        .await
        .context("failed to plant a fan-out child")?;
    }
    Ok(parent_id)
}

async fn operation_status_and_code(
    pool: &PgPool,
    operation_id: Uuid,
) -> Result<(String, Option<String>)> {
    sqlx::query_as::<_, (String, Option<String>)>(
        "select status::text, failure_code from ops_async_operation where id = $1",
    )
    .bind(operation_id)
    .fetch_one(pool)
    .await
    .context("failed to read the fan-out parent state")
}

/// Reads a parent's status, failure code and `result_json` detail in one go.
async fn operation_state(
    pool: &PgPool,
    operation_id: Uuid,
) -> Result<(String, Option<String>, Option<serde_json::Value>)> {
    sqlx::query_as::<_, (String, Option<String>, Option<serde_json::Value>)>(
        "select status::text, failure_code, result_json from ops_async_operation where id = $1",
    )
    .bind(operation_id)
    .fetch_one(pool)
    .await
    .context("failed to read the fan-out parent state")
}

/// Reproduces the pre-IRG-218 release: a parent whose background graph step was
/// wrongly written as the operation's failure while its children succeeded.
async fn mark_parent_failed_with(
    fixture: &ContentLifecycleFixture,
    parent_id: Uuid,
    failure_code: &str,
) -> Result<()> {
    sqlx::query(
        "update ops_async_operation
            set status = 'failed'::ops_async_operation_status, failure_code = $2
          where id = $1",
    )
    .bind(parent_id)
    .bind(failure_code)
    .execute(&fixture.state.persistence.postgres)
    .await
    .context("failed to mark the fan-out parent failed")?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn settles_parent_when_every_child_is_terminal() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id = plant_fan_out(
            &fixture,
            STRANDED_FAN_OUT_OPERATION_KIND,
            "processing",
            &["ready", "failed"],
        )
        .await?;

        let report = repair::reconcile_stranded_fan_out_operations(&fixture.state, false).await?;

        assert_eq!(report.candidates, 1, "the terminal fan-out must be a candidate");
        assert_eq!(report.settled, 1);
        assert_eq!(report.by_kind.len(), 1);
        assert_eq!(report.by_kind[0].operation_kind, STRANDED_FAN_OUT_OPERATION_KIND);
        assert_eq!(report.by_kind[0].operations, 1);
        let (status, failure_code) =
            operation_status_and_code(&fixture.state.persistence.postgres, parent_id).await?;
        assert_eq!(status, "failed", "a fan-out with a failed child settles failed");
        assert_eq!(
            failure_code.as_deref(),
            Some("children_failed:1/2"),
            "the aggregate names the failing child count"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn settles_ready_parent_when_every_child_is_ready() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id = plant_fan_out(
            &fixture,
            STRANDED_FAN_OUT_OPERATION_KIND,
            "processing",
            &["ready", "ready"],
        )
        .await?;

        let report = repair::reconcile_stranded_fan_out_operations(&fixture.state, false).await?;

        assert_eq!(report.settled, 1);
        let (status, failure_code) =
            operation_status_and_code(&fixture.state.persistence.postgres, parent_id).await?;
        assert_eq!(status, "ready", "a fan-out with only successful children settles ready");
        assert_eq!(failure_code, None, "a ready parent carries no failure code");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn dry_run_reports_without_writing() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id =
            plant_fan_out(&fixture, STRANDED_FAN_OUT_OPERATION_KIND, "processing", &["ready"])
                .await?;

        let report = repair::reconcile_stranded_fan_out_operations(&fixture.state, true).await?;

        assert_eq!(report.candidates, 1, "dry run must report the cohort");
        assert_eq!(report.settled, 0, "dry run must not write");
        assert!(report.dry_run);
        assert_eq!(
            operation_status_and_code(&fixture.state.persistence.postgres, parent_id).await?.0,
            "processing",
            "dry run must leave the parent stranded"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn leaves_parent_with_in_flight_child_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id = plant_fan_out(
            &fixture,
            STRANDED_FAN_OUT_OPERATION_KIND,
            "processing",
            &["ready", "processing"],
        )
        .await?;

        let report = repair::reconcile_stranded_fan_out_operations(&fixture.state, false).await?;

        assert_eq!(report.candidates, 0, "a live child is not the stranded cohort");
        assert_eq!(report.settled, 0);
        assert_eq!(
            operation_status_and_code(&fixture.state.persistence.postgres, parent_id).await?.0,
            "processing"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn leaves_parent_without_children_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id =
            plant_fan_out(&fixture, STRANDED_FAN_OUT_OPERATION_KIND, "processing", &[]).await?;

        let report = repair::reconcile_stranded_fan_out_operations(&fixture.state, false).await?;

        assert_eq!(report.candidates, 0, "a childless parent has no aggregate to derive");
        assert_eq!(report.settled, 0);
        assert_eq!(
            operation_status_and_code(&fixture.state.persistence.postgres, parent_id).await?.0,
            "processing"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn second_run_is_idempotent() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id =
            plant_fan_out(&fixture, STRANDED_FAN_OUT_OPERATION_KIND, "processing", &["ready"])
                .await?;

        let first = repair::reconcile_stranded_fan_out_operations(&fixture.state, false).await?;
        let second = repair::reconcile_stranded_fan_out_operations(&fixture.state, false).await?;

        assert_eq!(first.settled, 1);
        assert_eq!(second.settled, 0, "the settled parent must not be a candidate again");
        assert_eq!(
            operation_status_and_code(&fixture.state.persistence.postgres, parent_id).await?.0,
            "ready"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn reconciles_contradicted_outcome_parent_when_children_are_ready() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id = plant_fan_out(
            &fixture,
            STRANDED_FAN_OUT_OPERATION_KIND,
            "processing",
            &["ready", "ready"],
        )
        .await?;
        mark_parent_failed_with(&fixture, parent_id, "graph_projection_failed").await?;

        let report = repair::reconcile_contradicted_fan_out_outcomes(&fixture.state, false).await?;

        assert_eq!(report.candidates, 1, "the contradicted parent must be a candidate");
        assert_eq!(report.settled, 1);
        let (status, failure_code, detail) =
            operation_state(&fixture.state.persistence.postgres, parent_id).await?;
        assert_eq!(status, "ready", "the parent's outcome is its children's");
        assert_eq!(failure_code, None, "the background failure is not the parent's outcome");
        assert_eq!(
            detail
                .as_ref()
                .and_then(|value| value.pointer("/graph_convergence/failure_code"))
                .and_then(|value| value.as_str()),
            Some("graph_projection_failed"),
            "the background failure is kept as a detail",
        );

        let second = repair::reconcile_contradicted_fan_out_outcomes(&fixture.state, false).await?;
        assert_eq!(second.settled, 0, "the reconciled parent must not be a candidate again");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn dry_run_reports_contradicted_outcome_without_writing() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id =
            plant_fan_out(&fixture, STRANDED_FAN_OUT_OPERATION_KIND, "processing", &["ready"])
                .await?;
        mark_parent_failed_with(&fixture, parent_id, "graph_cleanup_failed").await?;

        let report = repair::reconcile_contradicted_fan_out_outcomes(&fixture.state, true).await?;

        assert_eq!(report.candidates, 1, "dry run must report the cohort");
        assert_eq!(report.settled, 0, "dry run must not write");
        assert!(report.dry_run);
        let (status, failure_code, detail) =
            operation_state(&fixture.state.persistence.postgres, parent_id).await?;
        assert_eq!(status, "failed", "dry run must leave the parent as it was");
        assert_eq!(failure_code.as_deref(), Some("graph_cleanup_failed"));
        assert_eq!(detail, None, "dry run must not write a detail");
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn leaves_contradicted_parent_with_a_failed_child_untouched() -> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;
    let result = async {
        let parent_id = plant_fan_out(
            &fixture,
            STRANDED_FAN_OUT_OPERATION_KIND,
            "processing",
            &["ready", "failed"],
        )
        .await?;
        mark_parent_failed_with(&fixture, parent_id, "graph_projection_failed").await?;

        let report = repair::reconcile_contradicted_fan_out_outcomes(&fixture.state, false).await?;

        assert_eq!(report.candidates, 0, "a parent whose child really failed is not contradicted");
        assert_eq!(report.settled, 0);
        let (status, failure_code, _detail) =
            operation_state(&fixture.state.persistence.postgres, parent_id).await?;
        assert_eq!(status, "failed");
        assert_eq!(failure_code.as_deref(), Some("graph_projection_failed"));
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
