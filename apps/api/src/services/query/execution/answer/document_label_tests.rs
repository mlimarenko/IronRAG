use std::collections::HashMap;

use uuid::Uuid;

use crate::domains::query_ir::{
    EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope, SourceSliceDirection,
    SourceSliceFilter, SourceSliceSpec,
};
use crate::infra::knowledge_rows::KnowledgeDocumentRow;
use crate::services::query::effective_query::{
    EFFECTIVE_QUERY_QUESTION_PREFIX, EFFECTIVE_QUERY_SCOPE_PREFIX,
};
use crate::services::query::execution::types::RuntimeMatchedChunk;
use crate::shared::extraction::table_summary::{
    build_table_column_summaries, render_table_column_summary,
};

use super::super::{
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    concise_document_subject_label, document_focus_marker_hits, focused_answer_document_id,
    parse_table_row_chunk, question_asks_table_aggregation, render_table_summary_chunk_section,
};

fn effective_query_text(scope: &str, question: &str) -> String {
    format!("{EFFECTIVE_QUERY_SCOPE_PREFIX} {scope}\n{EFFECTIVE_QUERY_QUESTION_PREFIX} {question}")
}

fn describe_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.0,
    }
}

fn table_row_inventory_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableRow],
        ..describe_query_ir()
    }
}

fn initial_table_rows_ir(row_count: u16) -> QueryIR {
    QueryIR {
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableRow],
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Head,
            count: Some(row_count),
            filter: SourceSliceFilter::None,
        }),
        ..describe_query_ir()
    }
}

fn table_average_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableAverage],
        ..describe_query_ir()
    }
}

fn table_frequency_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableFrequency],
        ..describe_query_ir()
    }
}

fn table_column_inventory_ir(label: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::TableRow,
            crate::domains::query_ir::QueryTargetKind::TableSummary,
        ],
        target_entities: vec![EntityMention {
            label: label.to_string(),
            role: EntityRole::Subject,
        }],
        ..describe_query_ir()
    }
}

fn retrieve_table_column_inventory_ir(label: &str) -> QueryIR {
    QueryIR { act: QueryAct::RetrieveValue, ..table_column_inventory_ir(label) }
}

mod document_labels;
mod missing_document;
mod table_columns;
mod table_query_intent;
mod table_rows;
mod table_summaries;
