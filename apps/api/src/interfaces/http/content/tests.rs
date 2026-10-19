use super::documents::{
    compose_document_id_filter, ensure_cost_facet_readable, facet_search_hints,
    parse_document_column_filters, parse_document_id_filter, parse_document_status_filter,
    parse_search_ranges,
};
use super::{batch, types};
use crate::domains::content::{
    ContentDocument, ContentDocumentPipelineState, ContentDocumentSummary, ContentRevision,
};
use crate::interfaces::http::router_support::ApiError;
use crate::services::content::service::ReprocessRevisionSource;
use chrono::Utc;
use uuid::Uuid;

#[test]
fn reprocess_metadata_preserves_active_revision_source_kind() {
    let revision = ContentRevision {
        id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_number: 1,
        parent_revision_id: None,
        content_source_kind: "upload".to_string(),
        checksum: "sha256:test".to_string(),
        mime_type: "application/pdf".to_string(),
        byte_size: 636,
        title: Some("runtime-upload-check.pdf".to_string()),
        language_code: Some("ru".to_string()),
        source_uri: Some("upload://runtime-upload-check.pdf".to_string()),
        document_hint: None,
        storage_key: Some("storage/runtime-upload-check.pdf".to_string()),
        created_by_principal_id: None,
        created_at: Utc::now(),
    };

    let metadata = types::build_reprocess_revision_metadata(
        &revision,
        ReprocessRevisionSource {
            file_name: "runtime-upload-check.pdf".to_owned(),
            checksum: revision.checksum.clone(),
            mime_type: revision.mime_type.clone(),
            byte_size: revision.byte_size,
            title: revision.title.clone(),
            source_uri: revision.source_uri.clone(),
            storage_key: revision.storage_key.clone().expect("storage key"),
        },
    );

    assert_eq!(metadata.content_source_kind, "upload");
    assert_eq!(metadata.replacement.checksum, revision.checksum);
    assert_eq!(metadata.replacement.mime_type, revision.mime_type);
    assert_eq!(metadata.replacement.byte_size, revision.byte_size);
    assert_eq!(metadata.replacement.title, revision.title);
    assert_eq!(metadata.replacement.language_code, revision.language_code);
    assert_eq!(metadata.replacement.source_uri, revision.source_uri);
    assert_eq!(metadata.storage_key, revision.storage_key);
}

#[test]
fn reprocess_metadata_preserves_edited_source_storage() {
    let revision = ContentRevision {
        id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_number: 3,
        parent_revision_id: Some(Uuid::now_v7()),
        content_source_kind: "edit".to_string(),
        checksum: "sha256:edited".to_string(),
        mime_type: "text/markdown".to_string(),
        byte_size: 128,
        title: Some("Inventory.xlsx".to_string()),
        language_code: None,
        source_uri: Some("edit://Inventory.md".to_string()),
        document_hint: None,
        storage_key: Some("content/demo/Inventory.md".to_string()),
        created_by_principal_id: None,
        created_at: Utc::now(),
    };

    let metadata = types::build_reprocess_revision_metadata(
        &revision,
        ReprocessRevisionSource {
            file_name: "Inventory.md".to_owned(),
            checksum: revision.checksum.clone(),
            mime_type: revision.mime_type.clone(),
            byte_size: revision.byte_size,
            title: revision.title.clone(),
            source_uri: revision.source_uri.clone(),
            storage_key: revision.storage_key.clone().expect("storage key"),
        },
    );

    assert_eq!(metadata.content_source_kind, "edit");
    assert_eq!(metadata.replacement.mime_type, "text/markdown");
    assert_eq!(metadata.replacement.source_uri.as_deref(), Some("edit://Inventory.md"));
    assert_eq!(metadata.storage_key.as_deref(), Some("content/demo/Inventory.md"));
}

