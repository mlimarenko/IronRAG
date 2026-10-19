use uuid::Uuid;

use super::code_boundaries::{detect_code_boundaries, guess_language};
use super::quality::{compute_simhash, mark_near_duplicates};
use super::{
    StructuredChunkingProfile, build_structured_chunk_windows, compute_window_text_pass,
    split_large_code_blocks,
};
use crate::shared::extraction::structured_document::{
    StructuredBlockData, StructuredBlockKind, StructuredChunkWindow,
};

fn make_block(
    ordinal: i32,
    kind: StructuredBlockKind,
    text: &str,
    is_boilerplate: bool,
) -> StructuredBlockData {
    StructuredBlockData {
        block_id: Uuid::now_v7(),
        ordinal,
        block_kind: kind,
        text: text.to_string(),
        normalized_text: text.to_string(),
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        source_span: None,
        parent_block_id: None,
        table_coordinates: None,
        code_language: None,
        is_boilerplate,
    }
}

fn make_paragraph(ordinal: i32, char_count: usize) -> StructuredBlockData {
    let text: String = "abcdefghij ".repeat(char_count / 11 + 1).chars().take(char_count).collect();
    make_block(ordinal, StructuredBlockKind::Paragraph, &text, false)
}

fn make_chunk(chunk_index: i32, content_text: &str) -> StructuredChunkWindow {
    StructuredChunkWindow {
        chunk_index,
        chunk_kind: StructuredBlockKind::Paragraph,
        support_block_ids: vec![Uuid::now_v7()],
        content_text: content_text.to_string(),
        normalized_text: content_text.to_string(),
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        token_count: None,
        literal_digest: None,
        quality_score: 1.0,
        simhash_fingerprint: None,
        is_near_duplicate: false,
        window_text: None,
    }
}

#[test]
fn overlap_produces_shared_blocks_between_chunks() {
    // Use 10 blocks of ~200 chars each (total ~2000+). With max_chars=1200, we get 2+ chunks.
    // Each block is 200 chars, so overlap_chars=300 can include at least one trailing block.
    let blocks: Vec<StructuredBlockData> = (0..10).map(|i| make_paragraph(i, 200)).collect();

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 1200, overlap_chars: 300 },
    );

    assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());

    let first_ids: std::collections::HashSet<Uuid> =
        chunks[0].support_block_ids.iter().copied().collect();
    let second_ids: std::collections::HashSet<Uuid> =
        chunks[1].support_block_ids.iter().copied().collect();
    let shared: Vec<_> = first_ids.intersection(&second_ids).collect();
    assert!(!shared.is_empty(), "overlap should produce at least one shared block between chunks");
}

#[test]
fn heading_starts_new_chunk_when_window_has_content() {
    let blocks = vec![
        make_paragraph(0, 500),
        make_paragraph(1, 500),
        make_block(2, StructuredBlockKind::Heading, "Section 2", false),
        make_paragraph(3, 500),
    ];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 2, "heading should force a split: got {} chunks", chunks.len());
    assert_eq!(
        chunks[1].support_block_ids[0], blocks[2].block_id,
        "second chunk should start with the heading block"
    );
}

#[test]
fn boilerplate_blocks_are_filtered_from_chunks() {
    let blocks = vec![
        make_block(0, StructuredBlockKind::Paragraph, "Normal paragraph text here.", false),
        make_block(1, StructuredBlockKind::Paragraph, "This is boilerplate content.", true),
        make_block(2, StructuredBlockKind::Paragraph, "Another normal paragraph.", false),
    ];

    let boilerplate_id = blocks[1].block_id;
    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    for chunk in &chunks {
        assert!(
            !chunk.support_block_ids.contains(&boilerplate_id),
            "boilerplate block_id must not appear in any chunk's support_block_ids"
        );
    }
}

#[test]
fn quality_score_rewards_code_and_headings() {
    let blocks = vec![
        make_block(
            0,
            StructuredBlockKind::CodeBlock,
            "fn main() { println!(\"hello world\"); } // some extra padding text to reach minimum length requirement for quality scoring",
            false,
        ),
        make_block(1, StructuredBlockKind::Heading, "Getting Started Guide", false),
    ];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 1);
    // The quality function adds +0.1 for code and +0.1 for heading, but clamps to 1.0 max.
    // So with code + heading the score should be exactly 1.0 (the clamped maximum).
    assert!(
        chunks[0].quality_score >= 1.0,
        "code + heading should give quality_score >= 1.0, got {}",
        chunks[0].quality_score
    );
}

