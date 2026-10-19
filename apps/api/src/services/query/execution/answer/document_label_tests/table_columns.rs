use super::*;

#[test]
fn build_table_row_grounded_answer_lists_schema_columns_for_target_table() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 1 | Column: account_id | Type: UUID | Constraints: PRIMARY KEY | Description: Unique account identifier"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 2 | Column: email | Type: VARCHAR(255) | Constraints: UNIQUE, NOT NULL | Description: Login email"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 2,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(8.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 3 | Column: status | Type: VARCHAR(20) | Constraints: NOT NULL | Description: Account state"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 3,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(99.0),
            source_text:
                "Sheet: Store Schema | Table: 6. Key Indexes | Row 1 | Index Name: idx_accounts_email_lower | Table: accounts | Column: LOWER(email) | Type: UNIQUE, btree"
                    .to_string(),
        },
    ];
    let ir = table_column_inventory_ir("accounts table");

    let answer = build_table_row_grounded_answer(
        "What columns does the accounts table have?",
        Some(&ir),
        &chunks,
    )
    .expect("schema column inventory answer");

    assert!(answer.contains("`account_id`"));
    assert!(answer.contains("`email`"));
    assert!(answer.contains("`status`"));
    assert!(answer.contains("`UUID`"));
    assert!(!answer.contains("LOWER(email)"));
}

#[test]
fn build_table_row_grounded_answer_does_not_list_schema_columns_without_table_ir_targets() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 1 | Column: account_id | Type: UUID | Constraints: PRIMARY KEY | Description: Unique account identifier"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 2 | Column: email | Type: VARCHAR(255) | Constraints: UNIQUE, NOT NULL | Description: Login email"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 2,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(99.0),
            source_text:
                "Sheet: Store Schema | Table: 6. Key Indexes | Row 1 | Index Name: idx_accounts_email_lower | Table: accounts | Column: LOWER(email) | Type: UNIQUE, btree"
                    .to_string(),
        },
    ];
    let ir = describe_query_ir();

    let answer = build_table_row_grounded_answer(
        "What columns does the accounts table have?",
        Some(&ir),
        &chunks,
    );

    if let Some(answer) = answer.as_deref() {
        assert!(!answer.contains("`account_id`"), "{answer}");
        assert!(!answer.contains("`UUID`"), "{answer}");
    }
}

#[test]
fn build_table_row_grounded_answer_lists_schema_columns_for_retrieve_value_ir() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 1 | Column: account_id | Type: UUID | Constraints: PRIMARY KEY | Description: Unique account identifier"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 1,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text:
                "Sheet: Store Schema | Table: 1. Table: accounts | Row 2 | Column: email | Type: VARCHAR(255) | Constraints: UNIQUE, NOT NULL | Description: Login email"
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 2,
            chunk_kind: None,
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(99.0),
            source_text:
                "Sheet: Store Schema | Table: 6. Key Indexes | Row 1 | Index Name: idx_accounts_email_lower | Table: accounts | Column: LOWER(email) | Type: UNIQUE, btree"
                    .to_string(),
        },
    ];
    let ir = retrieve_table_column_inventory_ir("accounts table");

    let answer = build_table_row_grounded_answer(
        "Which columns are in the accounts table?",
        Some(&ir),
        &chunks,
    )
    .expect("schema column inventory answer");

    assert!(answer.contains("`account_id`"));
    assert!(answer.contains("`email`"));
    assert!(answer.contains("`UUID`"));
    assert!(!answer.contains("LOWER(email)"));
}

#[test]
fn build_table_row_grounded_answer_lists_raw_pipe_columns_from_heading_section() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 0,
            chunk_kind: Some("heading".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0),
            source_text: "## 1. Table: accounts".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 1,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(9.0),
            source_text: "| account_id | UUID | PRIMARY KEY | Unique account identifier |"
                .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 2,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(8.0),
            source_text: "| email | VARCHAR(255) | UNIQUE, NOT NULL | Login email |".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 3,
            chunk_kind: Some("heading".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(20.0),
            source_text: "## 2. Table: orders".to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: 4,
            chunk_kind: Some("table_row".to_string()),
            document_id,
            document_label: "schema.pdf".to_string(),
            excerpt: String::new(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(20.0),
            source_text: "| order_id | UUID | PRIMARY KEY | Unique order identifier |".to_string(),
        },
    ];
    let ir = retrieve_table_column_inventory_ir("accounts table");

    let answer = build_table_row_grounded_answer(
        "Which columns are in the accounts table?",
        Some(&ir),
        &chunks,
    )
    .expect("raw pipe column inventory answer");

    assert!(answer.contains("`account_id`"));
    assert!(answer.contains("`email`"));
    assert!(answer.contains("`UUID`"));
    assert!(!answer.contains("order_id"));
}

#[test]
fn build_table_row_grounded_answer_rejects_unmatched_column_inventory_rows() {
    let document_id = Uuid::now_v7();
    let chunks = vec![RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id,
        document_label: "implementation-notes.md".to_string(),
        excerpt: String::new(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(99.0),
        source_text:
            "Sheet: Notes | Table: Route Notes | Row 1 | Column: Application route handlers | Detail: Handler list"
                .to_string(),
    }];
    let ir = QueryIR { act: QueryAct::Enumerate, ..table_column_inventory_ir("accounts table") };

    assert_eq!(
        build_table_row_grounded_answer(
            "Which columns are in the accounts table?",
            Some(&ir),
            &chunks,
        ),
        None
    );
}
