use super::*;
use serde_json::{Value, json};

struct ContentArchiveFixture {
    manifest: SnapshotManifest,
    rows: Vec<(&'static str, Value)>,
    intent_id: Uuid,
}

impl ContentArchiveFixture {
    fn new(published: bool) -> Self {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mutation_id = Uuid::now_v7();
        let item_id = Uuid::now_v7();
        let intent_mutation_id = Uuid::now_v7();
        let intent_id = Uuid::now_v7();
        let replacement = json!({"checksum": format!("sha256:{}", "a".repeat(64)), "byteSize": 12,
            "mimeType": "text/plain", "fileName": "safe.txt", "title": null,
            "languageCode": null, "sourceUri": null, "documentHint": null});
        let mut manifest =
            test_support::manifest_with_sections(POSTGRES_CONTENT_TABLES.to_vec(), false);
        manifest.library_id = library_id;
        manifest.postgres_tables.push("knowledge_revision".to_owned());
        let identity = json!({"workspaceId": workspace_id, "libraryId": library_id,
            "documentId": document_id, "operationKind": "visibility", "sourceIdentity": "source-v3",
            "materializationToken": null, "replacement": replacement});
        let payload_identity = json!({"workspaceId": workspace_id, "libraryId": library_id,
            "target": {"kind": "existing", "documentId": document_id}, "operationKind": "replace",
            "revision": {"visibilityIntentId": intent_id},
            "sourceIdentity": "source-v3", "materializationToken": null, "replacement": replacement});
        let mut rows = vec![
            (
                "content_document",
                json!({"id": document_id, "workspace_id": workspace_id,
                "library_id": library_id, "revision_sequence": 3}),
            ),
            (
                "content_revision",
                json!({"id": revision_id, "document_id": document_id,
                "workspace_id": workspace_id, "library_id": library_id, "revision_number": 3,
                "checksum": replacement["checksum"], "byte_size": 12, "mime_type": "text/plain",
                "title": null, "language_code": null, "source_uri": null, "document_hint": null,
                "admission_identity": {"origin": "mutation", "sourceIdentity": "source-v3",
                    "materializationToken": null, "visibilityIntentId": intent_id, "replacement": replacement}}),
            ),
            (
                "content_mutation",
                json!({"id": intent_mutation_id, "workspace_id": workspace_id,
                "library_id": library_id, "operation_kind": "visibility", "mutation_state": "applied",
                "request_identity": identity, "request_fingerprint": identity_digest(&identity)}),
            ),
            (
                "content_mutation_item",
                json!({"id": intent_id, "mutation_id": intent_mutation_id,
                "document_id": document_id, "base_revision_id": null, "result_revision_id": null,
                "visibility_intent_id": null, "item_state": "applied"}),
            ),
            (
                "content_visibility_intent",
                json!({"mutation_item_id": intent_id, "document_id": document_id, "revision_floor": 2}),
            ),
            (
                "content_mutation",
                json!({"id": mutation_id, "workspace_id": workspace_id,
                "library_id": library_id, "operation_kind": "replace", "mutation_state": "applied",
                "request_identity": payload_identity, "request_fingerprint": identity_digest(&payload_identity)}),
            ),
            (
                "content_mutation_item",
                json!({"id": item_id, "mutation_id": mutation_id,
                "document_id": document_id, "result_revision_id": revision_id, "base_revision_id": null,
                "visibility_intent_id": intent_id, "item_state": "applied"}),
            ),
            (
                "content_document_head",
                json!({"document_id": document_id, "visibility_intent_id": intent_id,
                "active_revision_id": if published {Some(revision_id)} else {None},
                "readable_revision_id": if published {Some(revision_id)} else {None},
                "latest_mutation_id": mutation_id}),
            ),
        ];
        rows.push(("knowledge_revision", json!({"revision_id": revision_id, "document_id": document_id,
            "workspace_id": workspace_id, "library_id": library_id, "text_state": "text_readable",
            "text_readable_at": "2026-10-08T00:00:00Z", "vector_state": "ready", "vector_ready_at": "2026-10-08T00:00:00Z",
            "graph_state": "ready"})));
        let references: Vec<_> = rows.iter().filter(|(table, _)| *table == "content_mutation")
            .map(|(_, row)| ("content_mutation_reference", json!({"id": row["id"],
                "workspace_id": workspace_id, "library_id": library_id, "request_surface": "rest",
                "reservation_principal_id": null, "idempotency_scope": null, "idempotency_key": null}))).collect();
        rows.extend(references);
        Self { manifest, rows, intent_id }
    }

