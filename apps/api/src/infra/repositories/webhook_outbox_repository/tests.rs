use anyhow::{Context as _, Result};
use sqlx::{PgPool, Row as _, postgres::PgPoolOptions};

use super::operator::{
    AUDIT_WEBHOOK_LIFECYCLE_OUTBOX_SELECT_SQL, build_webhook_lifecycle_outbox_audit_query,
};
use super::*;
use crate::app::config::Settings;

#[test]
fn operator_audit_query_never_selects_secret_bearing_columns() {
    let normalized = AUDIT_WEBHOOK_LIFECYCLE_OUTBOX_SELECT_SQL.to_ascii_lowercase();
    for forbidden in [
        "payload_json",
        "event_id",
        "target_url",
        "secret",
        "custom_headers",
        "lease_owner",
        "lease_token",
    ] {
        assert!(!normalized.contains(forbidden), "audit query loaded forbidden column {forbidden}");
    }
    assert!(
        !normalized.lines().any(|line| line.trim().trim_end_matches(',') == "last_error"),
        "audit query loaded raw failure text",
    );
    assert!(normalized.contains("last_error_code"));
}

#[test]
fn operator_audit_builds_sargable_keyset_predicates() {
    let cursor = WebhookLifecycleOutboxAuditCursor { created_at: Utc::now(), id: Uuid::now_v7() };
    let query = build_webhook_lifecycle_outbox_audit_query(
        Some(WebhookLifecycleOutboxDispatchState::DeadLetter),
        Some(Uuid::now_v7()),
        Some(cursor),
        100,
    );
    let sql = query.sql().as_str().to_ascii_lowercase();
    assert!(sql.contains("dispatch_state = $1"));
    assert!(sql.contains("library_id = $2"));
    assert!(sql.contains("(created_at, id) < ($3, $4)"));
    assert!(sql.contains("order by created_at desc, id desc limit $5"));
    assert!(!sql.contains("is null or"));
}

#[test]
fn resolution_reason_codes_are_bounded_and_machine_readable() {
    assert!(is_supported_webhook_lifecycle_outbox_resolution_reason_code("receiver_retired"));
    assert!(is_supported_webhook_lifecycle_outbox_resolution_reason_code("a"));
    assert!(!is_supported_webhook_lifecycle_outbox_resolution_reason_code(""));
    assert!(!is_supported_webhook_lifecycle_outbox_resolution_reason_code("Receiver retired"));
    assert!(!is_supported_webhook_lifecycle_outbox_resolution_reason_code(
        &"a".repeat(MAX_WEBHOOK_LIFECYCLE_OUTBOX_RESOLUTION_REASON_CODE_BYTES + 1)
    ));
}

