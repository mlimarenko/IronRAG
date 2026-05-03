#[cfg(test)]
use uuid::Uuid;

mod answer;
mod answer_pipeline;
mod canonical_answer_context;
mod consolidation;
mod context;
mod document_target;
mod embed;
mod endpoint_answer;
#[cfg(test)]
mod endpoint_chunk_answer;
mod fact_lookup;
mod focused_document_answer;
mod graph_retrieval;
mod hyde_crag;
mod port_answer;
mod preflight;
pub(crate) mod question_intent;
mod rerank;
mod retrieve;
mod source_context;
mod source_profile;
mod structured_query_pipeline;
mod table_retrieval;
mod table_row_answer;
mod table_summary_answer;
mod technical_answer;
mod technical_literal_context;
mod technical_literal_extractors;
mod technical_literal_focus;
mod technical_literals;
mod technical_parameter_answer;
mod technical_url_answer;
#[cfg(test)]
mod tests;
mod transport_answer;
mod tuning;
mod types;
mod verification;

#[cfg(test)]
use crate::domains::query::QueryVerificationState;
#[cfg(test)]
use crate::domains::query::QueryVerificationWarning;
use embed::embed_question;
use hyde_crag::generate_hyde_passage;
#[cfg(test)]
use port_answer::{build_port_and_protocol_answer, build_port_answer};
#[cfg(test)]
use preflight::{
    build_canonical_preflight_answer, build_preflight_answer_chunks,
    build_preflight_canonical_evidence, build_preflight_graph_evidence_context_lines,
    preflight_exact_literal_document_scope,
};
use preflight::{prepare_canonical_answer_preflight, select_technical_literal_chunks};
#[cfg(test)]
use technical_literal_context::build_exact_technical_literals_section;
use technical_literal_context::{
    collect_technical_literal_groups, render_exact_technical_literals_section,
};
#[cfg(test)]
use technical_literals::detect_technical_literal_intent;
#[cfg(test)]
use technical_literals::technical_literal_focus_keyword_segments;
use technical_literals::{
    TechnicalLiteralIntent, question_mentions_pagination, technical_literal_candidate_limit,
    technical_literal_focus_keywords,
};
#[cfg(test)]
use types::RuntimeAnswerVerification;
#[cfg(test)]
use verification::{enrich_query_assembly_diagnostics, enrich_query_candidate_summary};

#[cfg(test)]
use crate::domains::query::RuntimeQueryMode;
pub(crate) use answer::*;
pub(crate) use answer_pipeline::*;
pub(crate) use canonical_answer_context::*;
pub(crate) use consolidation::*;
pub(crate) use context::*;
pub(crate) use document_target::*;
#[cfg(test)]
pub(crate) use endpoint_chunk_answer::*;
pub(crate) use graph_retrieval::*;
pub(crate) use rerank::*;
pub(crate) use retrieve::*;
pub(crate) use source_context::*;
pub(crate) use structured_query_pipeline::*;
pub(crate) use table_retrieval::*;
pub(crate) use table_row_answer::*;
pub(crate) use table_summary_answer::*;
pub(crate) use types::*;
pub(crate) use verification::*;

/// HyDE passage generation timeout. Increase for slow LLM providers.
const HYDE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// HyDE generation temperature. Lower = more factual, higher = more creative.
const HYDE_TEMPERATURE: f64 = 0.3;
/// Maximum structured blocks included per answer assembly pass.
const MAX_ANSWER_BLOCKS: usize = 16;
/// Maximum chunks selected per document in balanced chunk selection.
const MAX_CHUNKS_PER_DOCUMENT: usize = 8;
/// Minimum chunks selected per document in balanced chunk selection.
const MIN_CHUNKS_PER_DOCUMENT: usize = 2;