    fn with_predecessor(mutation_state: Value, item_state: Option<Value>) -> Self {
        let mut fixture = Self::new(true);
        let id = Uuid::now_v7();
        let scope = fixture.row_mut("content_mutation_reference").clone();
        let audit = json!({"id": id, "workspace_id": scope["workspace_id"], "library_id": scope["library_id"],
            "origin_workspace_id": scope["workspace_id"], "origin_library_id": scope["library_id"],
            "predecessor_baseline_sha384": format!("\\x{}", crate::services::maintenance::content_visibility_intent_schema::PREDECESSOR_BASELINE_SHA384),
            "request_identity": null, "request_fingerprint": null, "operation_kind": "replace", "mutation_state": mutation_state});
        let mut reference = scope;
        reference["id"] = json!(id);
        fixture.rows.push(("content_mutation_reference", reference));
        fixture.rows.push(("content_mutation_predecessor_audit", audit));
        if let Some(item_state) = item_state {
            fixture.rows.push((
                "content_mutation_item",
                json!({
                    "id": Uuid::now_v7(), "mutation_id": id, "document_id": null,
                    "base_revision_id": null, "result_revision_id": null,
                    "visibility_intent_id": null, "item_state": item_state,
                }),
            ));
        }
        fixture
    }

    fn row_mut(&mut self, table: &str) -> &mut Value {
        &mut self.rows.iter_mut().find(|(name, _)| *name == table).unwrap().1
    }

    fn validate(&self) -> anyhow::Result<()> {
        let mut closure = SnapshotContentClosure::default();
        for (table, row) in &self.rows {
            closure.observe(table, row)?;
        }
        closure.validate(&self.manifest)
    }

    fn prune_publication_history(&mut self) {
        let intent_id = self.intent_id.to_string();
        let mutation_id = self
            .rows
            .iter()
            .find(|(table, row)| *table == "content_mutation" && row["operation_kind"] == "replace")
            .unwrap()
            .1["id"]
            .clone();
        self.rows.retain(|(table, row)| {
            !((*table == "content_mutation" || *table == "content_mutation_reference")
                && row["id"] == mutation_id)
                && !(*table == "content_mutation_item" && row["id"] != intent_id)
        });
        self.row_mut("content_document_head")["latest_mutation_id"] = Value::Null;
    }
}

#[test]
fn archive4_bound_readability_survives_pruned_publishing_item_and_absent_attempt() {
    for attempt in [Value::Null, json!(Uuid::now_v7())] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.prune_publication_history();
        fixture.row_mut("content_document_head")["latest_successful_attempt_id"] = attempt;
        fixture.validate().unwrap();
    }
}

#[test]
fn archive4_both_origins_preserve_identity_across_scope_and_storage_relocation() {
    for imported in [false, true] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.prune_publication_history();
        fixture.row_mut("content_document_head")["visibility_intent_id"] = Value::Null;
        let admission = &mut fixture.row_mut("content_revision")["admission_identity"];
        if imported {
            *admission = json!({"origin": "certified_import", "replacement": admission["replacement"],
                "provenance": {"recoverySnapshotId": "a".repeat(64),
                    "identityCertificateSha256": "b".repeat(64), "sourceBaselineSha384": "c".repeat(96),
                    "admittedBy": "snapshot-test-operator"}});
        }
        let original = admission.clone();
        fixture.row_mut("content_revision")["document_hint"] = json!("updated hint");
        let source_library = fixture.manifest.library_id;
        let source_workspace =
            required_uuid_field("document", fixture.row_mut("content_document"), "workspace_id")
                .unwrap();
        fixture.row_mut("content_revision")["storage_key"] =
            json!(format!("content/{source_workspace}/{source_library}/source.txt"));
        fixture.validate().unwrap();
        for _ in 0..2 {
            let target_library = Uuid::now_v7();
            let mut scope = SnapshotRowScope::new(
                fixture.manifest.library_id,
                target_library,
                Uuid::now_v7(),
                None,
            );
            // The archive fixture follows the production content table order.
            for table in POSTGRES_CONTENT_TABLES.iter().copied().chain(["knowledge_revision"]) {
                for (_, row) in fixture.rows.iter_mut().filter(|(name, _)| *name == table) {
                    scope.normalize_postgres_row(table, row).unwrap();
                }
            }
            fixture.manifest.library_id = target_library;
            fixture.validate().unwrap();
            assert_eq!(fixture.row_mut("content_revision")["admission_identity"], original);
        }
    }
}