#[test]
fn quality_score_penalizes_short_content() {
    let blocks = vec![make_block(0, StructuredBlockKind::Paragraph, "Very short text.", false)];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 1);
    assert!(
        chunks[0].quality_score < 1.0,
        "short content should give quality_score < 1.0, got {}",
        chunks[0].quality_score
    );
}

#[test]
fn simhash_fingerprint_is_computed() {
    let blocks = vec![make_paragraph(0, 200), make_paragraph(1, 200)];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].simhash_fingerprint.is_some(), "simhash_fingerprint should be Some");
    assert_ne!(chunks[0].simhash_fingerprint.unwrap(), 0, "simhash_fingerprint should be non-zero");
}

#[test]
fn near_duplicate_marking_works() {
    // Directly test mark_near_duplicates: two chunks with the same simhash but
    // different literal_digest should result in the later one being marked as near-duplicate.
    let shared_text = "This is a comprehensive paragraph with enough words to produce meaningful simhash shingles for near duplicate detection testing purposes in the chunking system";
    let fingerprint = compute_simhash(shared_text);

    let mut chunks = vec![
        StructuredChunkWindow {
            chunk_index: 0,
            chunk_kind: StructuredBlockKind::Paragraph,
            support_block_ids: vec![Uuid::now_v7()],
            content_text: shared_text.to_string(),
            normalized_text: shared_text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            token_count: Some(20),
            literal_digest: Some("sha256:aaa".to_string()),
            quality_score: 1.0,
            simhash_fingerprint: Some(fingerprint),
            is_near_duplicate: false,
            window_text: None,
        },
        StructuredChunkWindow {
            chunk_index: 1,
            chunk_kind: StructuredBlockKind::Paragraph,
            support_block_ids: vec![Uuid::now_v7()],
            content_text: shared_text.to_string(),
            normalized_text: shared_text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            token_count: Some(20),
            literal_digest: Some("sha256:bbb".to_string()),
            quality_score: 1.0,
            simhash_fingerprint: Some(fingerprint),
            is_near_duplicate: false,
            window_text: None,
        },
    ];

    mark_near_duplicates(&mut chunks);

    assert!(
        chunks[1].is_near_duplicate,
        "second chunk with same simhash but different digest should be marked as near_duplicate"
    );
}

#[test]
fn zero_overlap_produces_no_shared_blocks() {
    // 6 blocks of ~600 chars each to produce 2 chunks
    let blocks: Vec<StructuredBlockData> = (0..6).map(|i| make_paragraph(i, 600)).collect();

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert!(chunks.len() >= 2, "expected at least 2 chunks");

    for i in 0..chunks.len() {
        for j in (i + 1)..chunks.len() {
            let ids_i: std::collections::HashSet<Uuid> =
                chunks[i].support_block_ids.iter().copied().collect();
            let ids_j: std::collections::HashSet<Uuid> =
                chunks[j].support_block_ids.iter().copied().collect();
            let shared: Vec<_> = ids_i.intersection(&ids_j).collect();
            assert!(
                shared.is_empty(),
                "with zero overlap, no block_id should appear in multiple chunks"
            );
        }
    }
}

#[test]
fn detects_rust_function_boundaries() {
    let code = "use std::io;\n\npub fn main() {\n    println!(\"hello\");\n}\n\nfn helper() -> bool {\n    true\n}\n";
    let bounds = detect_code_boundaries(code, Some("rust"));
    assert!(bounds.contains(&2), "should detect pub fn main at line 2, got {bounds:?}");
    assert!(bounds.contains(&6), "should detect fn helper at line 6, got {bounds:?}");
}

#[test]
fn computes_sentence_context_without_reordering_existing_semantics() {
    let mut chunks = vec![
        make_chunk(0, "First alpha. First beta."),
        make_chunk(1, "Core sentence."),
        make_chunk(2, "Last alpha. Last beta."),
    ];

    compute_window_text_pass(&mut chunks);

    assert_eq!(
        chunks[1].window_text.as_deref(),
        Some("First beta. First alpha.\n\nCore sentence.\n\nLast alpha. Last beta.")
    );
}

