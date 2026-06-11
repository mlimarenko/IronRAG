//! Tuning knobs that govern user-observable grounded_answer behaviour.
//!
//! Every constant here shapes a decision the runtime makes on behalf
//! of the user — whether to clarify, how many sources to surface, how
//! strict a fixed-evidence answer must be. They are grouped in one
//! module so operators and future readers can scan the full set in
//! one place instead of hunting them across `answer_pipeline.rs`,
//! `agent_loop.rs`, and friends.
//!
//! Low-level implementation knobs that do not affect visible behaviour
//! (HyDE passage toggle, lexical-query fan-out cap, etc.) deliberately
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
/// small: a genuine one-line answer is useful, but a two-word shrug
/// almost never is.
pub(crate) const SINGLE_SHOT_MIN_ANSWER_CHARS: usize = 24;

/// When retrieval surfaces several candidate documents but the
/// single-shot answer is still very short, the LLM almost certainly
/// capitulated in front of good evidence. Retry through canonical
/// preflight over the same retrieved evidence so the model gives the
/// user an actual answer. The threshold is structural — no
/// decline-phrase matching — so language or provider changes do not
/// silently break the gate.
pub(crate) const SINGLE_SHOT_CONFIDENT_ANSWER_CHARS: usize = 80;

/// Minimum retrieval footprint that disarms the confident-length
/// escalation above. When retrieval came back essentially empty the
/// model has no real evidence to work with and a short "no answer"
/// reply is the correct output; retrying through preflight there
/// would only spend extra LLM time before returning the same refusal.
/// Five retrieved documents is a conservative
/// "the library has material about this" signal without overlapping
/// the `ready`-bucket / decline case.
pub(crate) const SINGLE_SHOT_RETRIEVAL_ESCALATION_MIN_DOCUMENTS: usize = 5;

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

/// Coverage floor for the P3 focus-broaden fallback. When a
/// *compiler-inferred* single-document focus pin (the IR named a module /
/// topic, not a document the user referenced verbatim) narrows retrieval to
/// at most this many chunks, the pin is dropped for a single broad
/// re-retrieval so a thin same-titled stub cannot hard-lock the answer and
/// exclude the real procedure docs. Deliberately small: it must fire only on
/// genuinely thin pins (the observed stub carried two chunks) and never on a
/// small-but-sufficient focused document, so the fallback costs at most one
/// extra retrieval and stays inside the 30s tool-call SLO.
pub(crate) const FOCUS_BROADEN_MIN_CHUNKS: usize = 2;

/// Minimum number of distinct source documents an entity must appear in
/// (via `runtime_graph_evidence`) before it qualifies as a cross-document
/// subject in the release-clarify probe. A single-doc entity is too
/// narrow to be a product variant worth offering as a clarification choice.
pub(crate) const RELEASE_CLARIFY_ENTITY_MIN_DOC_SPAN: usize = 2;

/// Minimum number of qualifying entities required to fire the
/// release-lane clarify-with-fallback. Fewer than this and the evidence
/// does not span multiple distinct subjects, so a flat list is more
/// useful than a clarifying question.
pub(crate) const RELEASE_CLARIFY_MIN_ENTITIES: usize = 2;

/// Sense-dominance ratio for the exact-label-term (short-identifier) graph
/// matching path. When a short identifier-shaped mention (e.g. an all-uppercase
/// token) matches multiple graph nodes by exact label or alias, the node set is
/// pruned to those whose `support_count >= max_matched_support / RATIO` before
/// the nodes seed neighborhood expansion. This removes incidental homographs
/// (low-evidence fringe senses) while keeping the dominant sense family.
///
/// The ratio is calibrated so that on a set whose strongest sense has support S,
/// any node with support >= S/150 survives. A sense family spanning up to ~150×
/// the weakest member is preserved; isolated noise nodes far below the dominant
/// sense are dropped. The value 150 is chosen to keep all members of a plausible
/// multi-sense dominant family (the weakest legitimate sense may be ~150× below
/// the strongest) while reliably excluding singletons with support ≤ a few
/// documents.
pub(crate) const EXACT_LABEL_TERM_SENSE_DOMINANCE_RATIO: f32 = 150.0;