#[test]
fn archive4_revision_admission_binding_fails_closed() {
    for field in ["visibilityIntentId", "sourceIdentity", "materializationToken", "replacement"] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.prune_publication_history();
        fixture.row_mut("content_revision")["admission_identity"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(fixture.validate().is_err(), "missing {field}");
    }
    for field in ["sourceIdentity", "materializationToken"] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.prune_publication_history();
        fixture.row_mut("content_revision")["admission_identity"][field] = json!("forged");
        assert!(fixture.validate().is_err(), "forged {field}");
    }
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.prune_publication_history();
    let admission = &mut fixture.row_mut("content_revision")["admission_identity"];
    *admission = json!({"origin": "certified_import", "replacement": admission["replacement"],
        "provenance": {"recoverySnapshotId": "a".repeat(64), "identityCertificateSha256": "b".repeat(64),
            "sourceBaselineSha384": "c".repeat(96), "admittedBy": "snapshot-test-operator"}});
    assert!(fixture.validate().unwrap_err().to_string().contains("not bound"));
}

#[test]
fn archive4_bound_admission_requires_exact_descriptor_floor_and_ready_state_without_history() {
    for field in ["checksum", "fileName", "documentHint"] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.prune_publication_history();
        fixture.row_mut("content_revision")["admission_identity"]["replacement"][field] =
            if field == "checksum" {
                json!(format!("sha256:{}", "b".repeat(64)))
            } else {
                json!("changed")
            };
        assert!(fixture.validate().is_err(), "forged {field}");
    }
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.prune_publication_history();
    fixture.row_mut("content_visibility_intent")["revision_floor"] = json!(3);
    assert!(fixture.validate().unwrap_err().to_string().contains("above intent floor"));
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.prune_publication_history();
    fixture.row_mut("content_revision")["document_hint"] = json!("changed");
    assert!(fixture.validate().is_err());
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.prune_publication_history();
    fixture.row_mut("knowledge_revision")["graph_state"] = json!("pending");
    assert!(fixture.validate().unwrap_err().to_string().contains("not READY"));
}

#[test]
fn archive4_null_admission_is_only_valid_for_nonreadable_history() {
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("content_revision")["admission_identity"] = Value::Null;
    fixture.validate().unwrap();
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_revision")["admission_identity"] = Value::Null;
    assert!(fixture.validate().is_err());
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("content_revision").as_object_mut().unwrap().remove("admission_identity");
    assert!(fixture.validate().is_err());
}

#[test]
fn archive4_missing_nullable_revision_metadata_and_ready_timestamps_fail_closed() {
    for field in ["title", "language_code", "source_uri"] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.row_mut("content_revision").as_object_mut().unwrap().remove(field);
        assert!(fixture.validate().is_err());
    }
    for field in ["text_readable_at", "vector_ready_at"] {
        let mut fixture = ContentArchiveFixture::new(true);
        fixture.row_mut("knowledge_revision").as_object_mut().unwrap().remove(field);
        assert!(fixture.validate().is_err());
    }
}

