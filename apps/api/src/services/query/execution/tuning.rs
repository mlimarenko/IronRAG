//! Tuning knobs that govern user-observable grounded_answer behaviour.
//!
//! Every constant here shapes a decision the runtime makes on behalf
//! of the user — whether to clarify, how many sources to surface, how
//! long the tool loop is allowed to run. They are grouped in one
//! module so operators and future readers can scan the full set in
//! one place instead of hunting them across `answer_pipeline.rs`,
//! `agent_loop.rs`, and friends.
//!
//! Low-level implementation knobs that do not affect visible behaviour
//! (CRAG score threshold, lexical-query fan-out cap, etc.) deliberately
//! stay next to the code that uses them — centralising those here
//! would just add indirection without operator value.

/// Maximum number of candidate variants surfaced in a clarification
/// prompt. More than this and the user reads a menu instead of a
/// question; fewer and the clarify branch risks missing what the
/// library actually has. Eight keeps broad provider families visible
/// without turning the prompt into an unbounded dump.
pub(crate) const CLARIFY_MAX_VARIANTS: usize = 8;

/// Minimum number of distinct retrieved documents below which the
/// clarify branch cannot fire — with one or two documents there is
/// no meaningful "pick a variant" choice to offer.
pub(crate) const CLARIFY_MIN_DISTINCT_DOCUMENTS: usize = 3;

/// Score-dominance ratio used to declare that the top retrieved
/// document clearly dominates the rest. When
/// `top1_score / top2_score >= CLARIFY_DOMINANCE_RATIO` the evidence
/// has one clear cluster and the runtime should answer directly
/// rather than clarify.
pub(crate) const CLARIFY_DOMINANCE_RATIO: f32 = 1.35;

/// Absolute floor below which a single-shot answer is always treated
/// as "model declined" and the question is escalated. Deliberately
/// small — a genuine one-line answer ("Нажмите Ctrl+Alt+Del") is
/// useful, but a two-word shrug almost never is.
pub(crate) const SINGLE_SHOT_MIN_ANSWER_CHARS: usize = 24;

/// When retrieval surfaces several candidate documents but the
/// single-shot answer is still very short, the LLM almost certainly
/// capitulated in front of good evidence. Escalate to the tool loop
/// on the short path so the model reads a document and gives the
/// user an actual answer. The threshold is structural — no
/// decline-phrase matching — so language or provider changes do not
/// silently break the gate.
pub(crate) const SINGLE_SHOT_CONFIDENT_ANSWER_CHARS: usize = 80;

/// Minimum retrieval footprint that disarms the confident-length
/// escalation above. When retrieval came back essentially empty the
/// model has no real evidence to work with and a short "no answer"
/// reply is the correct output; escalating to the tool loop there
/// would only spend another 30–60 s of LLM time before returning
/// the same refusal. Five retrieved documents is a conservative
/// "the library has material about this" signal without overlapping
/// the `ready`-bucket / decline case.
pub(crate) const SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS: usize = 5;

/// Maximum number of `[Title](url)` entries in the auto-appended
/// Sources block at the end of a grounded answer. Keeps long answers
/// from ending in a 20-bullet reference dump.
pub(crate) const MAX_APPENDED_SOURCES: usize = 5;

/// Upper bound on the number of chunks the winning document may
/// occupy in the final retrieval bundle when `focused_document_consolidation`
/// picks a high-confidence single-document winner (hint /
/// single-document subject / only retrieved document). Larger than
/// the default `top_k` so the winner can dominate the answer context
/// without starving tangentials into a zero-slot state.
pub(crate) const FOCUSED_WINNER_MAX_CHUNKS: usize = 16;

/// Excerpt size for chunks materialized by focused-document
/// consolidation. These chunks are already from the chosen winner
/// document, so a larger excerpt is preferable to truncating config
/// examples, long URLs, or parameter rows in the middle.
pub(crate) const FOCUSED_WINNER_EXCERPT_CHARS: usize = 720;

/// Raw-score floor that marks a retrieval hit as a document-identity
/// signal rather than an ordinary lexical/vector relevance score. RRF
/// still normalizes normal lanes, but these high-scale identity hits
/// must survive merge/rerank so focused-document consolidation can pack
/// the matching document instead of handing the answer model one intro
/// chunk plus unrelated tail evidence.
pub(crate) const DOCUMENT_IDENTITY_SCORE_FLOOR: f32 = 100_000.0;

/// Minimum best-score ratio required before the consolidation stage
/// treats one retrieved document as uniquely identified by score. This
/// is deliberately orders-of-magnitude higher than normal rerank
/// spreads; ordinary relevance differences must not collapse a
/// multi-document answer into a single-document focus.
pub(crate) const DOCUMENT_IDENTITY_DOMINANCE_RATIO: f32 = 1_000.0;
