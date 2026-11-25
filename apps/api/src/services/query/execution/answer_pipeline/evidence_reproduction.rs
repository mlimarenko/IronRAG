//! Detect an answer body that is the retrieved evidence itself.
//!
//! The answer stage must synthesize from bounded evidence. Some inputs make a
//! model hand the evidence back instead: a document whose body is a long
//! comment/activity feed reads like a ready-made summary, and the draft returns
//! one contiguous run of it. That body is not an answer — it is a dump — and a
//! failed or bounded verification must never publish it as one.
//!
//! The measure is token-based so wrapping, markdown decoration and case do not
//! hide a copy, and it ignores the short exact literals a synthesized answer
//! legitimately quotes. The verdict is typed ([`EvidenceReproduction`]) so the
//! answer pipeline can reject such a candidate and the visibility policy can
//! replace an unverified body instead of showing it.

use std::collections::HashSet;

use crate::domains::query::{QueryVerificationState, QueryVerificationWarning};

/// Verifier warning code for a body that reproduces its own evidence.
pub(crate) const ANSWER_REPRODUCED_EVIDENCE: &str = "answer_reproduced_evidence";

/// Token window that counts as one verbatim copy unit. Eight tokens is already
/// a sentence fragment; a shared identifier or short quote cannot reach it.
const WINDOW_TOKENS: usize = 8;

/// Below this size a body may legitimately quote a passage without being an
/// answer made of evidence.
const MIN_ANSWER_TOKENS: usize = 60;

/// One copied run must span a real sentence, not a shared label.
const MIN_COPIED_RUN_TOKENS: usize = 24;

/// Share of the answer that must be copied before the body counts as evidence
/// rather than as an answer, in whole percent.
const COPIED_COVERAGE_PERCENT: usize = 60;

/// How much of one candidate body came back verbatim from the evidence it was
/// generated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EvidenceReproduction {
    total_tokens: usize,
    copied_tokens: usize,
    longest_run_tokens: usize,
}

impl EvidenceReproduction {
    /// Whether the body is the evidence instead of an answer: long enough to be
    /// an answer, one sentence-long copied run, and most of it copied.
    #[must_use]
    pub(crate) fn is_wholesale(self) -> bool {
        self.total_tokens >= MIN_ANSWER_TOKENS
            && self.longest_run_tokens >= MIN_COPIED_RUN_TOKENS
            && self.copied_tokens * 100 >= self.total_tokens * COPIED_COVERAGE_PERCENT
    }

    #[must_use]
    fn copied_percent(self) -> usize {
        if self.total_tokens == 0 {
            return 0;
        }
        self.copied_tokens * 100 / self.total_tokens
    }
}

/// Measure copying without creating token windows across source boundaries.
#[must_use]
pub(crate) fn measure_evidence_reproduction<'a>(
    answer: &str,
    evidence: impl IntoIterator<Item = &'a str>,
) -> EvidenceReproduction {
    let answer_tokens = tokens(answer);
    let total_tokens = answer_tokens.len();
    if total_tokens < MIN_ANSWER_TOKENS {
        return EvidenceReproduction { total_tokens, copied_tokens: 0, longest_run_tokens: 0 };
    }
    let mut windows = HashSet::new();
    for source in evidence {
        windows.extend(tokens(source).windows(WINDOW_TOKENS).map(|window| window.join("\u{1f}")));
    }
    let mut covered = vec![false; total_tokens];
    for (index, window) in answer_tokens.windows(WINDOW_TOKENS).enumerate() {
        if windows.contains(&window.join("\u{1f}")) {
            covered[index..index + WINDOW_TOKENS].fill(true);
        }
    }
    EvidenceReproduction {
        total_tokens,
        copied_tokens: covered.iter().filter(|covered| **covered).count(),
        longest_run_tokens: longest_run(&covered),
    }
}

/// Whether `answer` is the evidence rather than an answer to the question.
#[must_use]
pub(crate) fn answer_reproduces_evidence(answer: &str, evidence: &str) -> bool {
    measure_evidence_reproduction(answer, [evidence]).is_wholesale()
}

/// The typed warning the answer stage records when a candidate that did not
/// earn `Verified` is its own evidence. A verified body keeps its text: quoting
/// a requested section verbatim is a legitimate answer when the verifier
/// established it as one.
#[must_use]
pub(crate) fn evidence_reproduction_warning<'a>(
    answer: &str,
    evidence: impl IntoIterator<Item = &'a str>,
    state: QueryVerificationState,
) -> Option<QueryVerificationWarning> {
    if matches!(state, QueryVerificationState::Verified) {
        return None;
    }
    let reproduction = measure_evidence_reproduction(answer, evidence);
    reproduction.is_wholesale().then(|| QueryVerificationWarning {
        code: ANSWER_REPRODUCED_EVIDENCE.to_string(),
        message: format!(
            "The answer reproduces its retrieved evidence: {}% of {} answer tokens are one verbatim copy from the provided context, in a run of {} tokens. Show a synthesized answer or a bounded refusal instead.",
            reproduction.copied_percent(),
            reproduction.total_tokens,
            reproduction.longest_run_tokens
        ),
        related_segment_id: None,
        related_fact_id: None,
    })
}

fn tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn longest_run(covered: &[bool]) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for covered in covered {
        if *covered {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}