#[test]
fn predecessor_terminal_states_are_typed_by_row_kind() {
    for (state, mutation, item) in [
        ("applied", true, true),
        ("failed", true, true),
        ("conflicted", true, true),
        ("canceled", true, false),
        ("skipped", false, true),
        ("pending", false, false),
        ("accepted", false, false),
        ("running", false, false),
    ] {
        assert_eq!(
            ContentArchiveFixture::with_predecessor(json!(state), Some(json!("skipped")))
                .validate()
                .is_ok(),
            mutation,
            "mutation {state}",
        );
        assert_eq!(
            ContentArchiveFixture::with_predecessor(json!("canceled"), Some(json!(state)))
                .validate()
                .is_ok(),
            item,
            "item {state}",
        );
    }
    for invalid in [Value::Null, json!(0), json!({"applied": null}), json!(["applied"])] {
        assert!(
            ContentArchiveFixture::with_predecessor(invalid.clone(), Some(json!("skipped")))
                .validate()
                .is_err()
        );
        assert!(
            ContentArchiveFixture::with_predecessor(json!("canceled"), Some(invalid))
                .validate()
                .is_err()
        );
    }
}

#[test]
fn archive3_predecessor_audit_is_explicit_terminal_nonpublished_and_certified() {
    let mut fixture = ContentArchiveFixture::with_predecessor(json!("applied"), None);
    fixture.validate().unwrap();
    fixture.row_mut("content_mutation_predecessor_audit")["mutation_state"] = json!("running");
    assert!(fixture.validate().is_err());
    fixture.row_mut("content_mutation_predecessor_audit")["mutation_state"] = json!("applied");
    fixture.row_mut("content_mutation_predecessor_audit")["predecessor_baseline_sha384"] =
        json!("\\x00");
    assert!(fixture.validate().is_err());
}

#[test]
fn archive3_registry_refuses_missing_or_dual_representation() {
    let mut fixture = ContentArchiveFixture::new(false);
    let modern = fixture.row_mut("content_mutation").clone();
    fixture.rows.push(("content_mutation_predecessor_audit", modern));
    assert!(fixture.validate().unwrap_err().to_string().contains("exactly one"));
    fixture.rows.retain(|(table, row)| {
        !(*table == "content_mutation_predecessor_audit"
            || (*table == "content_mutation" && row["operation_kind"] == "visibility"))
    });
    assert!(fixture.validate().unwrap_err().to_string().contains("exactly one"));
}

#[test]
fn archive3_unpublished_knowledge_revision_requires_exact_local_content_owner() {
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("knowledge_revision")["revision_id"] = json!(Uuid::now_v7());
    assert!(fixture.validate().is_err());
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("knowledge_revision")["workspace_id"] = json!(Uuid::now_v7());
    assert!(fixture.validate().is_err());
}

#[test]
fn archive4_predecessor_audit_is_not_read_authority_for_admitted_revision() {
    let mut fixture = ContentArchiveFixture::new(true);
    let (table, payload) = fixture
        .rows
        .iter_mut()
        .find(|(table, row)| *table == "content_mutation" && row["operation_kind"] == "replace")
        .unwrap();
    *table = "content_mutation_predecessor_audit";
    payload["request_identity"] = Value::Null;
    payload["origin_workspace_id"] = payload["workspace_id"].clone();
    payload["origin_library_id"] = payload["library_id"].clone();
    payload["predecessor_baseline_sha384"] = json!(format!(
        "\\x{}",
        crate::services::maintenance::content_visibility_intent_schema::PREDECESSOR_BASELINE_SHA384
    ));
    let (_, item) = fixture
        .rows
        .iter_mut()
        .find(|(table, row)| {
            *table == "content_mutation_item" && row["result_revision_id"].is_string()
        })
        .unwrap();
    item["visibility_intent_id"] = Value::Null;
    fixture.row_mut("content_document_head")["visibility_intent_id"] = Value::Null;
    let replacement =
        fixture.row_mut("content_revision")["admission_identity"]["replacement"].clone();
    fixture.row_mut("content_revision")["admission_identity"] = json!({
        "origin": "certified_import", "replacement": replacement,
        "provenance": {"recoverySnapshotId": "a".repeat(64),
            "identityCertificateSha256": "b".repeat(64), "sourceBaselineSha384": "c".repeat(96),
            "admittedBy": "snapshot-test-operator"}
    });
    fixture.validate().unwrap();
    fixture.row_mut("content_document_head")["readable_revision_id"] = Value::Null;
    fixture.row_mut("content_revision")["admission_identity"] = Value::Null;
    fixture.validate().unwrap();
}

