use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::domains::billing::CostCoverageStatus;
use crate::infra::repositories::ingest_repository;

use super::billing::{
    PipelineBillingRow, PipelineCallCounts, PipelineStageBilling, aggregate_pipeline_billing,
    collect_document_level_stage_billing, merge_billing_into_stage_events,
};
use super::stage_events::{merge_stages, sort_attempts_latest_first};
use super::{DocumentAttempt, DocumentStageEvent, lifecycle_cost_fields};

fn dec(value: &str) -> Decimal {
    Decimal::from_str_exact(value).expect("valid decimal")
}

fn counts(
    provider_call_count: i64,
    priced_provider_call_count: i64,
    failed_provider_call_count: i64,
    unsettled_provider_call_count: i64,
) -> PipelineCallCounts {
    PipelineCallCounts {
        provider_call_count,
        priced_provider_call_count,
        unpriced_provider_call_count: provider_call_count - priced_provider_call_count
            + failed_provider_call_count
            + unsettled_provider_call_count,
        failed_provider_call_count,
        unsettled_provider_call_count,
    }
}

#[test]
fn lifecycle_reports_a_zero_only_when_a_call_was_actually_priced_at_zero() {
    let (total_cost, currency_code, coverage) =
        lifecycle_cost_fields(counts(1, 1, 0, 0), Decimal::ZERO, Some("USD"));

    assert_eq!(total_cost, Some(Decimal::ZERO));
    assert_eq!(currency_code.as_deref(), Some("USD"));
    assert_eq!(coverage, Some(CostCoverageStatus::Complete));
}

#[test]
fn lifecycle_does_not_present_an_unpriced_sum_as_a_complete_zero() {
    let (total_cost, currency_code, coverage) =
        lifecycle_cost_fields(counts(3, 0, 0, 0), Decimal::ZERO, None);

    assert_eq!(total_cost, None, "an unpriced sum carries no cost information");
    assert_eq!(currency_code, None, "no currency may be invented");
    assert_eq!(coverage, Some(CostCoverageStatus::Unknown));
}

#[test]
fn lifecycle_reports_a_partially_priced_sum_as_a_lower_bound() {
    let (total_cost, currency_code, coverage) =
        lifecycle_cost_fields(counts(4, 2, 0, 0), dec("0.2500"), Some("EUR"));

    assert_eq!(total_cost, Some(dec("0.2500")));
    assert_eq!(currency_code.as_deref(), Some("EUR"));
    assert_eq!(coverage, Some(CostCoverageStatus::Partial));
}

#[test]
fn lifecycle_leaves_an_unrecorded_document_unmeasured() {
    let (total_cost, currency_code, coverage) =
        lifecycle_cost_fields(PipelineCallCounts::default(), Decimal::ZERO, None);

    assert_eq!(total_cost, None);
    assert_eq!(currency_code, None);
    assert_eq!(coverage, None, "no recorded provider call cannot be certified complete");
}

#[test]
fn failed_reservations_are_settled_neither_paid_nor_free() {
    // Two reservations terminated without a settled response: no settled usage
    // and no proof of either a dispatch or a zero charge.
    let counts = counts(0, 0, 2, 0);

    assert_eq!(counts.measured_cost(Decimal::ZERO), None);
    assert_eq!(counts.coverage(), Some(CostCoverageStatus::Unknown));
    assert_eq!(counts.provider_call_count, 0);
    assert_eq!(counts.failed_provider_call_count, 2);
    assert_eq!(counts.stage_status(), "failed");
}

#[test]
fn priced_success_with_an_unresolved_reservation_is_partial_not_unknown() {
    // 1 completed and priced call + 1 reservation that failed. The unpriced
    // count is not comparable with the completed count as a denominator.
    let counts = counts(1, 1, 1, 0);

    assert_eq!(counts.coverage(), Some(CostCoverageStatus::Partial));
    assert_eq!(counts.measured_cost(dec("0.4000")), Some(dec("0.4000")));
}

