use std::collections::HashSet;

use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAnswerUnit, SemanticByteSpan,
    SemanticEvidenceScope, SemanticFailureCode, SemanticNotRunReason, SemanticSourceEntry,
    SemanticSourceLocator, SemanticVerificationOutcome, SemanticVerificationReport,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    MAX_ANSWER_UNITS, MAX_ANSWER_UTF8_BYTES, MAX_EVIDENCE_UTF8_BYTES, MAX_QUESTION_UTF8_BYTES,
    MAX_SOURCE_ENTRIES, PreparedSemanticVerification,
};

/// Authoritative complete-text identity from the upstream evidence owner.
/// A proof must refer to the canonical entire revision, not a selected excerpt.
#[derive(Debug, Clone)]
pub(crate) struct CompleteDocumentProof {
    pub(crate) canonical_text_sha256: String,
    pub(crate) canonical_utf8_bytes: usize,
}

/// The caller has already authorized this exact revision and extracted canonical
/// source text. This value grants no permission and strips no metadata wrappers:
/// callers must not pass metadata, tool envelopes or secrets as `text`.
#[derive(Debug, Clone)]
pub(crate) struct AuthorizedSemanticSource {
    pub(crate) document_id: Uuid,
    pub(crate) document_revision_id: Uuid,
    pub(crate) locator: SemanticSourceLocator,
    pub(crate) text: String,
    pub(crate) complete_document: Option<CompleteDocumentProof>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SemanticPreparationError {
    #[error("no nonempty answer is available")]
    EmptyAnswer,
    #[error("no authorized source text is available")]
    NoEvidence,
    #[error("answer exceeds the UTF-8 byte limit")]
    AnswerTooLarge,
    #[error("question exceeds the UTF-8 byte limit")]
    QuestionTooLarge,
    #[error("answer exceeds the exhaustive unit limit")]
    TooManyUnits,
    #[error("source entry count exceeds its limit")]
    TooManySources,
    #[error("source text exceeds the aggregate UTF-8 byte limit")]
    EvidenceTooLarge,
    #[error("source text, identity or locator is invalid")]
    InvalidSource,
    #[error("complete document proof does not match the canonical text and locator")]
    InvalidCompleteDocumentProof,
    #[error("duplicate source entry")]
    DuplicateSource,
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn framed_hash(fields: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for field in fields {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    hex::encode(hash.finalize())
}

// The byte encoding is explicit and independent of JSON object ordering.
pub(super) fn source_identity_hash(
    source: &AuthorizedSemanticSource,
    scope: SemanticEvidenceScope,
) -> String {
    let (kind, locator) = match &source.locator {
        SemanticSourceLocator::Chunk { chunk_id } => {
            (b"chunk".as_slice(), chunk_id.as_bytes().to_vec())
        }
        SemanticSourceLocator::Segment { segment_id } => {
            (b"segment".as_slice(), segment_id.as_bytes().to_vec())
        }
        SemanticSourceLocator::DocumentSlice { start, end } => {
            let mut bytes = (*start as u64).to_be_bytes().to_vec();
            bytes.extend_from_slice(&(*end as u64).to_be_bytes());
            (b"document_slice".as_slice(), bytes)
        }
    };
    framed_hash(&[
        b"ironrag-semantic-source",
        &SEMANTIC_VERIFICATION_PROTOCOL_VERSION.to_be_bytes(),
        source.document_id.as_bytes(),
        source.document_revision_id.as_bytes(),
        kind,
        &locator,
        match scope {
            SemanticEvidenceScope::SelectedExcerpt => b"selected_excerpt",
            SemanticEvidenceScope::CompleteDocument => b"complete_document",
        },
        source.text.as_bytes(),
    ])
}

/// Prepare every nonempty line/block of the exact answer. Blank separators are
/// retained in adjacent spans, so units concatenate to the original bytes with
/// no uncovered assertion or normalization. Oversized input is refused, not cut.
pub(crate) fn prepare(
    question: &str,
    answer: &str,
    authorized_sources: Vec<AuthorizedSemanticSource>,
) -> Result<PreparedSemanticVerification, SemanticPreparationError> {
    if answer.len() > MAX_ANSWER_UTF8_BYTES {
        return Err(SemanticPreparationError::AnswerTooLarge);
    }
    if question.len() > MAX_QUESTION_UTF8_BYTES {
        return Err(SemanticPreparationError::QuestionTooLarge);
    }
    if answer.trim().is_empty() {
        return Err(SemanticPreparationError::EmptyAnswer);
    }
    if authorized_sources.is_empty() {
        return Err(SemanticPreparationError::NoEvidence);
    }
    if authorized_sources.len() > MAX_SOURCE_ENTRIES {
        return Err(SemanticPreparationError::TooManySources);
    }
    let answer_sha256 = sha256(answer.as_bytes());
    let units = prepare_units(answer, &answer_sha256)?;
    let sources = prepare_sources(authorized_sources)?;
    if sources.is_empty() {
        return Err(SemanticPreparationError::NoEvidence);
    }
    let manifest_sha256 = manifest_sha256(&sources);
    Ok(PreparedSemanticVerification {
        question: question.to_owned(),
        question_sha256: sha256(question.as_bytes()),
        answer_sha256,
        manifest_sha256,
        units,
        sources,
    })
}

pub(super) fn manifest_sha256(sources: &[SemanticSourceEntry]) -> String {
    let mut fields = vec![b"ironrag-semantic-manifest".as_slice()];
    for source in sources {
        fields.push(source.source_id.as_bytes());
    }
    framed_hash(&fields)
}

/// Record a refusal to prepare, not a judgment of omitted or invalid evidence.
/// Only hashes of the raw question/answer are retained. The empty canonical
/// manifest and empty units explicitly state that no semantic request existed.
pub(crate) fn preparation_failure_report(
    question: &str,
    answer: &str,
    error: SemanticPreparationError,
    elapsed_ms: u64,
) -> SemanticVerificationReport {
    let outcome = match error {
        SemanticPreparationError::EmptyAnswer => {
            SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::EmptyAnswer }
        }
        SemanticPreparationError::NoEvidence => {
            SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::NoEvidence }
        }
        SemanticPreparationError::AnswerTooLarge
        | SemanticPreparationError::QuestionTooLarge
        | SemanticPreparationError::TooManyUnits
        | SemanticPreparationError::TooManySources
        | SemanticPreparationError::EvidenceTooLarge => {
            SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::InputLimitExceeded }
        }
        SemanticPreparationError::InvalidSource
        | SemanticPreparationError::InvalidCompleteDocumentProof
        | SemanticPreparationError::DuplicateSource => {
            SemanticVerificationOutcome::Failed { code: SemanticFailureCode::InvalidProvenance }
        }
    };
    SemanticVerificationReport {
        protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
        question_sha256: sha256(question.as_bytes()),
        checked_answer_sha256: sha256(answer.as_bytes()),
        visible_answer_sha256: None,
        evidence_manifest_sha256: manifest_sha256(&[]),
        answer_units: Vec::new(),
        sources: Vec::new(),
        elapsed_ms,
        provider_call_id: None,
        outcome,
    }
}