fn identity_digest(identity: &Value) -> String {
    format!("v1:sha256:{}", hex::encode(Sha256::digest(serde_json::to_vec(identity).unwrap())))
}

#[test]
fn archive3_accepts_unresolved_intent_with_retained_payload() {
    ContentArchiveFixture::new(false).validate().unwrap();
}

#[test]
fn archive3_accepts_published_exact_bound_ready_item() {
    ContentArchiveFixture::new(true).validate().unwrap();
}

#[test]
fn archive3_accepts_fresh_ready_without_visibility_obligation() {
    let mut fixture = ContentArchiveFixture::new(true);
    let removed_id = fixture.row_mut("content_mutation")["id"].clone();
    fixture.rows.retain(|(table, row)| {
        *table != "content_visibility_intent"
            && !(*table == "content_mutation_reference" && row["id"] == removed_id)
            && !(*table == "content_mutation" && row["operation_kind"] == "visibility")
            && !(*table == "content_mutation_item" && row["id"] == fixture.intent_id.to_string())
    });
    fixture.row_mut("content_document_head")["visibility_intent_id"] = Value::Null;
    fixture.row_mut("content_mutation_item")["visibility_intent_id"] = Value::Null;
    fixture.validate().unwrap();
}

#[test]
fn archive3_preserves_unbound_web_run_identity_without_inventing_page_descriptor() {
    let mut fixture = ContentArchiveFixture::new(true);
    let removed_id = fixture.row_mut("content_mutation")["id"].clone();
    fixture.rows.retain(|(table, row)| {
        *table != "content_visibility_intent"
            && !(*table == "content_mutation_reference" && row["id"] == removed_id)
            && !(*table == "content_mutation" && row["operation_kind"] == "visibility")
            && !(*table == "content_mutation_item" && row["id"] == fixture.intent_id.to_string())
    });
    fixture.row_mut("content_document_head")["visibility_intent_id"] = Value::Null;
    fixture.row_mut("content_mutation_item")["visibility_intent_id"] = Value::Null;
    let mutation = fixture.row_mut("content_mutation");
    let original_scope = mutation["request_identity"].clone();
    mutation["operation_kind"] = json!("web_capture");
    mutation["request_identity"] = json!({"workspaceId": original_scope["workspaceId"],
        "libraryId": original_scope["libraryId"], "seedUrl": "https://example.test/seed",
        "mode": "single_page"});
    mutation["request_fingerprint"] = json!(identity_digest(&mutation["request_identity"]));
    fixture.validate().unwrap();
}

#[test]
fn archive3_rejects_missing_identity_and_modified_fingerprint() {
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("content_mutation")["request_identity"] = Value::Null;
    assert!(fixture.validate().unwrap_err().to_string().contains("missing immutable"));
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("content_mutation")["request_fingerprint"] = json!("wrong");
    assert!(fixture.validate().unwrap_err().to_string().contains("fingerprint mismatch"));
}

#[test]
fn archive3_rejects_retired_policy_columns_and_identity_inputs() {
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("content_mutation")["visibility_policy"] = json!("readable");
    assert!(fixture.validate().unwrap_err().to_string().contains("retired policy column"));
    let mut fixture = ContentArchiveFixture::new(false);
    let mutation = fixture.row_mut("content_mutation");
    mutation["request_identity"]["visibilityPolicy"] = json!("readable");
    mutation["request_fingerprint"] = json!(identity_digest(&mutation["request_identity"]));
    assert!(fixture.validate().unwrap_err().to_string().contains("retired policy identity input"));
}

#[test]
fn archive3_rejects_cross_scope_and_cross_document_references() {
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_revision")["library_id"] = json!(Uuid::now_v7());
    assert!(fixture.validate().is_err());
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_visibility_intent")["document_id"] = json!(Uuid::now_v7());
    assert!(fixture.validate().is_err());
}