#[test]
fn detects_language_boundary_aliases_and_lookahead_rules() {
    let rust_code = "/// Boundary docs\npub struct Item;\n";
    assert_eq!(detect_code_boundaries(rust_code, Some("rs")), vec![0, 1]);

    let python_code = "@decorator\ndef item():\n    pass\n@property\ndef value():\n    pass\n";
    assert_eq!(detect_code_boundaries(python_code, Some("py")), vec![0, 1, 4]);

    let javascript_code =
        "const top = () => true;\n    const nested = () => false;\nexport type Item = string;\n";
    assert_eq!(detect_code_boundaries(javascript_code, Some("js")), vec![0, 2]);
}

#[test]
fn detects_raw_line_sensitive_and_case_normalized_boundaries() {
    let c_code = "int run() {\n    if (ready) {\n}\nreturn value;\n";
    assert_eq!(detect_code_boundaries(c_code, Some("cpp")), vec![0]);

    let sql_code = "create table items (id int);\nselect * from items;\n";
    assert_eq!(detect_code_boundaries(sql_code, Some("sql")), vec![0]);

    let yaml = "root:\n  nested:\n- list:\nsecond:\n";
    assert_eq!(detect_code_boundaries(yaml, Some("yaml")), vec![0, 3]);
}

#[test]
fn detects_python_class_and_def_boundaries() {
    let code = "import os\n\nclass MyClass:\n    def __init__(self):\n        pass\n\n    def method(self):\n        pass\n\ndef standalone():\n    pass\n";
    let bounds = detect_code_boundaries(code, Some("python"));
    assert!(bounds.contains(&2), "should detect class MyClass at line 2, got {bounds:?}");
    assert!(bounds.contains(&9), "should detect def standalone at line 9, got {bounds:?}");
}

#[test]
fn auto_detects_language_from_content() {
    let rust_code = "pub fn main() {\n    let x = 5;\n    println!(\"{}\", x);\n}\n";
    let lang = guess_language(&rust_code.lines().collect::<Vec<_>>());
    assert_eq!(lang, "rust");
}

#[test]
fn splits_large_code_block_before_chunking() {
    let mut lines = Vec::new();
    for i in 0..10 {
        lines.push(format!("fn func_{i}() {{"));
        for j in 0..30 {
            lines.push(format!("    let x_{j} = {j};"));
        }
        lines.push("}".to_string());
        lines.push(String::new());
    }
    let big_code = lines.join("\n");
    let block = StructuredBlockData {
        block_id: Uuid::now_v7(),
        ordinal: 0,
        block_kind: StructuredBlockKind::CodeBlock,
        text: big_code.clone(),
        normalized_text: big_code,
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        source_span: None,
        parent_block_id: None,
        table_coordinates: None,
        code_language: Some("rust".to_string()),
        is_boilerplate: false,
    };
    let result = split_large_code_blocks(&[block], 500);
    assert!(
        result.len() > 1,
        "should split large code block into multiple sub-blocks, got {}",
        result.len()
    );
    assert!(
        result.iter().all(|split| split.parent_block_id.is_none()),
        "split code blocks must not reference a discarded parent block"
    );
}