#[test]
fn reprocess_metadata_can_describe_derived_text_source() {
    let revision = ContentRevision {
        id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        revision_number: 2,
        parent_revision_id: None,
        content_source_kind: "upload".to_string(),
        checksum: "sha256:original".to_string(),
        mime_type: "application/pdf".to_string(),
        byte_size: 4096,
        title: Some("quarterly-report.pdf".to_string()),
        language_code: None,
        source_uri: Some("upload://quarterly-report.pdf".to_string()),
        document_hint: None,
        storage_key: None,
        created_by_principal_id: None,
        created_at: Utc::now(),
    };

    let derived_source_uri = format!("derived-text://{}", revision.id);
    let metadata = types::build_reprocess_revision_metadata(
        &revision,
        ReprocessRevisionSource {
            file_name: "quarterly-report.txt".to_owned(),
            checksum: "sha256:derived".to_string(),
            mime_type: "text/plain".to_string(),
            byte_size: 128,
            title: Some("quarterly-report.txt".to_string()),
            source_uri: Some(derived_source_uri.clone()),
            storage_key: "content/derived/quarterly-report.txt".to_string(),
        },
    );

    assert_eq!(metadata.content_source_kind, "upload");
    assert_eq!(metadata.replacement.checksum, "sha256:derived");
    assert_eq!(metadata.replacement.mime_type, "text/plain");
    assert_eq!(metadata.replacement.byte_size, 128);
    assert_eq!(metadata.replacement.title.as_deref(), Some("quarterly-report.txt"));
    assert_eq!(metadata.replacement.source_uri.as_deref(), Some(derived_source_uri.as_str()));
    assert_eq!(metadata.storage_key.as_deref(), Some("content/derived/quarterly-report.txt"));
}

#[test]
fn batch_document_id_limit_allows_persisted_payload_ceiling() {
    assert_eq!(batch::BATCH_MAX_DOCUMENT_IDS, 100_000);
    assert!(batch::ensure_batch_document_id_limit(batch::BATCH_MAX_DOCUMENT_IDS).is_ok());
    assert!(batch::ensure_batch_document_id_limit(1).is_ok());
}

#[test]
fn batch_document_id_limit_rejects_empty_and_oversized_payloads() {
    let empty = batch::ensure_batch_document_id_limit(0)
        .expect_err("empty document id list must be rejected");
    assert!(matches!(empty, ApiError::BadRequest(_)));

    let oversized = batch::ensure_batch_document_id_limit(batch::BATCH_MAX_DOCUMENT_IDS + 1)
        .expect_err("payloads above the DoS sanity limit must fail");
    let ApiError::BadRequest(message) = oversized else {
        unreachable!("ensure_batch_document_id_limit must return bad_request on overflow");
    };
    assert!(message.contains("batch size exceeds maximum"));
}

#[test]
fn document_hint_patch_normalizes_empty_and_accepts_boundary_length() {
    assert_eq!(types::normalize_optional_document_hint(None).unwrap(), None);
    assert_eq!(types::normalize_optional_document_hint(Some("   ".to_string())).unwrap(), None);
    assert_eq!(
        types::normalize_optional_document_hint(Some("  source.pdf  ".to_string())).unwrap(),
        Some("source.pdf".to_string())
    );

    let boundary = "a".repeat(1024);
    assert_eq!(
        types::normalize_optional_document_hint(Some(boundary.clone())).unwrap(),
        Some(boundary)
    );
}

#[test]
fn document_hint_patch_rejects_oversized_payload() {
    let oversized = types::normalize_optional_document_hint(Some("a".repeat(1025)))
        .expect_err("document_hint over 1024 characters must be rejected");
    let ApiError::BadRequest(message) = oversized else {
        unreachable!("document_hint length validation must return bad_request");
    };
    assert!(message.contains("at most 1024 characters"));
}

#[test]
fn document_status_filter_trims_empty_values_and_preserves_requested_statuses() {
    let statuses = parse_document_status_filter(" canceled, ready, ,failed, processing, queued ")
        .expect("known statuses must parse");

    assert_eq!(
        statuses,
        vec![
            ironrag_contracts::documents::DocumentStatus::Canceled,
            ironrag_contracts::documents::DocumentStatus::Ready,
            ironrag_contracts::documents::DocumentStatus::Failed,
            ironrag_contracts::documents::DocumentStatus::Processing,
            ironrag_contracts::documents::DocumentStatus::Queued,
        ]
    );
    assert!(parse_document_status_filter(" , ").unwrap().is_empty());
}

#[test]
fn document_filters_reject_unknown_status_and_malformed_id() {
    let unknown_status =
        parse_document_status_filter("ready,unknown").expect_err("unknown status must be rejected");
    assert!(
        matches!(unknown_status, ApiError::BadRequest(message) if message.contains("unknown status filter value `unknown`"))
    );

    let malformed_id =
        parse_document_id_filter("not-a-uuid").expect_err("malformed document id must be rejected");
    assert!(
        matches!(malformed_id, ApiError::BadRequest(message) if message == "invalid document id `not-a-uuid` in ids filter")
    );
}