fn sentence_spans(answer: &str) -> Vec<SemanticByteSpan> {
    let mut spans: Vec<SemanticByteSpan> = Vec::new();
    let mut start = 0usize;
    let mut cursor = 0usize;
    while cursor < answer.len() {
        let ch = answer[cursor..].chars().next().expect("cursor is a char boundary");
        let boundary = matches!(ch, '.' | '!' | '?') || ch == '\n';
        if !boundary {
            cursor += ch.len_utf8();
            continue;
        }
        let mut end = cursor + ch.len_utf8();
        while end < answer.len() {
            let next = answer[end..].chars().next().expect("end is a char boundary");
            if next.is_whitespace() {
                end += next.len_utf8();
            } else {
                break;
            }
        }
        // A leading whitespace-only run belongs to the first real sentence,
        // not to a unit of its own.
        if !answer[start..end].trim().is_empty() {
            spans.push(SemanticByteSpan { start, end });
            start = end;
        }
        cursor = end;
    }
    if start < answer.len() {
        if let Some(last) = spans.last_mut() {
            if answer[start..].trim().is_empty() {
                last.end = answer.len();
            } else {
                spans.push(SemanticByteSpan { start, end: answer.len() });
            }
        } else {
            spans.push(SemanticByteSpan { start, end: answer.len() });
        }
    }
    spans
}