#[test]
fn archive3_rejects_counter_and_floor_mismatches() {
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_document")["revision_sequence"] = json!(1);
    assert!(fixture.validate().unwrap_err().to_string().contains("retained revision counter"));
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_visibility_intent")["revision_floor"] = json!(3);
    assert!(fixture.validate().unwrap_err().to_string().contains("above intent floor"));
}

#[test]
fn archive3_rejects_missing_and_ambiguous_intent_lineage() {
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.rows.retain(|(table, _)| *table != "content_visibility_intent");
    assert!(fixture.validate().is_err());
    let mut fixture = ContentArchiveFixture::new(false);
    let mut sibling = fixture.row_mut("content_mutation_item").clone();
    sibling["id"] = json!(Uuid::now_v7());
    fixture.rows.push(("content_mutation_item", sibling));
    assert!(fixture.validate().unwrap_err().to_string().contains("ambiguous lineage"));
}

#[test]
fn archive3_rejects_forged_published_owner_and_unready_projection() {
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_revision")["admission_identity"]["visibilityIntentId"] =
        json!(Uuid::now_v7());
    assert!(fixture.validate().is_err());
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("knowledge_revision")["vector_state"] = json!("pending");
    assert!(fixture.validate().unwrap_err().to_string().contains("not READY"));
}

#[test]
fn archive3_rejects_duplicate_identity_and_incomplete_inventory() {
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.rows.push(fixture.rows[0].clone());
    assert!(fixture.validate().unwrap_err().to_string().contains("duplicate identity"));
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.manifest.postgres_tables.retain(|table| table != "content_visibility_intent");
    assert!(
        fixture.validate().unwrap_err().to_string().contains("complete content table inventory")
    );
}

#[test]
fn archive3_rejects_changed_payload_binding_and_missing_counter() {
    let mut fixture = ContentArchiveFixture::new(true);
    let (_, payload) = fixture
        .rows
        .iter_mut()
        .find(|(table, row)| *table == "content_mutation" && row["operation_kind"] == "replace")
        .unwrap();
    payload["request_identity"]["replacement"]["fileName"] = json!("other.txt");
    payload["request_fingerprint"] = json!(identity_digest(&payload["request_identity"]));
    assert!(fixture.validate().unwrap_err().to_string().contains("binding mismatch"));
    let mut fixture = ContentArchiveFixture::new(false);
    fixture.row_mut("content_document").as_object_mut().unwrap().remove("revision_sequence");
    assert!(fixture.validate().unwrap_err().to_string().contains("revision_sequence"));
}

#[tokio::test]
async fn archive3_prevalidation_refuses_semantically_malformed_but_digest_correct_archive() {
    let mut fixture = ContentArchiveFixture::new(true);
    fixture.row_mut("content_revision")["admission_identity"] = Value::Null;
    let mut bytes = Vec::new();
    let mut builder = Builder::new(ZstdEncoder::new(&mut bytes));
    append_json_entry(&mut builder, "manifest.json", &fixture.manifest).await.unwrap();
    let mut inventory = SnapshotInventory::default();
    for table in &fixture.manifest.postgres_tables {
        let mut part = Vec::new();
        let mut count = 0;
        for (_, row) in fixture.rows.iter().filter(|(name, _)| *name == table) {
            part.extend_from_slice(&serde_json::to_vec(row).unwrap());
            part.push(b'\n');
            count += 1;
        }
        inventory.postgres_row_counts.insert(table.clone(), count);
        if !part.is_empty() {
            let path = format!("postgres/{table}/part-000001.ndjson");
            inventory
                .postgres_parts
                .push(append_raw_entry(&mut builder, &path, &part).await.unwrap());
        }
    }
    append_json_entry(&mut builder, "summary.json", &SnapshotSummary { inventory }).await.unwrap();
    let mut writer = builder.into_inner().await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut writer).await.unwrap();
    drop(writer);
    let spool = spool_snapshot_archive(bytes.as_slice()).await.unwrap();
    assert!(
        prevalidate_snapshot_archive(&spool)
            .await
            .unwrap_err()
            .to_string()
            .contains("lacks admission identity")
    );
}