#[test]
fn resolved_empty_document_set_stays_empty_instead_of_matching_everything() {
    let requested = Uuid::now_v7();
    let other = Uuid::now_v7();

    // No narrowing at all — the only case that means "every document".
    assert_eq!(compose_document_id_filter(None, None), None);

    // `indexing=vector_gap` on a library with no gap left. Answering this with
    // the whole library is exactly the defect the filter exists to fix.
    assert_eq!(compose_document_id_filter(None, Some(Vec::new())), Some(Vec::new()));

    // A deep link and an indexing filter narrow together, never widen.
    assert_eq!(
        compose_document_id_filter(Some(vec![requested, other]), Some(vec![requested])),
        Some(vec![requested])
    );
    assert_eq!(compose_document_id_filter(Some(vec![other]), Some(vec![requested])), Some(vec![]));
}

#[test]
fn document_id_filter_accepts_whitespace_delimited_uuid_list() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();

    let ids =
        parse_document_id_filter(&format!(" {first},, {second} ")).expect("UUID filter must parse");

    assert_eq!(ids, vec![first, second]);
}

#[test]
fn document_id_filter_rejects_an_explicit_empty_scope() {
    for raw in ["", " ", ",,", " , , "] {
        assert!(matches!(parse_document_id_filter(raw), Err(ApiError::BadRequest(_))));
    }
}

#[test]
fn map_document_summary_uses_persisted_summary_file_name() {
    let document_id = Uuid::now_v7();
    let summary = ContentDocumentSummary {
        document: ContentDocument {
            id: document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: "external-key".to_string(),
            document_state: "active".to_string(),
            created_at: Utc::now(),
        },
        file_name: Some("readable-revision.pdf".to_string()),
        head: None,
        active_revision: Some(ContentRevision {
            id: Uuid::now_v7(),
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            revision_number: 2,
            parent_revision_id: None,
            content_source_kind: "replace".to_string(),
            checksum: "checksum".to_string(),
            mime_type: "application/pdf".to_string(),
            byte_size: 128,
            title: Some("processing-replacement.pdf".to_string()),
            language_code: None,
            source_uri: Some("upload://processing-replacement.pdf".to_string()),
            document_hint: None,
            storage_key: Some("content/demo".to_string()),
            created_by_principal_id: None,
            created_at: Utc::now(),
        }),
        source_access: None,
        readiness: None,
        readiness_summary: None,
        prepared_revision: None,
        web_page_provenance: None,
        pipeline: ContentDocumentPipelineState { latest_mutation: None, latest_job: None },
    };

    let response = types::map_document_summary(summary);

    assert_eq!(response.file_name.as_deref(), Some("readable-revision.pdf"));
}

#[test]
fn document_indexing_filter_wire_values_are_the_exact_subset_names() {
    for (wire, expected) in [
        ("graph_gap", types::DocumentIndexingFilter::GraphGap),
        ("graph_ready", types::DocumentIndexingFilter::GraphReady),
        ("vector_gap", types::DocumentIndexingFilter::VectorGap),
    ] {
        let parsed: types::DocumentIndexingFilter =
            serde_json::from_value(serde_json::json!(wire)).expect("wire value must parse");
        assert_eq!(parsed, expected);
    }
    assert!(
        serde_json::from_value::<types::DocumentIndexingFilter>(serde_json::json!("ready"))
            .is_err(),
        "a derived-status bucket is not an indexing invariant"
    );
}