/// Previous paragraph/line grouping, used when a long answer would exceed the
/// unit cap. It is contiguous and exhaustive like the sentence pass.
fn line_spans(answer: &str) -> Vec<SemanticByteSpan> {
    let mut spans: Vec<SemanticByteSpan> = Vec::new();
    let mut offset = 0usize;
    let mut start = 0usize;
    let mut pending_nonempty = false;
    for line in answer.split_inclusive('\n') {
        if !line.trim().is_empty() {
            if pending_nonempty {
                spans.push(SemanticByteSpan { start, end: offset });
                start = offset;
            }
            pending_nonempty = true;
        }
        offset += line.len();
    }
    spans.push(SemanticByteSpan { start, end: answer.len() });
    spans
}

pub(super) fn prepare_units(
    answer: &str,
    answer_sha256: &str,
) -> Result<Vec<SemanticAnswerUnit>, SemanticPreparationError> {
    let mut spans = sentence_spans(answer);
    if spans.len() > MAX_ANSWER_UNITS {
        spans = line_spans(answer);
    }
    if spans.len() > MAX_ANSWER_UNITS {
        return Err(SemanticPreparationError::TooManyUnits);
    }
    Ok(spans
        .into_iter()
        .map(|span| SemanticAnswerUnit {
            unit_id: format!(
                "u_{}",
                framed_hash(&[
                    b"ironrag-semantic-unit",
                    answer_sha256.as_bytes(),
                    &(span.start as u64).to_be_bytes(),
                    &(span.end as u64).to_be_bytes()
                ])
            ),
            text: answer[span.start..span.end].to_owned(),
            span,
        })
        .collect())
}

fn prepare_sources(
    authorized_sources: Vec<AuthorizedSemanticSource>,
) -> Result<Vec<SemanticSourceEntry>, SemanticPreparationError> {
    let mut total_bytes = 0usize;
    let mut sources = Vec::with_capacity(authorized_sources.len());
    let mut seen = HashSet::new();
    for source in authorized_sources {
        // An empty selected source carries no evidence. Dropping it is not a
        // provenance failure; a set with no usable source becomes NoEvidence.
        if source.text.trim().is_empty() {
            continue;
        }
        total_bytes = total_bytes
            .checked_add(source.text.len())
            .ok_or(SemanticPreparationError::EvidenceTooLarge)?;
        if total_bytes > MAX_EVIDENCE_UTF8_BYTES {
            return Err(SemanticPreparationError::EvidenceTooLarge);
        }
        let entry = prepare_source(source)?;
        if !seen.insert(entry.source_id.clone()) {
            return Err(SemanticPreparationError::DuplicateSource);
        }
        sources.push(entry);
    }
    sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(sources)
}

fn prepare_source(
    source: AuthorizedSemanticSource,
) -> Result<SemanticSourceEntry, SemanticPreparationError> {
    if source.text.trim().is_empty()
        || source.document_id.is_nil()
        || source.document_revision_id.is_nil()
    {
        return Err(SemanticPreparationError::InvalidSource);
    }
    match &source.locator {
        SemanticSourceLocator::Chunk { chunk_id } if chunk_id.is_nil() => {
            return Err(SemanticPreparationError::InvalidSource);
        }
        SemanticSourceLocator::Segment { segment_id } if segment_id.is_nil() => {
            return Err(SemanticPreparationError::InvalidSource);
        }
        SemanticSourceLocator::DocumentSlice { start, end }
            if end.checked_sub(*start) != Some(source.text.len()) =>
        {
            return Err(SemanticPreparationError::InvalidSource);
        }
        _ => {}
    }
    let text_sha256 = sha256(source.text.as_bytes());
    let scope = if let Some(proof) = &source.complete_document {
        if proof.canonical_text_sha256 != text_sha256
            || proof.canonical_utf8_bytes != source.text.len()
            || source.locator
                != (SemanticSourceLocator::DocumentSlice { start: 0, end: source.text.len() })
        {
            return Err(SemanticPreparationError::InvalidCompleteDocumentProof);
        }
        SemanticEvidenceScope::CompleteDocument
    } else {
        SemanticEvidenceScope::SelectedExcerpt
    };
    let source_id = format!("s_{}", source_identity_hash(&source, scope));
    Ok(SemanticSourceEntry {
        source_id,
        document_id: source.document_id,
        document_revision_id: source.document_revision_id,
        locator: source.locator,
        scope,
        text_sha256,
        text: source.text,
    })
}