#[test]
fn builds_structured_chunk_windows_from_semantic_blocks() {
    let blocks = vec![
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: StructuredBlockKind::Heading,
            text: "API".to_string(),
            normalized_text: "API".to_string(),
            heading_trail: vec!["API".to_string()],
            section_path: vec!["api".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 1,
            block_kind: StructuredBlockKind::EndpointBlock,
            text: "GET /v1/accounts".to_string(),
            normalized_text: "GET /v1/accounts".to_string(),
            heading_trail: vec!["API".to_string()],
            section_path: vec!["api".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
    ];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 80, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::EndpointBlock);
    assert_eq!(chunks[0].support_block_ids.len(), 2);
    assert_eq!(chunks[0].heading_trail, vec!["API".to_string()]);
    assert!(chunks[0].literal_digest.as_deref().is_some_and(|value| value.starts_with("sha256:")));
}

#[test]
fn table_rows_become_independent_chunks() {
    let table_id = Uuid::now_v7();
    let blocks = vec![
        StructuredBlockData {
            block_id: table_id,
            ordinal: 0,
            block_kind: StructuredBlockKind::Table,
            text: "| Name | Value |\n| --- | --- |\n| Alice | 42 |".to_string(),
            normalized_text: "| Name | Value |\n| --- | --- |\n| Alice | 42 |".to_string(),
            heading_trail: vec!["people".to_string()],
            section_path: vec!["people".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 1,
            block_kind: StructuredBlockKind::TableRow,
            text: "| Alice | 42 |".to_string(),
            normalized_text: "Sheet: people | Row 1 | Name: Alice | Value: 42".to_string(),
            heading_trail: vec!["people".to_string()],
            section_path: vec!["people".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: Some(table_id),
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 2,
            block_kind: StructuredBlockKind::TableRow,
            text: "| Bob | 7 |".to_string(),
            normalized_text: "Sheet: people | Row 2 | Name: Bob | Value: 7".to_string(),
            heading_trail: vec!["people".to_string()],
            section_path: vec!["people".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: Some(table_id),
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
    ];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 2);
    assert!(chunks.iter().all(|chunk| chunk.chunk_kind == StructuredBlockKind::TableRow));
    assert_eq!(chunks[0].content_text, "| Alice | 42 |");
    assert_eq!(chunks[1].content_text, "| Bob | 7 |");
}

#[test]
fn table_summaries_become_independent_chunks() {
    let blocks = vec![
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: StructuredBlockKind::Heading,
            text: "organizations".to_string(),
            normalized_text: "organizations".to_string(),
            heading_trail: vec!["organizations".to_string()],
            section_path: vec!["organizations".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 1,
            block_kind: StructuredBlockKind::MetadataBlock,
            text: String::new(),
            normalized_text: "Table Summary | Sheet: organizations | Column: Country | Value Kind: categorical | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 2 | Most Frequent Count: 2 | Most Frequent Tie Count: 1 | Most Frequent Values: Sweden".to_string(),
            heading_trail: vec!["organizations".to_string()],
            section_path: vec!["organizations".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: Some(Uuid::now_v7()),
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 2,
            block_kind: StructuredBlockKind::MetadataBlock,
            text: String::new(),
            normalized_text: "Table Summary | Sheet: organizations | Column: Employees | Value Kind: numeric | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 20 | Min: 10 | Max: 30".to_string(),
            heading_trail: vec!["organizations".to_string()],
            section_path: vec!["organizations".to_string()],
            page_number: None,
            source_span: None,
            parent_block_id: Some(Uuid::now_v7()),
            table_coordinates: None,
            code_language: None,
            is_boilerplate: false,
        },
    ];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 0 },
    );

    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::Heading);
    assert_eq!(chunks[1].chunk_kind, StructuredBlockKind::MetadataBlock);
    assert_eq!(chunks[2].chunk_kind, StructuredBlockKind::MetadataBlock);
    assert!(chunks[1].normalized_text.starts_with("Table Summary |"));
    assert!(chunks[2].normalized_text.starts_with("Table Summary |"));
}

#[test]
fn source_units_become_independent_chunks() {
    let blocks = vec![
        make_block(0, StructuredBlockKind::SourceProfile, "[source_profile unit_count=2]", false),
        make_block(1, StructuredBlockKind::SourceUnit, "[unit_id=one] first detail", false),
        make_block(2, StructuredBlockKind::SourceUnit, "[unit_id=two] second detail", false),
    ];

    let chunks = build_structured_chunk_windows(
        &blocks,
        StructuredChunkingProfile { max_chars: 2800, overlap_chars: 280 },
    );

    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].chunk_kind, StructuredBlockKind::SourceProfile);
    assert_eq!(chunks[1].chunk_kind, StructuredBlockKind::SourceUnit);
    assert_eq!(chunks[2].chunk_kind, StructuredBlockKind::SourceUnit);
    assert_eq!(chunks[1].content_text, "[unit_id=one] first detail");
    assert_eq!(chunks[2].content_text, "[unit_id=two] second detail");
}