#[test]
fn document_column_filters_parse_typed_values_and_reject_bad_keys_and_types() {
    // Unknown keys are rejected outright by the strict DTO.
    assert!(
        serde_json::from_str::<types::DocumentColumnFilters>(r#"{"fileNam":["a"]}"#).is_err(),
        "an unknown filter key must be a deserialization error"
    );
    // Wrong element type is rejected by the typed array.
    assert!(
        serde_json::from_str::<types::DocumentColumnFilters>(r#"{"fileSize":["nope"]}"#).is_err(),
        "a wrong filter value type must be a deserialization error"
    );

    // A null element selects the NULL bucket; several values are OR within the
    // column. An absent / empty column imposes no constraint.
    let parsed: types::DocumentColumnFilters = serde_json::from_str(
        r#"{"fileSize":[null,128],"status":["ready","failed"],"fileName":[]}"#,
    )
    .expect("well-typed filter payload");
    let filters = parsed.into_repository().expect("valid typed filters");
    assert!(filters.file_size.present);
    assert!(filters.file_size.include_null);
    assert_eq!(filters.file_size.values, vec![128]);
    assert_eq!(filters.status.values, vec!["ready".to_string(), "failed".to_string()]);
    assert!(!filters.file_name.present, "an empty array means no constraint");
}

#[test]
fn document_column_filters_reject_malformed_timestamp_and_decimal_with_400() {
    let bad_timestamp: types::DocumentColumnFilters =
        serde_json::from_str(r#"{"uploadedAt":["not-a-timestamp"]}"#)
            .expect("string is well typed");
    assert!(matches!(bad_timestamp.into_repository(), Err(ApiError::BadRequest(_))));

    let bad_cost: types::DocumentColumnFilters =
        serde_json::from_str(r#"{"cost":["12,50"]}"#).expect("string is well typed");
    assert!(matches!(bad_cost.into_repository(), Err(ApiError::BadRequest(_))));
}

#[test]
fn document_filters_payload_is_bounded_and_malformed_json_is_400() {
    let oversized = format!(r#"{{"fileName":["{}"]}}"#, "a".repeat(70 * 1024));
    assert!(matches!(
        parse_document_column_filters(Some(&oversized)),
        Err(ApiError::BadRequest(_))
    ));
    assert!(matches!(
        parse_document_column_filters(Some("{not json")),
        Err(ApiError::BadRequest(_))
    ));
    assert!(parse_document_column_filters(None).is_ok());
    assert!(parse_document_column_filters(Some("   ")).is_ok());
}

#[test]
fn facet_cursor_rejects_a_value_that_does_not_match_its_column() {
    let cursor = types::DocumentFacetCursor {
        snapshot_at: Utc::now(),
        column: types::DocumentFacetColumn::FileSize,
        value: Some("text-where-a-number-belongs".to_string()),
        number_value: None,
        value_is_null: false,
    };
    assert!(matches!(cursor.to_repository(), Err(ApiError::BadRequest(_))));
}

#[test]
fn facet_value_serializes_as_string_number_or_null() {
    assert_eq!(
        serde_json::to_value(types::FacetValue::Text("x".to_string())).unwrap(),
        serde_json::json!("x")
    );
    assert_eq!(
        serde_json::to_value(types::FacetValue::Number(128)).unwrap(),
        serde_json::json!(128)
    );
    assert_eq!(serde_json::to_value(types::FacetValue::Null(())).unwrap(), serde_json::Value::Null);
}

#[test]
fn list_cursor_rejects_mismatched_sort_and_malformed_cost_value() {
    let base = types::DocumentListCursor {
        created_at: Utc::now(),
        document_id: Uuid::now_v7(),
        text_value: None,
        number_value: None,
        finished_value: None,
        value_is_null: false,
        sort: types::DocumentListSortKey::UploadedAt,
        sort_desc: true,
        snapshot_at: Utc::now(),
    };
    assert!(base.validate_for_sort(types::DocumentListSortKey::UploadedAt, true).is_ok());
    // A cursor from another sort is rejected before it can be reinterpreted.
    assert!(matches!(
        base.validate_for_sort(types::DocumentListSortKey::Cost, true),
        Err(ApiError::BadRequest(_))
    ));
    assert!(matches!(
        base.validate_for_sort(types::DocumentListSortKey::UploadedAt, false),
        Err(ApiError::BadRequest(_))
    ));
    // A cost cursor whose primary value is not a decimal is a 400, not a 500.
    let malformed_cost = types::DocumentListCursor {
        sort: types::DocumentListSortKey::Cost,
        text_value: Some("12,50".to_string()),
        ..base.clone()
    };
    assert!(matches!(
        malformed_cost.validate_for_sort(types::DocumentListSortKey::Cost, true),
        Err(ApiError::BadRequest(_))
    ));
    // A value in the wrong slot for the sort is rejected too.
    let wrong_shape = types::DocumentListCursor {
        sort: types::DocumentListSortKey::Cost,
        number_value: Some(5),
        ..base
    };
    assert!(wrong_shape.validate_for_sort(types::DocumentListSortKey::Cost, true).is_err());
}

#[test]
fn search_ranges_validate_bounds_and_serialize_canonical_camel_case() {
    // Unknown keys and wrong element types are rejected by the strict DTO.
    assert!(
        serde_json::from_str::<types::DocumentSearchRanges>(r#"{"fileBytes":{"min":0,"max":1}}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<types::DocumentSearchRanges>(r#"{"fileSize":{"min":1.5,"max":2}}"#)
            .is_err()
    );
    // A value outside i64 fails deserialization (not a 500 at the database).
    assert!(
        serde_json::from_str::<types::DocumentSearchRanges>(
            r#"{"fileSize":{"min":0,"max":99999999999999999999}}"#
        )
        .is_err()
    );

    let reversed: types::DocumentSearchRanges =
        serde_json::from_str(r#"{"fileSize":{"min":10,"max":5}}"#).unwrap();
    assert!(matches!(reversed.into_repository(), Err(ApiError::BadRequest(_))));
    let negative: types::DocumentSearchRanges =
        serde_json::from_str(r#"{"pipelineTime":{"min":-1,"max":5}}"#).unwrap();
    assert!(matches!(negative.into_repository(), Err(ApiError::BadRequest(_))));
    let bad_date: types::DocumentSearchRanges =
        serde_json::from_str(r#"{"date":{"min":"not-a-date","max":"2026-01-01T00:00:00Z"}}"#)
            .unwrap();
    assert!(matches!(bad_date.into_repository(), Err(ApiError::BadRequest(_))));

    let ok: types::DocumentSearchRanges = serde_json::from_str(
        r#"{"fileSize":{"min":0,"max":10},"date":{"min":"2026-01-01T00:00:00Z","max":"2026-02-01T00:00:00Z"}}"#,
    )
    .unwrap();
    let repository = ok.into_repository().expect("valid ranges");
    assert_eq!(repository.file_size, Some((0, 10)));
    assert!(repository.date.is_some());

    let canonical = types::DocumentSearchRanges {
        file_size: Some(types::DocumentIntRange { min: 0, max: 10 }),
        pipeline_time: None,
        date: Some(types::DocumentDateRange {
            min: "2026-01-01T00:00:00Z".to_string(),
            max: "2026-02-01T00:00:00Z".to_string(),
        }),
    };
    assert_eq!(
        serde_json::to_value(&canonical).unwrap(),
        serde_json::json!({
            "fileSize": {"min": 0, "max": 10},
            "date": {"min": "2026-01-01T00:00:00Z", "max": "2026-02-01T00:00:00Z"},
        })
    );
}

#[test]
fn search_ranges_payload_is_bounded_and_malformed_json_is_400() {
    assert!(matches!(parse_search_ranges(Some("{not json")), Err(ApiError::BadRequest(_))));
    let oversized =
        format!(r#"{{"date":{{"min":"{}","max":"2026-01-01T00:00:00Z"}}}}"#, "a".repeat(9 * 1024));
    assert!(matches!(parse_search_ranges(Some(&oversized)), Err(ApiError::BadRequest(_))));
    assert!(parse_search_ranges(None).is_ok());
    assert!(parse_search_ranges(Some("   ")).is_ok());
}

#[test]
fn cost_facet_guard_rejects_mixed_currency_scope_and_dirty_rollup() {
    let library = Uuid::now_v7();
    assert!(ensure_cost_facet_readable(library, false, None, false).is_ok());
    // A mixed-currency document anywhere in scope is a conflict even when the
    // returned facet page looks clean.
    assert!(matches!(
        ensure_cost_facet_readable(library, false, None, true),
        Err(ApiError::Conflict(_))
    ));
    // A dirty rollup fails closed before the mixed-currency probe is trusted.
    assert!(ensure_cost_facet_readable(library, true, None, false).is_err());
}

#[test]
fn facet_search_hints_forward_to_own_column_and_reject_wrong_columns() {
    use crate::infra::repositories::content_document_list_repository::DocumentListSearchRanges;
    let size = |min: i64, max: i64| DocumentListSearchRanges {
        file_size: Some((min, max)),
        ..Default::default()
    };

    let hints =
        facet_search_hints(types::DocumentFacetColumn::FileSize, size(1485, 1588), Vec::new())
            .expect("size range forwards to the size facet");
    assert!(hints.statuses.is_empty());
    assert_eq!(hints.range, Some(("1485".to_string(), "1588".to_string())));

    // A range for the wrong column is a clear 400, not a silent no-op.
    assert!(matches!(
        facet_search_hints(types::DocumentFacetColumn::FileName, size(1485, 1588), Vec::new()),
        Err(ApiError::BadRequest(_))
    ));
    // A status hint applies only to the status facet.
    assert!(
        facet_search_hints(
            types::DocumentFacetColumn::FileName,
            DocumentListSearchRanges::default(),
            vec!["ready".to_string()],
        )
        .is_err()
    );
    assert!(
        facet_search_hints(
            types::DocumentFacetColumn::Status,
            DocumentListSearchRanges::default(),
            vec!["ready".to_string()],
        )
        .is_ok()
    );
    // A single range only.
    let multi = DocumentListSearchRanges {
        file_size: Some((0, 1)),
        date: Some((Utc::now(), Utc::now())),
        ..Default::default()
    };
    assert!(matches!(
        facet_search_hints(types::DocumentFacetColumn::FileSize, multi, Vec::new()),
        Err(ApiError::BadRequest(_))
    ));
}