#[test]
fn priced_zero_with_an_unresolved_reservation_is_partial() {
    let counts = counts(1, 1, 1, 0);

    assert_eq!(
        counts.measured_cost(Decimal::ZERO),
        Some(Decimal::ZERO),
        "a priced call keeps its genuine zero even with an unresolved sibling"
    );
    assert_eq!(counts.coverage(), Some(CostCoverageStatus::Partial));
}

#[test]
fn an_open_reservation_keeps_its_stage_running_and_uncovered() {
    let counts = counts(0, 0, 0, 1);

    assert_eq!(counts.measured_cost(Decimal::ZERO), None);
    assert_eq!(counts.coverage(), Some(CostCoverageStatus::Unknown));
    assert_eq!(counts.stage_status(), "running");
}

#[test]
fn pipeline_billing_merges_call_kinds_that_share_pipeline_stage() {
    let attempt_id = Uuid::now_v7();
    let rows = vec![
        PipelineBillingRow {
            attempt_id: Some(attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: dec("0.1000"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
            total_provider_call_count: 2,
            provider_call_count: 2,
            priced_provider_call_count: 2,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
        },
        PipelineBillingRow {
            attempt_id: Some(attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: dec("0.2500"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:03Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:07Z")),
            total_provider_call_count: 5,
            provider_call_count: 5,
            priced_provider_call_count: 5,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
        },
    ];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");
    let per_stage =
        billing.per_stage_by_attempt.get(&attempt_id).expect("attempt billing should exist");

    assert_eq!(billing.total, dec("0.3500"));
    assert_eq!(billing.currency.as_deref(), Some("USD"));
    assert_eq!(billing.total_by_attempt[&attempt_id].cost, dec("0.3500"));
    assert_eq!(per_stage.len(), 1);
    assert_eq!(per_stage[0].stage_name, "extract_graph");
    assert_eq!(per_stage[0].cost, dec("0.3500"));
    assert_eq!(per_stage[0].currency.as_deref(), Some("USD"));
    assert_eq!(per_stage[0].provider_kind.as_deref(), Some("provider-alpha, provider-beta"));
    assert_eq!(per_stage[0].model_name.as_deref(), Some("alpha-chat-large, alpha-embedding-large"));
    assert_eq!(per_stage[0].elapsed_ms, Some(7000));
    assert_eq!(per_stage[0].call_counts.provider_call_count, 7);
    assert_eq!(per_stage[0].call_counts.priced_provider_call_count, 7);
    assert_eq!(per_stage[0].call_counts.coverage(), Some(CostCoverageStatus::Complete));
}

#[test]
fn pipeline_billing_rejects_mixed_currencies_before_summing() {
    let attempt_id = Uuid::now_v7();
    let usd = PipelineBillingRow {
        attempt_id: Some(attempt_id),
        call_kind: "graph_extract".to_string(),
        stage_cost: dec("0.1000"),
        currency_code: Some("USD".to_string()),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-chat-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
        total_provider_call_count: 1,
        provider_call_count: 1,
        priced_provider_call_count: 1,
        failed_provider_call_count: 0,
        unsettled_provider_call_count: 0,
    };
    let mut eur = usd.clone();
    eur.stage_cost = dec("0.2000");
    eur.currency_code = Some("EUR".to_string());

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
        currency_code: Some("USD".to_string()),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-embedding-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:01:00Z")),
        total_provider_call_count: 3,
        provider_call_count: 3,
        priced_provider_call_count: 3,
        failed_provider_call_count: 0,
        unsettled_provider_call_count: 0,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");
    let per_stage =
        billing.per_stage_by_attempt.get(&attempt_id).expect("attempt billing should exist");

    assert_eq!(billing.total, Decimal::ZERO);
    assert_eq!(per_stage.len(), 1);
    assert_eq!(per_stage[0].stage_name, "embed_chunk");
    assert_eq!(per_stage[0].cost, Decimal::ZERO);
    assert_eq!(per_stage[0].call_counts.measured_cost(per_stage[0].cost), Some(Decimal::ZERO));
    assert_eq!(per_stage[0].model_name.as_deref(), Some("alpha-embedding-large"));
    assert_eq!(per_stage[0].elapsed_ms, Some(60000));
    assert_eq!(per_stage[0].call_counts.provider_call_count, 3);
    assert_eq!(per_stage[0].call_counts.coverage(), Some(CostCoverageStatus::Complete));
}

#[test]
fn pipeline_billing_keeps_unpriced_calls_observable_without_a_zero_price() {
    let attempt_id = Uuid::now_v7();
    let rows = vec![PipelineBillingRow {
        attempt_id: Some(attempt_id),
        call_kind: "embed_chunk".to_string(),
        stage_cost: Decimal::ZERO,
        currency_code: None,
        provider_kind: None,
        model_name: None,
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:01:00Z")),
        total_provider_call_count: 4,
        provider_call_count: 4,
        priced_provider_call_count: 0,
        failed_provider_call_count: 0,
        unsettled_provider_call_count: 0,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.currency, None);
    assert_eq!(billing.call_counts.measured_cost(billing.total), None);
    assert_eq!(billing.call_counts.coverage(), Some(CostCoverageStatus::Unknown));
    assert_eq!(billing.call_counts.provider_call_count, 4);
    assert_eq!(billing.call_counts.unpriced_provider_call_count, 4);
}

#[test]
fn document_level_stage_billing_keeps_hidden_attempt_costs_observable() {
    let visible_attempt_id = Uuid::now_v7();
    let hidden_attempt_id = Uuid::now_v7();
    let rows = vec![
        PipelineBillingRow {
            attempt_id: Some(visible_attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: dec("0.0100"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:01Z")),
            total_provider_call_count: 1,
            provider_call_count: 1,
            priced_provider_call_count: 1,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
        },
        PipelineBillingRow {
            attempt_id: Some(hidden_attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: dec("0.1200"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("beta-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:02Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:04Z")),
            total_provider_call_count: 1,
            provider_call_count: 1,
            priced_provider_call_count: 1,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
        },
        PipelineBillingRow {
            attempt_id: Some(hidden_attempt_id),
            call_kind: "graph_extract".to_string(),
            stage_cost: dec("0.3400"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:05Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:09Z")),
            total_provider_call_count: 4,
            provider_call_count: 4,
            priced_provider_call_count: 4,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
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
    assert_eq!(document_level_stages[0].cost, dec("0.1200"));
    assert_eq!(document_level_stages[1].cost, dec("0.3400"));
    assert_eq!(document_level_stages[1].call_counts.provider_call_count, 4);
}

#[test]
fn pipeline_billing_ignores_query_call_kinds_for_document_pipeline_stages() {
    let rows = vec![PipelineBillingRow {
        attempt_id: Some(Uuid::now_v7()),
        call_kind: "query_answer".to_string(),
        stage_cost: dec("1.5000"),
        currency_code: Some("USD".to_string()),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-chat-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:00:01Z")),
        total_provider_call_count: 1,
        provider_call_count: 1,
        priced_provider_call_count: 1,
        failed_provider_call_count: 0,
        unsettled_provider_call_count: 0,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.total, dec("1.5000"));
    assert!(billing.total_by_attempt.is_empty());
    assert!(billing.per_stage_by_attempt.is_empty());
}

#[test]
fn pipeline_billing_keeps_document_stage_cost_when_attempt_link_is_absent() {
    let rows = vec![PipelineBillingRow {
        attempt_id: None,
        call_kind: "graph_extract".to_string(),
        stage_cost: dec("0.2500"),
        currency_code: Some("USD".to_string()),
        provider_kind: Some("provider-alpha".to_string()),
        model_name: Some("alpha-chat-large".to_string()),
        stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
        stage_finished_at: Some(utc("2026-05-12T10:00:05Z")),
        total_provider_call_count: 4,
        provider_call_count: 4,
        priced_provider_call_count: 4,
        failed_provider_call_count: 0,
        unsettled_provider_call_count: 0,
    }];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.total, dec("0.2500"));
    assert!(billing.total_by_attempt.is_empty());
    assert!(billing.per_stage_by_attempt.is_empty());
    assert_eq!(billing.unattributed_per_stage.len(), 1);
    assert_eq!(billing.unattributed_per_stage[0].stage_name, "extract_graph");
    assert_eq!(billing.unattributed_per_stage[0].elapsed_ms, Some(5000));
    assert_eq!(billing.unattributed_per_stage[0].call_counts.provider_call_count, 4);
}

#[test]
fn pipeline_billing_keeps_retry_attempt_stage_costs_isolated() {
    let first_attempt_id = Uuid::now_v7();
    let retry_attempt_id = Uuid::now_v7();
    let rows = vec![
        PipelineBillingRow {
            attempt_id: Some(first_attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: dec("0.1000"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:00:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:00:02Z")),
            total_provider_call_count: 1,
            provider_call_count: 1,
            priced_provider_call_count: 1,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
        },
        PipelineBillingRow {
            attempt_id: Some(retry_attempt_id),
            call_kind: "embed_chunk".to_string(),
            stage_cost: dec("0.2000"),
            currency_code: Some("USD".to_string()),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-embedding-large".to_string()),
            stage_started_at: Some(utc("2026-05-12T10:05:00Z")),
            stage_finished_at: Some(utc("2026-05-12T10:05:03Z")),
            total_provider_call_count: 2,
            provider_call_count: 2,
            priced_provider_call_count: 2,
            failed_provider_call_count: 0,
            unsettled_provider_call_count: 0,
        },
    ];

    let billing =
        aggregate_pipeline_billing(&rows).expect("single-currency billing rows should aggregate");

    assert_eq!(billing.total, dec("0.3000"));
    assert_eq!(
        billing.total_by_attempt[&first_attempt_id].cost,
        dec("0.1000"),
        "an attempt never carries the document-wide total"
    );
    assert_eq!(billing.per_stage_by_attempt[&first_attempt_id][0].cost, dec("0.1000"));
    assert_eq!(
        billing.per_stage_by_attempt[&first_attempt_id][0].call_counts.provider_call_count,
        1
    );
    assert_eq!(billing.total_by_attempt[&retry_attempt_id].cost, dec("0.2000"));
    assert_eq!(billing.per_stage_by_attempt[&retry_attempt_id][0].cost, dec("0.2000"));
    assert_eq!(
        billing.per_stage_by_attempt[&retry_attempt_id][0].call_counts.provider_call_count,
        2
    );
}

#[test]
fn billing_materializes_missing_pipeline_stages_on_latest_attempt() {
    let mut attempt = attempt_with_content_stage();

    let stage_billing = vec![
        PipelineStageBilling {
            stage_name: "extract_graph".to_string(),
            cost: dec("0.4500"),
            currency: Some("USD".to_string()),
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-chat-large".to_string()),
            started_at: Some(utc("2026-05-12T10:00:20Z")),
            finished_at: Some(utc("2026-05-12T10:00:29Z")),
            elapsed_ms: Some(9000),
            call_counts: counts(9, 9, 0, 0),
        },
        PipelineStageBilling {
            stage_name: "embed_chunk".to_string(),
            cost: dec("0.1200"),
            currency: Some("USD".to_string()),
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("beta-embedding-large".to_string()),
            started_at: Some(utc("2026-05-12T10:00:10Z")),
            finished_at: Some(utc("2026-05-12T10:00:13Z")),
            elapsed_ms: Some(3000),
            call_counts: counts(3, 3, 0, 0),
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
    assert_eq!(embed_stage.cost_coverage, Some(CostCoverageStatus::Complete));
    assert_eq!(embed_stage.estimated_cost, Some(dec("0.1200")));
}

#[test]
fn materialized_stage_from_unpriced_calls_claims_no_measured_cost() {
    let mut attempt = attempt_with_content_stage();

    merge_billing_into_stage_events(
        &mut attempt,
        &[PipelineStageBilling {
            stage_name: "embed_chunk".to_string(),
            cost: Decimal::ZERO,
            currency: None,
            provider_kind: Some("provider-beta".to_string()),
            model_name: Some("beta-embedding-large".to_string()),
            started_at: Some(utc("2026-05-12T10:00:10Z")),
            finished_at: Some(utc("2026-05-12T10:00:13Z")),
            elapsed_ms: Some(3000),
            call_counts: counts(2, 0, 0, 0),
        }],
    );

    let embed_stage = attempt
        .stage_events
        .iter()
        .find(|stage| stage.stage == "embed_chunk")
        .expect("materialized embed stage");
    assert_eq!(embed_stage.estimated_cost, None);
    assert_eq!(embed_stage.currency_code, None);
    assert_eq!(embed_stage.unpriced_provider_call_count, Some(2));
    assert_eq!(embed_stage.cost_coverage, Some(CostCoverageStatus::Unknown));
}

#[test]
fn materialized_stage_from_failed_reservations_is_not_completed() {
    let mut attempt = attempt_with_content_stage();

    merge_billing_into_stage_events(
        &mut attempt,
        &[PipelineStageBilling {
            stage_name: "extract_graph".to_string(),
            cost: Decimal::ZERO,
            currency: None,
            provider_kind: None,
            model_name: None,
            started_at: Some(utc("2026-05-12T10:00:20Z")),
            finished_at: Some(utc("2026-05-12T10:00:29Z")),
            elapsed_ms: Some(9000),
            call_counts: counts(0, 0, 2, 0),
        }],
    );

    let graph_stage = attempt
        .stage_events
        .iter()
        .find(|stage| stage.stage == "extract_graph")
        .expect("materialized graph stage");
    assert_eq!(graph_stage.status, "failed");
    assert_eq!(graph_stage.provider_call_count, Some(0));
    assert_eq!(graph_stage.failed_provider_call_count, Some(2));
    assert_eq!(graph_stage.estimated_cost, None);
}

#[test]
fn materialized_stage_with_a_mixed_completion_is_not_completed() {
    for (call_counts, expected_status) in [
        // One completed call does not outvote a failed attempt in the same stage.
        (counts(1, 1, 1, 0), "failed"),
        // An open reservation means the stage is still running.
        (counts(1, 1, 0, 1), "running"),
        // Only completed calls remain: provider records say the calls completed.
        (counts(1, 1, 0, 0), "completed"),
    ] {
        let mut attempt = attempt_with_content_stage();
        merge_billing_into_stage_events(
            &mut attempt,
            &[PipelineStageBilling {
                stage_name: "extract_graph".to_string(),
                cost: dec("0.4000"),
                currency: Some("USD".to_string()),
                provider_kind: Some("provider-alpha".to_string()),
                model_name: Some("alpha-chat-large".to_string()),
                started_at: Some(utc("2026-05-12T10:00:20Z")),
                finished_at: Some(utc("2026-05-12T10:00:29Z")),
                elapsed_ms: Some(9000),
                call_counts,
            }],
        );

        let graph_stage = attempt
            .stage_events
            .iter()
            .find(|stage| stage.stage == "extract_graph")
            .expect("materialized graph stage");
        assert_eq!(graph_stage.status, expected_status, "{call_counts:?}");
    }
}

#[test]
fn worker_stage_status_stays_authoritative_over_billing_evidence() {
    // The worker reported `completed` for extract_content; a ledger row for the
    // same stage carries a failed and an open reservation. Billing may add
    // accounting, never change the worker's own stage status.
    let mut attempt = attempt_with_content_stage();

    merge_billing_into_stage_events(
        &mut attempt,
        &[PipelineStageBilling {
            stage_name: "extract_content".to_string(),
            cost: Decimal::ZERO,
            currency: None,
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("alpha-vision".to_string()),
            started_at: Some(utc("2026-05-12T10:00:00Z")),
            finished_at: Some(utc("2026-05-12T10:00:02Z")),
            elapsed_ms: Some(2000),
            call_counts: counts(2, 1, 1, 1),
        }],
    );

    assert_eq!(attempt.stage_events.len(), 1);
    assert_eq!(attempt.stage_events[0].stage, "extract_content");
    assert_eq!(attempt.stage_events[0].status, "completed");
    assert_eq!(attempt.stage_events[0].failed_provider_call_count, Some(1));
    assert_eq!(attempt.stage_events[0].unsettled_provider_call_count, Some(1));
}

#[test]
fn ledger_does_not_overwrite_a_worker_stage_measurement_with_nothing() {
    let mut attempt = attempt_with_content_stage();
    attempt.stage_events[0].estimated_cost = Some(dec("0.0100"));
    attempt.stage_events[0].currency_code = Some("USD".to_string());

    merge_billing_into_stage_events(
        &mut attempt,
        &[PipelineStageBilling {
            stage_name: "extract_content".to_string(),
            cost: Decimal::ZERO,
            currency: None,
            provider_kind: None,
            model_name: None,
            started_at: None,
            finished_at: None,
            elapsed_ms: None,
            call_counts: counts(1, 0, 0, 0),
        }],
    );

    assert_eq!(attempt.stage_events[0].estimated_cost, Some(dec("0.0100")));
    assert_eq!(attempt.stage_events[0].currency_code.as_deref(), Some("USD"));
    assert_eq!(attempt.stage_events[0].cost_coverage, Some(CostCoverageStatus::Unknown));
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
fn stage_message_survives_a_later_stage_row_without_one() {
    let attempt_id = Uuid::now_v7();
    let started = ingest_repository::IngestStageEventRow {
        id: Uuid::now_v7(),
        attempt_id,
        stage_name: "extract_graph".to_string(),
        stage_state: "started".to_string(),
        ordinal: 1,
        message: None,
        details_json: json!({}),
        recorded_at: utc("2026-05-12T10:00:00Z"),
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: None,
        started_at: Some(utc("2026-05-12T10:00:00Z")),
    };
    let failed = ingest_repository::IngestStageEventRow {
        id: Uuid::now_v7(),
        attempt_id,
        stage_name: "extract_graph".to_string(),
        stage_state: "failed".to_string(),
        ordinal: 2,
        message: Some("provider request rejected locally before send".to_string()),
        details_json: json!({}),
        recorded_at: utc("2026-05-12T10:00:05Z"),
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: None,
        started_at: None,
    };

    let stages = merge_stages(&[&started, &failed]);

    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].status, "failed");
    assert_eq!(stages[0].message.as_deref(), Some("provider request rejected locally before send"));
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

fn attempt_with_content_stage() -> DocumentAttempt {
    DocumentAttempt {
        job_id: Uuid::now_v7(),
        attempt_no: 1,
        attempt_kind: "document_upload".to_string(),
        status: "succeeded".to_string(),
        current_stage: None,
        failure_class: None,
        failure_code: None,
        failure_message: None,
        retryable: false,
        total_cost: None,
        currency_code: None,
        queue_started_at: utc("2026-05-12T10:00:00Z"),
        started_at: Some(utc("2026-05-12T10:00:00Z")),
        finished_at: Some(utc("2026-05-12T10:00:30Z")),
        total_elapsed_ms: Some(30000),
        provider_call_count: None,
        unpriced_provider_call_count: None,
        failed_provider_call_count: None,
        unsettled_provider_call_count: None,
        cost_coverage: None,
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
            priced_provider_call_count: None,
            unpriced_provider_call_count: None,
            failed_provider_call_count: None,
            unsettled_provider_call_count: None,
            cost_coverage: None,
            message: None,
            details: None,
        }],
    }
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
        current_stage: None,
        failure_class: None,
        failure_code: None,
        failure_message: None,
        retryable: false,
        total_cost: None,
        currency_code: None,
        queue_started_at,
        started_at: Some(utc(started_at)),
        finished_at: None,
        total_elapsed_ms: None,
        provider_call_count: None,
        unpriced_provider_call_count: None,
        failed_provider_call_count: None,
        unsettled_provider_call_count: None,
        cost_coverage: None,
        stage_events: Vec::new(),
    }
}

fn utc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value).expect("valid timestamp").with_timezone(&Utc)
}
