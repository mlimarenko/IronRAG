use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::{
    ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR, TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR,
    WebhookDeliveryAttemptRow, WebhookDeliveryAttemptViewRow, WebhookSubscriptionViewRow,
    is_active_webhook_subscription_quota_error, is_total_webhook_subscription_quota_error,
};

#[test]
fn active_subscription_quota_has_a_narrow_typed_classifier() {
    let quota = sqlx::Error::Protocol(ACTIVE_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR.to_string());
    let unrelated = sqlx::Error::Protocol("unrelated".to_string());

    assert!(is_active_webhook_subscription_quota_error(&quota));
    assert!(!is_active_webhook_subscription_quota_error(&unrelated));
}

#[test]
fn total_subscription_quota_has_a_narrow_typed_classifier() {
    let quota = sqlx::Error::Protocol(TOTAL_WEBHOOK_SUBSCRIPTION_QUOTA_ERROR.to_string());
    let unrelated = sqlx::Error::Protocol("unrelated".to_string());

    assert!(is_total_webhook_subscription_quota_error(&quota));
    assert!(!is_total_webhook_subscription_quota_error(&unrelated));
}

#[test]
fn management_projection_debug_redacts_remote_urls_and_errors() {
    let now = Utc::now();
    let subscription = WebhookSubscriptionViewRow {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: None,
        display_name: "synthetic".to_string(),
        target_url: "https://example.invalid/?token=subscription-secret".to_string(),
        event_types: vec!["revision.ready".to_string()],
        active: true,
        created_at: now,
        updated_at: now,
    };
    let attempt = WebhookDeliveryAttemptViewRow {
        id: Uuid::now_v7(),
        subscription_id: subscription.id,
        event_type: "revision.ready".to_string(),
        event_id: Uuid::now_v7().to_string(),
        target_url: "https://example.invalid/?token=attempt-secret".to_string(),
        attempt_number: 1,
        delivery_state: "failed".to_string(),
        response_status: Some(500),
        error_message: Some("remote-error-secret".to_string()),
        delivered_at: None,
        next_attempt_at: None,
        created_at: now,
    };

    let debug = format!("{subscription:?} {attempt:?}");
    for secret in ["subscription-secret", "attempt-secret", "remote-error-secret"] {
        assert!(!debug.contains(secret), "Debug exposed {secret}");
    }
}

#[test]
fn delivery_attempt_debug_redacts_remote_and_payload_content() {
    let secret_payload = "synthetic-payload-secret";
    let secret_target = "https://example.invalid/private?token=synthetic-query-secret";
    let secret_response = "synthetic-response-secret";
    let secret_error = "synthetic-error-secret";
    let lease_token = Uuid::now_v7();
    let now = Utc::now();
    let row = WebhookDeliveryAttemptRow {
        id: Uuid::now_v7(),
        subscription_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Some(Uuid::now_v7()),
        event_type: "revision.ready".to_string(),
        event_id: Uuid::now_v7().to_string(),
        occurred_at: now,
        payload_json: json!({ "secret": secret_payload }),
        target_url: secret_target.to_string(),
        attempt_number: 1,
        delivery_state: "delivering".to_string(),
        response_status: None,
        response_body_excerpt: Some(secret_response.to_string()),
        error_code: Some("transport_request".to_string()),
        error_message: Some(secret_error.to_string()),
        job_id: Some(Uuid::now_v7()),
        delivery_lease_token: Some(lease_token),
        next_attempt_at: None,
        delivered_at: None,
        created_at: now,
        updated_at: now,
    };

    let debug = format!("{row:?}");
    let lease_token_text = lease_token.to_string();
    for secret in
        [secret_payload, secret_target, secret_response, secret_error, lease_token_text.as_str()]
    {
        assert!(!debug.contains(secret), "Debug exposed {secret}");
    }
}
