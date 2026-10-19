use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::infra::repositories::ingest_repository;

use super::billing::{
    PipelineBillingRow, PipelineStageBilling, aggregate_pipeline_billing,
    collect_document_level_stage_billing, merge_billing_into_stage_events,
};
use super::stage_events::{merge_stages, sort_attempts_latest_first};
use super::{DocumentAttempt, DocumentStageEvent, lifecycle_total_cost_fields};

#[test]
fn lifecycle_cost_fields_keep_zero_cost_visible() {
    let (total_cost, currency_code) = lifecycle_total_cost_fields(Decimal::ZERO, "USD");

    assert_eq!(total_cost, Some(Decimal::ZERO));
    assert_eq!(currency_code.as_deref(), Some("USD"));
}

#[test]
fn lifecycle_cost_fields_keep_non_zero_cost_visible() {
    let amount = Decimal::from_str_exact("0.1234").expect("valid decimal");
    let (total_cost, currency_code) = lifecycle_total_cost_fields(amount, "USD");

    assert_eq!(total_cost, Some(amount));
    assert_eq!(currency_code.as_deref(), Some("USD"));
}

#[test]
fn pipeline_billing_merges_call_kinds_that_share_pipeline_stage() {
    let attempt_id = Uuid::now_v7();
    let rows = vec![
        PipelineBillingRow {
            attempt_id: Some(attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: Decimal::from_str_exact("0.1000").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
            provider_call_count: 2,
        },
        PipelineBillingRow {
            attempt_id: Some(attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: Decimal::from_str_exact("0.2500").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:03Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:07Z")),
            provider_call_count: 5,
        },
    ];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");
    let per_stage =
        billing.per_stage_by_attempt.get(&attempt_id).expect("attempt billing should exist");

    assert_eq!(billing.total, Decimal::from_str_exact("0.3500").expect("valid decimal"));
    assert_eq!(
        billing.total_by_attempt[&attempt_id].total,
        Decimal::from_str_exact("0.3500").expect("valid decimal")
    );
    assert_eq!(per_stage.len(), 1);
    assert_eq!(per_stage[0].stage_name, "extract_graph");
    assert_eq!(per_stage[0].cost, Decimal::from_str_exact("0.3500").expect("valid decimal"));
    assert_eq!(per_stage[0].provider_kind.as_deref(), Some("provider-alpha, provider-beta"));
    assert_eq!(per_stage[0].model_name.as_deref(), Some("alpha-chat-large, alpha-embedding-large"));
    assert_eq!(per_stage[0].elapsed_ms, Some(7000));
    assert_eq!(per_stage[0].provider_call_count, 7);
}

#[test]
fn pipeline_billing_rejects_mixed_currencies_before_summing() {
    let attempt_id = Uuid::now_v7();
    let usd = PipelineBillingRow {
        attempt_id: Some(attempt_id),
        call_kind: "graph_extract".to_string(),
        stage_cost: Decimal::from_str_exact("0.1000").expect("valid decimal"),
        currency_code: "USD".to_string(),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-chat-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
        provider_call_count: 1,
    };
    let mut eur = usd.clone();
    eur.stage_cost = Decimal::from_str_exact("0.2000").expect("valid decimal");
    eur.currency_code = "EUR".to_string();

    let error = aggregate_pipeline_billing(&[usd, eur])
        .expect_err("unlike currencies must never be combined into one document total");

    assert!(error.to_string().contains("multiple currencies"));
}

#[test]
fn pipeline_billing_keeps_zero_cost_provider_calls_visible() {
    let attempt_id = Uuid::now_v7();
    let rows = vec![PipelineBillingRow {
        attempt_id: Some(attempt_id),
        call_kind: "embed_chunk".to_string(),
        stage_cost: Decimal::ZERO,
        currency_code: "USD".to_string(),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-embedding-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:01:00Z")),
        provider_call_count: 3,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");
    let per_stage =
        billing.per_stage_by_attempt.get(&attempt_id).expect("attempt billing should exist");

    assert_eq!(billing.total, Decimal::ZERO);
    assert_eq!(billing.total_by_attempt[&attempt_id].total, Decimal::ZERO);
    assert_eq!(per_stage.len(), 1);
    assert_eq!(per_stage[0].stage_name, "embed_chunk");
    assert_eq!(per_stage[0].cost, Decimal::ZERO);
    assert_eq!(per_stage[0].model_name.as_deref(), Some("alpha-embedding-large"));
    assert_eq!(per_stage[0].elapsed_ms, Some(60000));
    assert_eq!(per_stage[0].provider_call_count, 3);
}

#[test]
fn document_level_stage_billing_keeps_hidden_attempt_costs_observable() {
    let visible_attempt_id = Uuid::now_v7();
    let hidden_attempt_id = Uuid::now_v7();
    let rows = vec![
        PipelineBillingRow {
            attempt_id: Some(visible_attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: Decimal::from_str_exact("0.0100").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:01Z")),
            provider_call_count: 1,
        },
        PipelineBillingRow {
            attempt_id: Some(hidden_attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: Decimal::from_str_exact("0.1200").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("beta-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:02Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:04Z")),
            provider_call_count: 1,
        },
        PipelineBillingRow {
            attempt_id: Some(hidden_attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: Decimal::from_str_exact("0.3400").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:05Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:09Z")),
            provider_call_count: 4,
        },
    ];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");
    let visible_attempt_ids = BTreeSet::from([visible_attempt_id]);
    let document_level_stages =
        collect_document_level_stage_billing(&billing, &visible_attempt_ids);

    assert_eq!(
        document_level_stages.iter().map(|stage| stage.stage_name.as_str()).collect::<Vec<_>>(),
        vec!["embed_chunk", "extract_graph"]
    );
    assert_eq!(
        document_level_stages[0].cost,
        Decimal::from_str_exact("0.1200").expect("valid decimal")
    );
    assert_eq!(
        document_level_stages[1].cost,
        Decimal::from_str_exact("0.3400").expect("valid decimal")
    );
    assert_eq!(document_level_stages[1].provider_call_count, 4);
}

#[test]
fn pipeline_billing_ignores_query_call_kinds_for_document_pipeline_stages() {
    let rows = vec![PipelineBillingRow {
        attempt_id: Some(Uuid::now_v7()),
        call_kind: "query_answer".to_string(),
        stage_cost: Decimal::from_str_exact("1.5000").expect("valid decimal"),
        currency_code: "USD".to_string(),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-chat-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:00:01Z")),
        provider_call_count: 1,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.total, Decimal::from_str_exact("1.5000").expect("valid decimal"));
    assert!(billing.total_by_attempt.is_empty());
    assert!(billing.per_stage_by_attempt.is_empty());
}

#[test]
fn pipeline_billing_keeps_document_stage_cost_when_attempt_link_is_absent() {
    let rows = vec![PipelineBillingRow {
        attempt_id: None,
        call_kind: "graph_extract".to_string(),
        stage_cost: Decimal::from_str_exact("0.2500").expect("valid decimal"),
        currency_code: "USD".to_string(),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-chat-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:00:05Z")),
        provider_call_count: 4,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.total, Decimal::from_str_exact("0.2500").expect("valid decimal"));
    assert!(billing.total_by_attempt.is_empty());
    assert!(billing.per_stage_by_attempt.is_empty());
    assert_eq!(billing.unattributed_per_stage.len(), 1);
    assert_eq!(billing.unattributed_per_stage[0].stage_name, "extract_graph");
    assert_eq!(billing.unattributed_per_stage[0].elapsed_ms, Some(5000));
    assert_eq!(billing.unattributed_per_stage[0].provider_call_count, 4);
}

#[test]
fn pipeline_billing_keeps_retry_attempt_stage_costs_isolated() {
    let first_attempt_id = Uuid::now_v7();
    let retry_attempt_id = Uuid::now_v7();
    let rows = vec![
        PipelineBillingRow {
            attempt_id: Some(first_attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: Decimal::from_str_exact("0.1000").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
            provider_call_count: 1,
        },
        PipelineBillingRow {
            attempt_id: Some(retry_attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: Decimal::from_str_exact("0.2000").expect("valid decimal"),
            currency_code: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:05:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:05:03Z")),
            provider_call_count: 2,
        },
    ];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.total, Decimal::from_str_exact("0.3000").expect("valid decimal"));
    assert_eq!(
        billing.per_stage_by_attempt[&first_attempt_id][0].cost,
        Decimal::from_str_exact("0.1000").expect("valid decimal")
    );
    assert_eq!(billing.per_stage_by_attempt[&first_attempt_id][0].provider_call_count, 1);
    assert_eq!(
        billing.per_stage_by_attempt[&retry_attempt_id][0].cost,
        Decimal::from_str_exact("0.2000").expect("valid decimal")
    );
    assert_eq!(billing.per_stage_by_attempt[&retry_attempt_id][0].provider_call_count, 2);
}

#[test]
fn billing_materializes_missing_pipeline_stages_on_latest_attempt() {
    let mut attempt = DocumentAttempt {
        job_id: Uuid::now_v7(),
        attempt_no: 1,
        attempt_kind: "document_upload".to_string(),
        status: "succeeded".to_string(),
        total_cost: None,
        currency_code: None,
        queue_started_at: utc("2026-05-12T10:00:00Z"),
        started_at: Some(utc("2026-05-12T10:00:00Z")),
        finished_at: Some(utc("2026-05-12T10:00:30Z")),
        total_elapsed_ms: Some(30000),
        stage_events: vec![DocumentStageEvent {
            stage: "extract_content".to_string(),
            status: "completed".to_string(),
            started_at: utc("2026-05-12T10:00:00Z"),
            finished_at: Some(utc("2026-05-12T10:00:02Z")),
            elapsed_ms: Some(2000),
            provider_kind: None,
            model_name: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            estimated_cost: None,
            currency_code: None,
            reused_chunks: None,
            reused_entities: None,
            reused_relations: None,
            chunks_processed: None,
            provider_call_count: None,
            details: None,
        }],
    };
    let stage_billing = vec![
        PipelineStageBilling {
            stage_name: "extract_graph".to_string(),
            cost: Decimal::from_str_exact("0.4500").expect("valid decimal"),
            currency: "USD".to_string(),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            started_at: Some(utc("2026-05-12T10:00:20Z")),
            finished_at: Some(utc("2026-05-12T10:00:29Z")),
            elapsed_ms: Some(9000),
            provider_call_count: 9,
        },
        PipelineStageBilling {
            stage_name: "embed_chunk".to_string(),
            cost: Decimal::from_str_exact("0.1200").expect("valid decimal"),
            currency: "USD".to_string(),
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("beta-embedding-large".to_string()),
            started_at: Some(utc("2026-05-12T10:00:10Z")),
            finished_at: Some(utc("2026-05-12T10:00:13Z")),
            elapsed_ms: Some(3000),
            provider_call_count: 3,
        },
    ];

    merge_billing_into_stage_events(&mut attempt, &stage_billing);

    assert_eq!(
        attempt.stage_events.iter().map(|stage| stage.stage.as_str()).collect::<Vec<_>>(),
        vec!["extract_content", "embed_chunk", "extract_graph"]
    );
    let embed_stage = attempt
        .stage_events
        .iter()
        .find(|stage| stage.stage == "embed_chunk")
        .expect("materialized embed stage");
    assert_eq!(embed_stage.status, "completed");
    assert_eq!(embed_stage.elapsed_ms, Some(3000));
    assert_eq!(embed_stage.model_name.as_deref(), Some("beta-embedding-large"));
    assert_eq!(embed_stage.provider_kind.as_deref(), Some("provider-beta"));
    assert_eq!(embed_stage.provider_call_count, Some(3));
    assert_eq!(
        embed_stage.estimated_cost,
        Some(Decimal::from_str_exact("0.1200").expect("valid decimal"))
    );
}

#[test]
fn terminal_stage_event_keeps_started_finished_and_elapsed_time() {
    let row = ingest_repository::IngestStageEventRow {
        id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        stage_name: "extract_content".to_string(),
        stage_state: "completed".to_string(),
        ordinal: 10,
        message: None,
        details_json: json!({}),
        recorded_at: utc("2026-05-12T10:00:12Z"),
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: None,
        started_at: Some(utc("2026-05-12T10:00:02Z")),
    };

    let stages = merge_stages(&[&row]);

    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].stage, "extract_content");
    assert_eq!(stages[0].status, "completed");
    assert_eq!(stages[0].started_at, utc("2026-05-12T10:00:02Z"));
    assert_eq!(stages[0].finished_at, Some(utc("2026-05-12T10:00:12Z")));
    assert_eq!(stages[0].elapsed_ms, Some(10000));
}

#[test]
fn attempts_with_same_queue_time_sort_by_latest_retry_first() {
    let queue_started_at = utc("2026-05-12T10:00:00Z");
    let mut attempts = vec![
        attempt_summary(1, "failed", queue_started_at, "2026-05-12T10:00:01Z"),
        attempt_summary(4, "succeeded", queue_started_at, "2026-05-12T10:30:00Z"),
        attempt_summary(2, "failed", queue_started_at, "2026-05-12T10:05:00Z"),
    ];

    sort_attempts_latest_first(&mut attempts);

    assert_eq!(attempts[0].attempt_no, 4);
    assert_eq!(attempts[0].status, "succeeded");
    assert_eq!(attempts[1].attempt_no, 2);
    assert_eq!(attempts[2].attempt_no, 1);
}

fn attempt_summary(
    attempt_no: i32,
    status: &str,
    queue_started_at: DateTime<Utc>,
    started_at: &str,
) -> DocumentAttempt {
    DocumentAttempt {
        job_id: Uuid::now_v7(),
        attempt_no,
        attempt_kind: "document_upload".to_string(),
        status: status.to_string(),
        total_cost: None,
        currency_code: None,
        queue_started_at,
        started_at: Some(utc(started_at)),
        finished_at: None,
        total_elapsed_ms: None,
        stage_events: Vec::new(),
    }
}

fn utc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value).expect("valid timestamp").with_timezone(&Utc)
}
