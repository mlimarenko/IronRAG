#[test]
fn append_auto_key_includes_exact_base_but_explicit_key_keeps_its_namespace() {
    use super::{hash_append_payload, normalize_document_idempotency_key};
    use crate::mcp_types::McpMutationOperationKind;
    use uuid::Uuid;
    let document = Uuid::from_u128(1);
    let base = Uuid::from_u128(2);
    let next_base = Uuid::from_u128(3);
    let first = hash_append_payload(base, "same text");
    let second = hash_append_payload(next_base, "same text");
    assert_ne!(first, second);
    assert_eq!(first, hash_append_payload(base, "same text"));
    let key = |explicit, hash: &str| {
        normalize_document_idempotency_key(
            explicit,
            document,
            &McpMutationOperationKind::Append,
            hash,
        )
    };
    assert_ne!(key(None, &first), key(None, &second));
    assert_eq!(key(Some("caller-key"), &first), key(Some("caller-key"), &second));
}

#[test]
fn replacement_auto_key_binds_all_metadata_scope_intent_and_actual_bytes() {
    use super::*;
    let command = ReplaceInlineMutationCommand {
        workspace_id: Uuid::from_u128(1),
        library_id: Uuid::from_u128(2),
        document_id: Uuid::from_u128(3),
        idempotency_key: None,
        requested_by_principal_id: None,
        request_surface: "mcp".to_owned(),
        source_identity: None,
        file_name: "source.txt".to_owned(),
        title: None,
        language_code: None,
        source_uri: None,
        document_hint: None,
        mime_type: None,
        file_bytes: b"source".to_vec(),
        visibility_intent_id: None,
    };
    let hash = hash_replace_payload(&command).unwrap();
    let mut siblings = Vec::new();
    let mut changed = command.clone();
    changed.title = Some("title".to_owned());
    siblings.push(changed);
    let mut changed = command.clone();
    changed.language_code = Some("en".to_owned());
    siblings.push(changed);
    let mut changed = command.clone();
    changed.source_uri = Some("https://example.com".to_owned());
    siblings.push(changed);
    let mut changed = command.clone();
    changed.document_hint = Some("hint".to_owned());
    siblings.push(changed);
    let mut changed = command.clone();
    changed.file_name = "other.txt".to_owned();
    siblings.push(changed);
    let mut changed = command.clone();
    changed.mime_type = Some("text/markdown".to_owned());
    siblings.push(changed);
    let mut changed = command.clone();
    changed.file_bytes = b"change".to_vec();
    siblings.push(changed);
    let mut changed = command.clone();
    changed.workspace_id = Uuid::from_u128(4);
    siblings.push(changed);
    let mut changed = command.clone();
    changed.library_id = Uuid::from_u128(4);
    siblings.push(changed);
    let mut changed = command.clone();
    changed.document_id = Uuid::from_u128(4);
    siblings.push(changed);
    let mut changed = command.clone();
    changed.visibility_intent_id = Some(Uuid::from_u128(4));
    siblings.push(changed);
    for changed in siblings {
        assert_ne!(hash, hash_replace_payload(&changed).unwrap());
    }
    let mut equivalent = command.clone();
    equivalent.file_name = " source.txt ".to_owned();
    equivalent.mime_type = Some("text/plain".to_owned());
    assert_eq!(hash, hash_replace_payload(&equivalent).unwrap());
    assert_eq!(
        normalize_document_idempotency_key(
            Some("key"),
            command.document_id,
            &McpMutationOperationKind::Replace,
            &hash
        ),
        normalize_document_idempotency_key(
            Some("key"),
            Uuid::from_u128(99),
            &McpMutationOperationKind::Replace,
            "changed"
        )
    );
}

#[test]
fn semantic_hash_has_unambiguous_field_boundaries_and_nulls() {
    use super::hash_semantic_identity;
    use serde_json::json;
    assert_ne!(
        hash_semantic_identity(&json!(["ab", "c"])).unwrap(),
        hash_semantic_identity(&json!(["a", "bc"])).unwrap()
    );
    assert_ne!(
        hash_semantic_identity(&json!([null])).unwrap(),
        hash_semantic_identity(&json!([""])).unwrap()
    );
}

#[test]
fn upload_auto_key_uses_effective_creation_descriptor_and_caller_namespace() {
    use super::*;
    let command = UploadInlineDocumentCommand {
        workspace_id: Uuid::from_u128(1),
        library_id: Uuid::from_u128(2),
        external_key: None,
        idempotency_key: None,
        requested_by_principal_id: None,
        request_surface: "mcp".to_owned(),
        source_identity: None,
        file_name: "source.txt".to_owned(),
        title: None,
        document_hint: None,
        mime_type: None,
        file_bytes: b"body".to_vec(),
        parent_external_key: None,
    };
    let hash = hash_upload_payload(&command).unwrap();
    let mut equivalent = command.clone();
    equivalent.title = Some(" ".to_owned());
    equivalent.mime_type = Some("text/plain".to_owned());
    assert_eq!(hash, hash_upload_payload(&equivalent).unwrap());
    let mut changed = command.clone();
    changed.title = Some("different".to_owned());
    let next = hash_upload_payload(&changed).unwrap();
    assert_ne!(hash, next);
    assert_ne!(
        normalize_upload_idempotency_key(None, command.library_id, 0, &hash),
        normalize_upload_idempotency_key(None, command.library_id, 0, &next)
    );
    assert_eq!(
        normalize_upload_idempotency_key(Some("key"), command.library_id, 0, &hash),
        normalize_upload_idempotency_key(Some("key"), command.library_id, 0, &next)
    );
    assert_ne!(
        normalize_upload_idempotency_key(Some("key"), command.library_id, 0, &hash),
        normalize_upload_idempotency_key(Some("key"), command.library_id, 1, &hash)
    );
}