async fn connect_postgres() -> Result<PgPool> {
    let settings = Settings::from_env().context("load settings for outbox operator test")?;
    let postgres = PgPoolOptions::new()
        .max_connections(2)
        .connect(&settings.database_url)
        .await
        .context("connect outbox operator test postgres")?;
    sqlx::migrate!("./migrations")
        .run(&postgres)
        .await
        .context("apply migrations for outbox operator test")?;
    Ok(postgres)
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn audit_filter_and_exact_dead_letter_repair_are_fail_closed() -> Result<()> {
    let postgres = connect_postgres().await?;
    let mut transaction = postgres.begin().await?;
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let dead_letter_id = Uuid::now_v7();
    let pending_id = Uuid::now_v7();
    let suffix = Uuid::now_v7().simple().to_string();
    let payload_marker = format!("private-payload-{suffix}");
    let error_marker = format!("private-error-{suffix}");
    let dead_letter_event_marker = format!("revision.ready:dead-letter:{dead_letter_id}");

    sqlx::query(
        "insert into catalog_workspace (id, slug, display_name)
         values ($1, $2, 'Webhook outbox operator test')",
    )
    .bind(workspace_id)
    .bind(format!("outbox-ops-{suffix}"))
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "insert into catalog_library (id, workspace_id, slug, display_name)
         values ($1, $2, $3, 'Webhook outbox operator test library')",
    )
    .bind(library_id)
    .bind(workspace_id)
    .bind(format!("outbox-ops-library-{suffix}"))
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "insert into webhook_lifecycle_outbox (
            id, event_id, event_type, occurred_at, workspace_id, library_id,
            payload_json, dispatch_state, dispatch_attempts, last_error_code, last_error
         ) values
            ($1, $3, 'revision.ready', now(), $5, $6, $7, 'dead_letter', 8,
             'fanout_failed', 'redacted failure'),
            ($2, $4, 'revision.ready', now(), $5, $6, $7, 'pending', 2, null, null)",
    )
    .bind(dead_letter_id)
    .bind(pending_id)
    .bind(&dead_letter_event_marker)
    .bind(format!("revision.ready:pending:{pending_id}"))
    .bind(workspace_id)
    .bind(library_id)
    .bind(serde_json::json!({ "private": &payload_marker }))
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update webhook_lifecycle_outbox
         set last_error = $2
         where id = $1",
    )
    .bind(dead_letter_id)
    .bind(&error_marker)
    .execute(&mut *transaction)
    .await?;

    let rows = audit_webhook_lifecycle_outbox(
        &mut *transaction,
        Some(WebhookLifecycleOutboxDispatchState::DeadLetter),
        Some(library_id),
        None,
        100,
    )
    .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, dead_letter_id);
    assert_eq!(rows[0].last_error_code.as_deref(), Some("fanout_failed"));
    let audit_json = serde_json::to_string(&rows)?;
    let audit_debug = format!("{rows:?}");
    for marker in [&payload_marker, &error_marker, &dead_letter_event_marker] {
        assert!(!audit_json.contains(marker), "redacted audit leaked marker {marker}");
        assert!(!audit_debug.contains(marker), "redacted audit debug leaked marker {marker}");
    }

    let untouched =
        requeue_dead_letter_webhook_lifecycle_outbox(&mut *transaction, pending_id).await?;
    assert!(untouched.is_none(), "pending rows must not be modified by dead-letter repair");

    let repaired = requeue_dead_letter_webhook_lifecycle_outbox(&mut *transaction, dead_letter_id)
        .await?
        .context("dead-letter row was not requeued")?;
    assert_eq!(repaired.dispatch_state, "pending");
    assert_eq!(repaired.dispatch_attempts, 0);
    assert!(repaired.lease_expires_at.is_none());

    let persisted = sqlx::query(
        "select
            dispatch_state,
            dispatch_attempts,
            available_at <= now() as due,
            lease_owner is null as lease_owner_cleared,
            lease_token is null as lease_token_cleared,
            leased_at is null as leased_at_cleared,
            lease_expires_at is null as lease_expiry_cleared,
            last_error_code is null as error_code_cleared,
            last_error is null as error_cleared,
            dispatched_at is null as dispatched_at_cleared
         from webhook_lifecycle_outbox
         where id = $1",
    )
    .bind(dead_letter_id)
    .fetch_one(&mut *transaction)
    .await?;
    assert_eq!(persisted.get::<String, _>("dispatch_state"), "pending");
    assert_eq!(persisted.get::<i32, _>("dispatch_attempts"), 0);
    for column in [
        "due",
        "lease_owner_cleared",
        "lease_token_cleared",
        "leased_at_cleared",
        "lease_expiry_cleared",
        "error_code_cleared",
        "error_cleared",
        "dispatched_at_cleared",
    ] {
        assert!(persisted.get::<bool, _>(column), "repair invariant failed: {column}");
    }

    let second_repair =
        requeue_dead_letter_webhook_lifecycle_outbox(&mut *transaction, dead_letter_id).await?;
    assert!(second_repair.is_none(), "repair must be a one-way compare-and-set");

    transaction.rollback().await?;
    postgres.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn concurrent_dead_letter_repair_has_exactly_one_winner() -> Result<()> {
    let postgres = connect_postgres().await?;
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let outbox_id = Uuid::now_v7();
    let suffix = Uuid::now_v7().simple().to_string();

    let test_result = async {
        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name)
             values ($1, $2, 'Concurrent outbox operator test')",
        )
        .bind(workspace_id)
        .bind(format!("outbox-cas-{suffix}"))
        .execute(&postgres)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, $3, 'Concurrent outbox operator test library')",
        )
        .bind(library_id)
        .bind(workspace_id)
        .bind(format!("outbox-cas-library-{suffix}"))
        .execute(&postgres)
        .await?;
        sqlx::query(
            "insert into webhook_lifecycle_outbox (
                id, event_id, event_type, occurred_at, workspace_id, library_id,
                payload_json, dispatch_state, dispatch_attempts, last_error_code, last_error
             ) values ($1, $2, 'revision.ready', now(), $3, $4, '{}',
                       'dead_letter', 8, 'fanout_failed', 'redacted failure')",
        )
        .bind(outbox_id)
        .bind(format!("revision.ready:concurrent-cas:{outbox_id}"))
        .bind(workspace_id)
        .bind(library_id)
        .execute(&postgres)
        .await?;

        let mut left_connection = postgres.acquire().await?;
        let mut right_connection = postgres.acquire().await?;
        let (left, right) = tokio::join!(
            requeue_dead_letter_webhook_lifecycle_outbox(&mut *left_connection, outbox_id,),
            requeue_dead_letter_webhook_lifecycle_outbox(&mut *right_connection, outbox_id,),
        );
        let winners = [left?, right?].into_iter().filter(Option::is_some).count();
        anyhow::ensure!(winners == 1, "expected exactly one CAS winner, got {winners}");
        Ok::<(), anyhow::Error>(())
    }
    .await;

    // Cleanup runs even when the assertion path returns an error.
    let _ = sqlx::query("delete from webhook_lifecycle_outbox where id = $1")
        .bind(outbox_id)
        .execute(&postgres)
        .await;
    let _ = sqlx::query("delete from catalog_library where id = $1")
        .bind(library_id)
        .execute(&postgres)
        .await;
    let _ = sqlx::query("delete from catalog_workspace where id = $1")
        .bind(workspace_id)
        .execute(&postgres)
        .await;
    postgres.close().await;
    test_result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn dead_letter_resolution_is_atomic_audited_and_never_masks_delivery() -> Result<()> {
    let postgres = connect_postgres().await?;
    let mut transaction = postgres.begin().await?;
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let dead_letter_id = Uuid::now_v7();
    let dispatched_id = Uuid::now_v7();
    let suffix = Uuid::now_v7().simple().to_string();

    sqlx::query(
        "insert into catalog_workspace (id, slug, display_name)
         values ($1, $2, 'Webhook outbox resolution test')",
    )
    .bind(workspace_id)
    .bind(format!("outbox-resolution-{suffix}"))
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "insert into catalog_library (id, workspace_id, slug, display_name)
         values ($1, $2, $3, 'Webhook outbox resolution test library')",
    )
    .bind(library_id)
    .bind(workspace_id)
    .bind(format!("outbox-resolution-library-{suffix}"))
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "insert into webhook_lifecycle_outbox (
            id, event_id, event_type, occurred_at, workspace_id, library_id,
            payload_json, dispatch_state, dispatch_attempts,
            last_error_code, last_error, dispatched_at
         ) values
            ($1, $3, 'revision.ready', now(), $5, $6, '{}',
             'dead_letter', 12, 'fanout_failed', 'redacted failure', null),
            ($2, $4, 'revision.ready', now(), $5, $6, '{}',
             'dispatched', 1, null, null, now())",
    )
    .bind(dead_letter_id)
    .bind(dispatched_id)
    .bind(format!("revision.ready:resolved:{dead_letter_id}"))
    .bind(format!("revision.ready:dispatched:{dispatched_id}"))
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;

    assert!(
        resolve_dead_letter_webhook_lifecycle_outbox(
            &mut *transaction,
            dead_letter_id,
            "free form secret-like reason",
        )
        .await
        .is_err(),
        "free-form resolution text must fail before persistence",
    );
    let resolved = resolve_dead_letter_webhook_lifecycle_outbox(
        &mut *transaction,
        dead_letter_id,
        "receiver_retired",
    )
    .await?
    .context("dead-letter row was not resolved")?;
    assert_eq!(resolved.dispatch_state, "resolved");
    assert_eq!(resolved.resolution_reason_code.as_deref(), Some("receiver_retired"));
    assert!(resolved.resolved_at.is_some());
    assert!(resolved.dispatched_at.is_none(), "resolution must never claim delivery");

    assert!(
        resolve_dead_letter_webhook_lifecycle_outbox(
            &mut *transaction,
            dead_letter_id,
            "receiver_retired",
        )
        .await?
        .is_none(),
        "resolution must be one exact compare-and-set",
    );
    assert!(
        resolve_dead_letter_webhook_lifecycle_outbox(
            &mut *transaction,
            dispatched_id,
            "receiver_retired",
        )
        .await?
        .is_none(),
        "a dispatched row must never be relabeled as resolved",
    );

    let audit = sqlx::query_as::<_, (String, String, String, Uuid, Uuid)>(
        "select
            event.action_kind, event.result_kind::text,
            event.redacted_message, subject.workspace_id, subject.library_id
         from audit_event event
         join audit_event_subject subject on subject.audit_event_id = event.id
         where subject.subject_kind = 'webhook_lifecycle_outbox'
           and subject.subject_id = $1",
    )
    .bind(dead_letter_id)
    .fetch_one(&mut *transaction)
    .await?;
    assert_eq!(audit.0, "webhook.lifecycle_outbox.dead_letter_resolved");
    assert_eq!(audit.1, "succeeded");
    assert!(audit.2.contains("reason_code=receiver_retired"));
    assert_eq!((audit.3, audit.4), (workspace_id, library_id));

    sqlx::query("delete from webhook_lifecycle_outbox where id = $1")
        .bind(dead_letter_id)
        .execute(&mut *transaction)
        .await?;
    let durable_audit = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from audit_event_subject
         where subject_kind = 'webhook_lifecycle_outbox'
           and subject_id = $1",
    )
    .bind(dead_letter_id)
    .fetch_one(&mut *transaction)
    .await?;
    assert_eq!(durable_audit, 1, "resolution audit must survive outbox/catalog deletion");

    transaction.rollback().await?;
    postgres.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn lifecycle_outbox_lease_renewal_uses_db_clock_and_token_fencing() -> Result<()> {
    let postgres = connect_postgres().await?;
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let outbox_id = Uuid::now_v7();
    let suffix = Uuid::now_v7().simple().to_string();

    let test_result = async {
        sqlx::query(
            "insert into catalog_workspace (id, slug, display_name)
             values ($1, $2, 'Webhook outbox heartbeat test')",
        )
        .bind(workspace_id)
        .bind(format!("outbox-heartbeat-{suffix}"))
        .execute(&postgres)
        .await?;
        sqlx::query(
            "insert into catalog_library (id, workspace_id, slug, display_name)
             values ($1, $2, $3, 'Webhook outbox heartbeat test library')",
        )
        .bind(library_id)
        .bind(workspace_id)
        .bind(format!("outbox-heartbeat-library-{suffix}"))
        .execute(&postgres)
        .await?;
        sqlx::query(
            "insert into webhook_lifecycle_outbox (
                id, event_id, event_type, occurred_at,
                workspace_id, library_id, payload_json
             ) values ($1, $2, 'revision.ready', now(), $3, $4, '{}')",
        )
        .bind(outbox_id)
        .bind(format!("revision.ready:heartbeat:{outbox_id}"))
        .bind(workspace_id)
        .bind(library_id)
        .execute(&postgres)
        .await?;

        let first = lease_webhook_lifecycle_outbox_batch(
            &postgres,
            "heartbeat-owner-one",
            chrono::Duration::seconds(2),
            1,
        )
        .await?;
        anyhow::ensure!(first.events.len() == 1, "first owner did not lease fixture row");
        assert!(
            renew_webhook_lifecycle_outbox_lease(
                &postgres,
                outbox_id,
                first.lease_token,
                chrono::Duration::seconds(120),
            )
            .await?
        );
        let db_clock_extended = sqlx::query_scalar::<_, bool>(
            "select lease_expires_at >= now() + interval '110 seconds'
             from webhook_lifecycle_outbox where id = $1",
        )
        .bind(outbox_id)
        .fetch_one(&postgres)
        .await?;
        assert!(db_clock_extended, "renewal must be based on PostgreSQL now()");

        sqlx::query(
            "update webhook_lifecycle_outbox
             set lease_expires_at = now() - interval '1 second'
             where id = $1",
        )
        .bind(outbox_id)
        .execute(&postgres)
        .await?;
        let replacement = lease_webhook_lifecycle_outbox_batch(
            &postgres,
            "heartbeat-owner-two",
            chrono::Duration::seconds(120),
            1,
        )
        .await?;
        anyhow::ensure!(replacement.events.len() == 1, "replacement owner did not reclaim");
        assert_ne!(replacement.lease_token, first.lease_token);
        assert!(
            !renew_webhook_lifecycle_outbox_lease(
                &postgres,
                outbox_id,
                first.lease_token,
                chrono::Duration::seconds(120),
            )
            .await?,
            "stale owner must not renew the replacement token",
        );
        assert!(
            renew_webhook_lifecycle_outbox_lease(
                &postgres,
                outbox_id,
                replacement.lease_token,
                chrono::Duration::seconds(120),
            )
            .await?
        );
        assert!(
            mark_webhook_lifecycle_outbox_dispatched(
                &postgres,
                outbox_id,
                replacement.lease_token,
            )
            .await?
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;

    let _ = sqlx::query("delete from catalog_workspace where id = $1")
        .bind(workspace_id)
        .execute(&postgres)
        .await;
    postgres.close().await;
    test_result
}
