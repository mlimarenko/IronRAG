use chrono::{DateTime, Utc};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{WebhookEvent, document_deleted_event_id, revision_ready_event_id};

    #[test]
    fn lifecycle_event_ids_are_deterministic_per_persisted_transition() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let first_deleted_at = Utc::now();
        let second_deleted_at = first_deleted_at + chrono::Duration::microseconds(1);

        assert_eq!(revision_ready_event_id(revision_id), revision_ready_event_id(revision_id));
        assert_eq!(
            document_deleted_event_id(document_id, first_deleted_at),
            document_deleted_event_id(document_id, first_deleted_at),
        );
        assert_ne!(
            document_deleted_event_id(document_id, first_deleted_at),
            document_deleted_event_id(document_id, second_deleted_at),
            "a later delete transition must receive a distinct event identity",
        );
    }

    #[test]
    fn canonical_delivery_payload_overrides_spoofed_metadata() {
        let event = WebhookEvent {
            event_type: "revision.ready".into(),
            event_id: "revision.ready:stable".into(),
            occurred_at: Utc::now(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            payload_json: serde_json::json!({
                "event_id": "spoofed",
                "document_id": Uuid::now_v7(),
            }),
        };

        let payload = event.canonical_delivery_payload().expect("object payload");
        assert_eq!(payload["event_id"], event.event_id);
        assert_eq!(payload["event_type"], event.event_type);
        assert_eq!(payload["workspace_id"], event.workspace_id.to_string());
        assert!(payload.get("document_id").is_some());

        let mut invalid = event;
        invalid.payload_json = serde_json::json!(["not", "an", "object"]);
        assert!(invalid.canonical_delivery_payload().is_none());
    }
}

/// Outbound event that will be fanned out to matching subscriptions.
#[derive(Clone)]
pub struct WebhookEvent {
    pub event_type: String,
    pub event_id: String,
    /// Immutable producer-side occurrence time persisted with the outbox row.
    pub occurred_at: DateTime<Utc>,
    pub workspace_id: Uuid,
    /// Canonical queue ownership. Outbound events are emitted by document or
    /// revision lifecycle transitions, both of which always belong to a
    /// library. Keeping this non-optional prevents invalid nil-library jobs.
    pub library_id: Uuid,
    pub payload_json: serde_json::Value,
}

impl std::fmt::Debug for WebhookEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookEvent")
            .field("event_type", &self.event_type)
            .field("event_id", &self.event_id)
            .field("occurred_at", &self.occurred_at)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("payload_json", &"<redacted>")
            .finish()
    }
}

impl WebhookEvent {
    /// Builds the flat signed outbound contract. Canonical metadata always
    /// overwrites same-named payload keys so producers cannot spoof routing or
    /// dedupe identity inside the signed body.
    #[must_use]
    pub fn canonical_delivery_payload(&self) -> Option<serde_json::Value> {
        self.payload_json.as_object()?;
        Some(canonical_delivery_payload_from_parts(
            &self.payload_json,
            &self.event_type,
            &self.event_id,
            self.occurred_at,
            self.workspace_id,
            self.library_id,
        ))
    }
}

/// Rebuilds the signed flat payload from trusted relational metadata.
///
/// Delivery calls this again even though new attempts already persist a
/// canonical payload. That makes legacy attempts safe and prevents a stored
/// JSON field from overriding routing, tenant, identity or occurrence data.
#[must_use]
pub fn canonical_delivery_payload_from_parts(
    payload_json: &serde_json::Value,
    event_type: &str,
    event_id: &str,
    occurred_at: DateTime<Utc>,
    workspace_id: Uuid,
    library_id: Uuid,
) -> serde_json::Value {
    let mut payload = payload_json.as_object().cloned().unwrap_or_default();
    payload.insert("event_type".to_string(), serde_json::json!(event_type));
    payload.insert("event_id".to_string(), serde_json::json!(event_id));
    payload.insert("occurred_at".to_string(), serde_json::Value::String(occurred_at.to_rfc3339()));
    payload.insert("workspace_id".to_string(), serde_json::json!(workspace_id));
    payload.insert("library_id".to_string(), serde_json::json!(library_id));
    serde_json::Value::Object(payload)
}

/// Stable identity for the one `revision.ready` transition owned by a
/// canonical revision. Worker retries must reuse this value so queue dedupe is
/// effective across process crashes.
#[must_use]
pub fn revision_ready_event_id(revision_id: Uuid) -> String {
    format!("revision.ready:{revision_id}")
}

/// Stable identity for one persisted document-delete transition.
///
/// `deleted_at` is part of the canonical document row and is locked before the
/// event is created. A retry of the same transition reuses the same identity,
/// while a later restore/delete cycle receives a different one.
#[must_use]
pub fn document_deleted_event_id(document_id: Uuid, deleted_at: DateTime<Utc>) -> String {
    format!("document.deleted:{document_id}:{}", deleted_at.timestamp_micros())
}
