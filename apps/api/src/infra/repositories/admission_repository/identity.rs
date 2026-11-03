//! Immutable admission wire representation and canonical fingerprint encoding.

use ironrag_contracts::documents::ReplacementIdentity;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::shared::web::ingest::{
    WebIngestPattern, WebIngestUrlFilter, validate_web_ingest_url_filter,
};

use super::{
    AdmissionError, ContentAdmissionRequest, ContentAdmissionTarget, VisibilityAdmissionRequest,
    WebRunAdmissionRequest,
};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContentIdentity {
    workspace_id: Uuid,
    library_id: Uuid,
    operation_kind: String,
    source_identity: Option<String>,
    materialization_token: Option<String>,
    target: TargetIdentity,
    revision: Option<RevisionIdentity>,
    replacement: Option<ReplacementIdentity>,
}

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum TargetIdentity {
    New {
        external_key: Option<String>,
        file_name: Option<String>,
        parent_external_key: Option<String>,
    },
    Existing {
        document_id: Uuid,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionIdentity {
    content_source_kind: String,
    visibility_intent_id: Option<Uuid>,
    base_revision_id: Option<Uuid>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VisibilityIdentity {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    requested_by_principal_id: Option<Uuid>,
    request_surface: String,
    operation_kind: String,
    source_identity: String,
    materialization_token: Option<String>,
    replacement: ReplacementIdentity,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WebIdentity {
    workspace_id: Uuid,
    library_id: Uuid,
    operation_kind: String,
    source_identity: String,
    seed_url: String,
    normalized_seed_url: String,
    mode: String,
    boundary_policy: String,
    max_depth: i32,
    max_pages: i32,
    crawl_allow_patterns: Vec<WebIngestPattern>,
    crawl_block_patterns: Vec<WebIngestPattern>,
    materialization_allow_patterns: Vec<WebIngestPattern>,
    materialization_block_patterns: Vec<WebIngestPattern>,
}

pub(super) fn content_request_identity(request: &ContentAdmissionRequest) -> Value {
    let target = match &request.target {
        ContentAdmissionTarget::New { external_key, file_name, parent_external_key } => {
            TargetIdentity::New {
                external_key: external_key.clone(),
                file_name: file_name.clone(),
                parent_external_key: parent_external_key.clone(),
            }
        }
        ContentAdmissionTarget::Existing { document_id } => {
            TargetIdentity::Existing { document_id: *document_id }
        }
    };
    serde_json::json!(ContentIdentity {
        workspace_id: request.workspace_id,
        library_id: request.library_id,
        operation_kind: request.operation_kind.clone(),
        source_identity: request.source_identity.clone(),
        materialization_token: None,
        target,
        revision: request.revision.as_ref().map(|revision| RevisionIdentity {
            content_source_kind: revision.content_source_kind.clone(),
            visibility_intent_id: revision.visibility_intent_id,
            base_revision_id: revision.base_revision_id,
        }),
        replacement: request.revision.as_ref().map(|revision| revision.replacement.clone()),
    })
}

pub(super) fn visibility_request_identity(
    request: &VisibilityAdmissionRequest,
    intent: &ironrag_contracts::documents::CreateVisibilityIntentRequest,
) -> Value {
    serde_json::json!(VisibilityIdentity {
        workspace_id: request.workspace_id,
        library_id: request.library_id,
        document_id: request.document_id,
        requested_by_principal_id: request.requested_by_principal_id,
        request_surface: request.request_surface.clone(),
        operation_kind: "visibility".to_owned(),
        source_identity: intent.source_identity.clone(),
        materialization_token: intent.materialization_token.clone(),
        replacement: intent.replacement.clone(),
    })
}

pub(super) fn web_request_identity(
    request: &WebRunAdmissionRequest,
) -> Result<Value, AdmissionError> {
    Ok(serde_json::json!(WebIdentity {
        workspace_id: request.workspace_id,
        library_id: request.library_id,
        operation_kind: "web_capture".to_owned(),
        source_identity: request
            .source_identity
            .as_ref()
            .unwrap_or(&request.normalized_seed_url)
            .clone(),
        seed_url: request.seed_url.clone(),
        normalized_seed_url: request.normalized_seed_url.clone(),
        mode: request.mode.clone(),
        boundary_policy: request.boundary_policy.clone(),
        max_depth: request.max_depth,
        max_pages: request.max_pages,
        crawl_allow_patterns: serde_json::from_value(request.crawl_allow_patterns.clone())?,
        crawl_block_patterns: serde_json::from_value(request.crawl_block_patterns.clone())?,
        materialization_allow_patterns: serde_json::from_value(
            request.materialization_allow_patterns.clone()
        )?,
        materialization_block_patterns: serde_json::from_value(
            request.materialization_block_patterns.clone()
        )?,
    }))
}

fn encode<T: serde::de::DeserializeOwned + Serialize>(
    value: &Value,
) -> Result<Vec<u8>, AdmissionError> {
    let identity: T = serde_json::from_value(value.clone())?;
    // Option fields must be explicitly present, including nested descriptors.
    // Reject missing fields rather than synthesizing nulls at the trust boundary.
    let canonical = serde_json::to_value(&identity)?;
    if canonical != *value {
        return Err(AdmissionError::InvariantViolation("incomplete admission identity"));
    }
    if let Some(replacement) = canonical.get("replacement").filter(|value| !value.is_null()) {
        let descriptor: ReplacementIdentity = serde_json::from_value(replacement.clone())?;
        let normalized = descriptor.clone().normalize().map_err(|_| {
            AdmissionError::InvariantViolation("invalid admission replacement descriptor")
        })?;
        if descriptor != normalized {
            return Err(AdmissionError::InvariantViolation(
                "unnormalized admission replacement descriptor",
            ));
        }
    }
    let mut bytes = Vec::new();
    encode_value(&canonical, &mut bytes)?;
    Ok(bytes)
}

fn encode_value(value: &Value, bytes: &mut Vec<u8>) -> Result<(), AdmissionError> {
    match value {
        Value::Object(map) => {
            bytes.push(b'{');
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key)?;
                bytes.push(b':');
                encode_value(value, bytes)?;
            }
            bytes.push(b'}');
        }
        Value::Array(values) => {
            bytes.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                encode_value(value, bytes)?;
            }
            bytes.push(b']');
        }
        _ => serde_json::to_writer(bytes, value)?,
    }
    Ok(())
}

fn validate_web_filter(
    allow: &[WebIngestPattern],
    block: &[WebIngestPattern],
) -> Result<(), AdmissionError> {
    let filter =
        WebIngestUrlFilter { allow_patterns: allow.to_vec(), block_patterns: block.to_vec() };
    let normalized = validate_web_ingest_url_filter(filter, "admissionFilter")
        .map_err(|_| AdmissionError::InvariantViolation("invalid web admission filter"))?;
    let same = |original: &[WebIngestPattern], normalized: &[WebIngestPattern]| {
        original.len() == normalized.len()
            && original.iter().zip(normalized).all(|(a, b)| a.kind == b.kind && a.value == b.value)
    };
    if !same(allow, &normalized.allow_patterns) || !same(block, &normalized.block_patterns) {
        return Err(AdmissionError::InvariantViolation("unnormalized web admission filter"));
    }
    Ok(())
}

/// Computes the established v1 digest from a complete immutable admission.
/// Object keys are ordered recursively by UTF-8 bytes.
///
/// # Errors
/// Rejects unknown, incomplete or incorrectly typed identities before hashing.
pub fn fingerprint(value: &Value) -> Result<String, AdmissionError> {
    // Web discovery owns seeds and filters; fetched pages own content targets.
    // Select their schema once, never retry a different schema after refusal.
    let encoded = match value["operationKind"].as_str() {
        Some("visibility") => encode::<VisibilityIdentity>(value)?,
        Some("web_capture") if value.get("target").is_none() => {
            let web: WebIdentity = serde_json::from_value(value.clone())?;
            let settings = crate::shared::web::ingest::validate_web_run_settings(
                &web.mode,
                Some(&web.boundary_policy),
                Some(web.max_depth),
                Some(web.max_pages),
            )
            .map_err(|_| AdmissionError::InvariantViolation("invalid web admission settings"))?;
            if settings.max_depth != web.max_depth || settings.max_pages != web.max_pages {
                return Err(AdmissionError::InvariantViolation(
                    "unnormalized web admission settings",
                ));
            }
            let seed = crate::shared::web::url_identity::normalize_seed_url(&web.seed_url)
                .map_err(|_| AdmissionError::InvariantViolation("invalid web admission seed"))?;
            if seed != web.normalized_seed_url {
                return Err(AdmissionError::InvariantViolation("unnormalized web admission seed"));
            }
            crate::shared::web::ingest::validate_web_boundary_seed_host(
                &web.boundary_policy,
                &seed,
            )
            .map_err(|_| AdmissionError::InvariantViolation("invalid web admission boundary"))?;
            validate_web_filter(&web.crawl_allow_patterns, &web.crawl_block_patterns)?;
            validate_web_filter(
                &web.materialization_allow_patterns,
                &web.materialization_block_patterns,
            )?;
            encode::<WebIdentity>(value)?
        }
        Some(
            "upload" | "append" | "replace" | "edit" | "reprocess" | "delete" | "connector_sync"
            | "web_capture",
        ) => encode::<ContentIdentity>(value)?,
        _ => {
            return Err(AdmissionError::InvariantViolation("unknown admission identity operation"));
        }
    };
    Ok(format!("v1:sha256:{}", hex::encode(Sha256::digest(encoded))))
}

/// Verifies that the fingerprint is the digest of the complete identity.
///
/// # Errors
/// Rejects malformed identities and fingerprints that do not match their identity.
pub fn verify_fingerprint(value: &Value, expected: &str) -> Result<(), AdmissionError> {
    if fingerprint(value)? != expected {
        return Err(AdmissionError::InvariantViolation("admission fingerprint mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reverse_objects(value: &mut Value) {
        match value {
            Value::Object(map) => {
                let entries: Vec<_> = std::mem::take(map).into_iter().collect();
                for (key, mut value) in entries.into_iter().rev() {
                    reverse_objects(&mut value);
                    map.insert(key, value);
                }
            }
            Value::Array(values) => values.iter_mut().for_each(reverse_objects),
            _ => {}
        }
    }

    #[test]
    fn canonical_digest_for_complete_content_identity() {
        let identity = serde_json::json!({
            "workspaceId": Uuid::nil(), "libraryId": Uuid::max(),
            "operationKind": "delete", "sourceIdentity": null,
            "materializationToken": null,
            "target": {"kind": "existing", "documentId": Uuid::nil()},
            "revision": null, "replacement": null,
        });
        assert_eq!(
            fingerprint(&identity).unwrap(),
            "v1:sha256:695ae38a614fca04a28804e08bab05da3ad83aa344de8ceaa1b6a5d63b7d5b43"
        );
        assert!(
            verify_fingerprint(
                &identity,
                "v1:sha256:695ae38a614fca04a28804e08bab05da3ad83aa344de8ceaa1b6a5d63b7d5b43"
            )
            .is_ok()
        );
        assert!(
            verify_fingerprint(
                &identity,
                "v1:sha256:695ae38a614fca04a28804e08bab05da3ad83aa344de8ceaa1b6a5d63b7d5b42"
            )
            .is_err()
        );
    }

    #[test]
    fn canonical_json_sorts_recursively_and_escapes() {
        let value =
            serde_json::json!({"é": [1, -2, {"z": "quote\"\\\n\u{0001}/é", "a": 0}], "a": true});
        let mut bytes = Vec::new();
        encode_value(&value, &mut bytes).unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            "{\"a\":true,\"é\":[1,-2,{\"a\":0,\"z\":\"quote\\\"\\\\\\n\\u0001/é\"}]}"
        );
    }

    #[test]
    fn replacement_identity_requires_existing_normalized_form() {
        let mut replacement = serde_json::json!({
            "checksum": format!("sha256:{}", "a".repeat(64)),
            "byteSize": 14, "mimeType": "text/plain", "fileName": "generic.txt",
            "title": "Example", "languageCode": null, "sourceUri": null, "documentHint": null
        });
        let mut identity = serde_json::json!({
            "workspaceId": Uuid::nil(), "libraryId": Uuid::max(),
            "operationKind": "replace", "sourceIdentity": null, "materializationToken": null,
            "target": {"kind": "existing", "documentId": Uuid::nil()},
            "revision": {"contentSourceKind":"upload", "visibilityIntentId":null, "baseRevisionId":null},
            "replacement": replacement
        });
        assert!(fingerprint(&identity).is_ok());
        replacement = identity["replacement"].clone();
        replacement["title"] = serde_json::json!(" Example ");
        identity["replacement"] = replacement.clone();
        assert!(fingerprint(&identity).is_err());
        let normalized: ReplacementIdentity = serde_json::from_value(replacement).unwrap();
        identity["replacement"] = serde_json::to_value(normalized.normalize().unwrap()).unwrap();
        assert!(fingerprint(&identity).is_ok());
        identity["replacement"]["byteSize"] = serde_json::json!(14.0);
        assert!(fingerprint(&identity).is_err());
        identity["replacement"].as_object_mut().unwrap().remove("title");
        assert!(fingerprint(&identity).is_err());
    }

    #[test]
    fn web_filter_uses_normalized_patterns_and_optional_source() {
        let mut identity = serde_json::json!({
            "workspaceId":Uuid::nil(), "libraryId":Uuid::max(), "operationKind":"web_capture",
            "sourceIdentity":"generic", "seedUrl":"https://example.test/", "normalizedSeedUrl":"https://example.test/",
            "mode":"single_page", "boundaryPolicy":"same_host", "maxDepth":0, "maxPages":1,
            "crawlAllowPatterns":[{"kind":"path_prefix","value":"/docs"}], "crawlBlockPatterns":[],
            "materializationAllowPatterns":[], "materializationBlockPatterns":[]
        });
        assert!(fingerprint(&identity).is_ok());
        identity["crawlAllowPatterns"][0]["value"] = serde_json::json!(" /docs ");
        assert!(fingerprint(&identity).is_err());
        identity["crawlAllowPatterns"][0]["value"] = serde_json::json!("/docs");
        identity["crawlAllowPatterns"][0]["kind"] = serde_json::json!("unknown");
        assert!(fingerprint(&identity).is_err());
        identity["crawlAllowPatterns"][0]["kind"] = serde_json::json!("path_prefix");
        identity["crawlAllowPatterns"][0]["source"] = Value::Null;
        assert!(fingerprint(&identity).is_err());
    }

    #[test]
    fn closed_identity_rejects_unknown_and_missing_fields() {
        let identity = serde_json::json!({
            "workspaceId": Uuid::nil(), "libraryId": Uuid::max(),
            "operationKind": "delete", "sourceIdentity": null,
            "materializationToken": null,
            "target": {"kind": "existing", "documentId": Uuid::nil()},
            "revision": null, "replacement": null,
        });
        let expected = fingerprint(&identity).unwrap();
        let mut reordered = identity.clone();
        reverse_objects(&mut reordered);
        assert_eq!(fingerprint(&reordered).unwrap(), expected);
        let mut missing = identity.clone();
        missing.as_object_mut().unwrap().remove("sourceIdentity");
        assert!(fingerprint(&missing).is_err());
        let mut unknown = identity.clone();
        unknown["target"]["unexpected"] = Value::Null;
        assert!(fingerprint(&unknown).is_err());
        let mut changed = identity;
        changed["sourceIdentity"] = Value::String("changed".into());
        assert_ne!(fingerprint(&changed).unwrap(), expected);
        changed["operationKind"] = Value::String("invented".into());
        assert!(fingerprint(&changed).is_err());
    }
}
