//! Canonical intermediate representation produced by `QueryCompiler`.
//!
//! Every downstream stage in the query pipeline (planner, verification,
//! session, ranking, answer generation) consumes **this** struct instead of
//! re-classifying the raw question with hardcoded keyword lists.
//!
//! Design rules, in priority order:
//!
//! 1. **Core axes are finite Rust enums.** `act`, `scope`, `language`,
//!    `entity_role`, `literal_kind`, `ref_kind` — the compiler is forced to
//!    pick exactly one, the type system refuses anything else. These are the
//!    axes that actually change pipeline routing, so they must be typed.
//!
//! 2. **Open-ended classifications are plain strings backed by an Arango
//!    ontology.** `target_types`, `comparison.dimension`, `document_focus.hint`
//!    are free-form tags. Adding a new kind of question (say "kafka topic"
//!    instead of "endpoint") is an ontology record, not a code change. The
//!    compiler is few-shot-primed from the ontology at prompt time.
//!
//! 3. **Unresolved references are first-class.** `conversation_refs` captures
//!    anaphora/deixis/ellipsis that the compiler *could not* resolve on its
//!    own. The session-level resolver (a separate stage) then fills them
//!    against conversation state. Follow-up detection is `!refs.is_empty()`
//!    or `act == FollowUp`, never a keyword check.
//!
//! 4. **Confidence is explicit, not implicit.** The `confidence` field plus
//!    `needs_clarification` let the pipeline downgrade strictness or ask the
//!    user, instead of the current binary "suppress to stub" reaction.
//!
//! The JSON schema produced by [`query_ir_json_schema`] is fed to the LLM
//! through provider structured outputs (OpenAI `json_schema` strict mode,
//! or `json_object` + prompt-engineering fallback for providers that don't
//! support strict mode — see `docs/query_compiler_provider_audit.md`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// =============================================================================
// Core axes — finite enums the downstream pipeline dispatches on.
// =============================================================================

/// What the user is fundamentally asking the system to do.
///
/// Matches the seven acts that the golden set's labelling guide enumerates.
/// The verification guard strictness, the answer builder choice, and the
/// source-link rendering all key off this.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QueryAct {
    /// Literal value expected in the answer ("what is the URL", "what port does X listen on").
    RetrieveValue,
    /// Conceptual / narrative answer ("explain X", "describe Y").
    Describe,
    /// Procedural answer ("how do I configure Z", "how to enable Y").
    ConfigureHow,
    /// Side-by-side contrast of two named subjects.
    Compare,
    /// Listing all values matching a constraint.
    Enumerate,
    /// Meta-questions about the library itself ("what documents are here",
    /// "is there a GraphQL API in this corpus").
    Meta,
    /// User refers back to prior turn without restating the topic.
    FollowUp,
}

impl QueryAct {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrieveValue => "retrieve_value",
            Self::Describe => "describe",
            Self::ConfigureHow => "configure_how",
            Self::Compare => "compare",
            Self::Enumerate => "enumerate",
            Self::Meta => "meta",
            Self::FollowUp => "follow_up",
        }
    }
}

/// Which slice of the knowledge base the answer spans.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QueryScope {
    /// Answer is expected to come from one document (default).
    SingleDocument,
    /// User mentioned two or more documents / modules / subjects to compare
    /// or aggregate across.
    MultiDocument,
    /// User explicitly referenced a different library.
    CrossLibrary,
    /// Question is about the library itself, not its contents
    /// ("what docs are in this library", "how many PDFs").
    LibraryMeta,
}

impl QueryScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleDocument => "single_document",
            Self::MultiDocument => "multi_document",
            Self::CrossLibrary => "cross_library",
            Self::LibraryMeta => "library_meta",
        }
    }
}

/// Primary language the user wrote the question in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QueryLanguage {
    En,
    Ru,
    /// Other / mixed / indeterminate. Answer language falls back to the
    /// library's configured default (or first-detected script).
    Auto,
}

impl QueryLanguage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ru => "ru",
            Self::Auto => "auto",
        }
    }
}

/// Role a named entity plays in the question.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntityRole {
    /// Primary thing the question is about ("payment module" in "how to configure payment module").
    Subject,
    /// Secondary named thing, usually in comparisons or "for X" clauses.
    Object,
    /// Qualifier on the subject (an adjective like "new" in "fields of the new customer table").
    Modifier,
}

/// Shape of a literal span so downstream can validate / match it correctly.
///
/// Kept deliberately coarse — exact regex validation is the verifier's job;
/// the compiler just labels the surface shape it observed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LiteralKind {
    /// Looks like http(s)://..., including API-style paths after a method.
    Url,
    /// Filesystem or URL path (`/etc/app/config.ini`, `/api/v2/orders`).
    Path,
    /// Identifier in camelCase / snake_case / SCREAMING_CASE
    /// (`fetchUserDetails`, `DATABASE_URL`, `with_cards`).
    Identifier,
    /// Semver / release version (`4.5.1`, `1.2`).
    Version,
    /// Numeric-looking code (`71`, `500`, port number `8080`).
    NumericCode,
    /// Any other verbatim literal the user quoted (backticked string,
    /// inline SQL / AQL snippet, config line).
    Other,
}

/// Kind of conversational reference the compiler could not resolve.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRefKind {
    /// Generic pronoun referring to prior turn ("it", "this").
    Pronoun,
    /// Deictic reference ("here", "that one").
    Deictic,
    /// Missing noun phrase — elliptic continuation ("and for the other one?").
    Elliptic,
    /// Single interrogative word that cannot stand on its own ("What?", "How?", "Where?").
    BareInterrogative,
}

/// Direction of an ordered slice requested from a sequential source.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceSliceDirection {
    /// Earliest units in the source order.
    Head,
    /// Latest units in the source order.
    Tail,
    /// Bounded representation of the whole ordered source.
    All,
}

/// Why the compiler is unsure and would prefer clarification from the user.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClarificationReason {
    /// Question is too short / ambiguous to pin an act.
    AmbiguousTooShort,
    /// Two or more incompatible interpretations are plausible.
    MultipleInterpretations,
    /// References prior turn but session state is empty or the anaphora
    /// cannot be resolved against it.
    AnaphoraUnresolved,
    /// User asked about a concept the library's ontology does not cover.
    UnknownTargetType,
}

// =============================================================================
// Composite types
// =============================================================================

/// Named thing the user talks about, with role in the question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EntityMention {
    /// Surface label as written by the user, after light normalisation
    /// (trimmed, collapsed whitespace, case preserved).
    pub label: String,
    pub role: EntityRole,
}

/// Literal the user wrote verbatim and expects the system to respect.
///
/// Custom `Deserialize` accepts either a fully-qualified object
/// (`{"text":"/api", "kind":"path"}`) or a bare string (`"/api"`) that gets
/// auto-classified by [`LiteralKind::infer`]. Both the golden set
/// (hand-labelled strings) and future LLM outputs (strict schema objects)
/// round-trip through the same type.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LiteralSpan {
    /// Exact substring from the question.
    pub text: String,
    pub kind: LiteralKind,
}

impl<'de> Deserialize<'de> for LiteralSpan {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Full { text: String, kind: LiteralKind },
            Bare(String),
        }

        match Shape::deserialize(deserializer)? {
            Shape::Full { text, kind } => Ok(Self { text, kind }),
            Shape::Bare(text) => {
                let kind = LiteralKind::infer(&text);
                Ok(Self { text, kind })
            }
        }
    }
}

impl LiteralKind {
    /// Best-effort shape classifier used when the literal arrives as a bare
    /// string (e.g. from the hand-labelled golden set). The LLM path is
    /// expected to emit the full object form through strict JSON schema.
    #[must_use]
    pub fn infer(text: &str) -> Self {
        let trimmed = text.trim();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            Self::Url
        } else if trimmed.starts_with('/') {
            Self::Path
        } else if !trimmed.is_empty() && trimmed.chars().all(|ch| ch.is_ascii_digit()) {
            Self::NumericCode
        } else if !trimmed.is_empty()
            && trimmed.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
            && trimmed.contains('.')
        {
            Self::Version
        } else if !trimmed.is_empty()
            && trimmed
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
        {
            Self::Identifier
        } else {
            Self::Other
        }
    }
}

/// When `QueryAct::Compare`, the two sides and the dimension compared.
///
/// `a` and `b` are optional because the user may ask a comparison without
/// naming both sides explicitly ("compare both services", "compare these two").
/// The resolver picks the implicit sides from session state or document
/// focus when possible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComparisonSpec {
    #[serde(default)]
    pub a: Option<String>,
    #[serde(default)]
    pub b: Option<String>,
    /// Free-form ontology tag ("transport_protocol", "performance",
    /// "feature_coverage"). Not enforced by the type system — grown via
    /// ontology entries.
    pub dimension: String,
}

/// Hint that pins the question to a specific document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DocumentHint {
    /// Surface string the user used to identify the document
    /// (title fragment, filename, section name).
    pub hint: String,
}

/// Unresolved reference the session resolver will fill from prior turns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedRef {
    /// The exact surface form used ("here", "this", "the same", "that one").
    pub surface: String,
    pub kind: ConversationRefKind,
}

/// Ordered slice request over a sequential source. The compiler sets this
/// only when the user explicitly asks for a positional range of records/items
/// in an ordered source. Ordinary summaries and needle lookups leave it null.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceSliceSpec {
    pub direction: SourceSliceDirection,
    #[serde(default)]
    pub count: Option<u16>,
}

/// Date/time or date-range constraint normalized by the query compiler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TemporalConstraint {
    /// Exact surface span from the user-visible question or history.
    pub surface: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
}

/// Clarification request the compiler would like to bubble up.
///
/// Custom `Deserialize` accepts either the full object form
/// (`{"reason":"...", "suggestion":"..."}`) or a bare reason string
/// (`"anaphora_unresolved"`). Golden-set labellers use the bare form for
/// brevity; the LLM path will emit the full form through strict schema.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ClarificationSpec {
    pub reason: ClarificationReason,
    /// Short prompt the UI could show the user, in their language.
    /// Empty string if the pipeline should just use a generic default.
    pub suggestion: String,
}

impl<'de> Deserialize<'de> for ClarificationSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Full {
                reason: ClarificationReason,
                #[serde(default)]
                suggestion: String,
            },
            Bare(ClarificationReason),
        }

        Ok(match Shape::deserialize(deserializer)? {
            Shape::Full { reason, suggestion } => Self { reason, suggestion },
            Shape::Bare(reason) => Self { reason, suggestion: String::new() },
        })
    }
}

// =============================================================================
// Root struct
// =============================================================================

/// Canonical intermediate representation of a user question.
///
/// Invariants that downstream stages can rely on (enforced by compiler prompt
/// + optional post-parse validator, not by the Rust type system):
/// - `QueryAct::Compare` implies `Some(comparison)`.
/// - `QueryAct::FollowUp` usually implies `!conversation_refs.is_empty()`,
///   though a bare interrogative ("What?") can be `FollowUp` with only a
///   `BareInterrogative` ref.
/// - `QueryScope::CrossLibrary` implies the user named another library
///   explicitly — the compiler SHOULD populate `document_focus` or
///   `target_entities` accordingly.
/// - `confidence` ∈ [0.0, 1.0]; values below ~0.6 should cause the pipeline
///   to prefer `needs_clarification` over a confident reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QueryIR {
    pub act: QueryAct,
    pub scope: QueryScope,
    pub language: QueryLanguage,

    /// Open-ended ontology tags (e.g. `"endpoint"`, `"port"`, `"config_key"`,
    /// `"procedure"`, `"protocol"`, `"error_code"`, `"env_var"`, `"metric"`,
    /// `"table_row"`, `"document"`, `"concept"`). New tags are added as
    /// ontology rows in Arango, never as Rust enum variants.
    #[serde(default)]
    pub target_types: Vec<String>,

    #[serde(default)]
    pub target_entities: Vec<EntityMention>,

    /// Verbatim literals (URLs, paths, config keys, code snippets). Drives
    /// the verifier's strictness: a retrieve-value act with literal
    /// constraints is the most security-sensitive branch.
    #[serde(default)]
    pub literal_constraints: Vec<LiteralSpan>,

    /// Temporal constraints compiled into provider-normalized ISO bounds.
    /// Downstream retrieval consumes these typed bounds instead of parsing
    /// natural-language date wording.
    #[serde(default)]
    pub temporal_constraints: Vec<TemporalConstraint>,

    #[serde(default)]
    pub comparison: Option<ComparisonSpec>,

    #[serde(default)]
    pub document_focus: Option<DocumentHint>,

    /// Anaphora / deixis / ellipsis the compiler observed but did not resolve.
    /// Session-level resolver consumes this against prior turns.
    #[serde(default)]
    pub conversation_refs: Vec<UnresolvedRef>,

    /// Populated only when the compiler is not confident enough to proceed.
    #[serde(default)]
    pub needs_clarification: Option<ClarificationSpec>,

    /// Optional ordered slice request for sequential sources. Downstream
    /// retrieval may consume this only after resolving it to a structured
    /// source revision; it must not re-classify natural-language wording.
    #[serde(default)]
    pub source_slice: Option<SourceSliceSpec>,

    /// Compiler self-assessed confidence ∈ [0.0, 1.0]. Defaults to
    /// `1.0` when omitted so the golden evaluation set (which does not
    /// carry per-row confidence) deserialises as ground-truth.
    #[serde(default = "default_ground_truth_confidence")]
    pub confidence: f32,
}

const fn default_ground_truth_confidence() -> f32 {
    1.0
}

// =============================================================================
// Derived routing helpers (consumed by downstream stages instead of keyword
// lists). Kept as plain methods on the IR so the callsites stay readable.
// =============================================================================

impl QueryIR {
    /// `true` when the query targets an exact configuration value.
    /// Drives verifier strictness and fact-search bias.
    #[must_use]
    pub fn is_exact_literal_technical(&self) -> bool {
        matches!(self.act, QueryAct::RetrieveValue) && !self.literal_constraints.is_empty()
    }

    /// `true` when the query scope spans multiple documents.
    #[must_use]
    pub const fn is_multi_document(&self) -> bool {
        matches!(self.scope, QueryScope::MultiDocument)
    }

    /// `true` when the query is a follow-up — either explicitly declared
    /// by the compiler or evidenced by unresolved conversation refs.
    #[must_use]
    pub fn is_follow_up(&self) -> bool {
        matches!(self.act, QueryAct::FollowUp) || !self.conversation_refs.is_empty()
    }

    /// Overview-style questions need source coverage, not just the
    /// highest-scoring local passage. The signal is structural: broad
    /// acts without exact literals or comparisons. Natural-language
    /// wording stays inside the compiler output instead of becoming
    /// downstream keyword lists.
    #[must_use]
    pub fn requests_source_coverage_context(&self) -> bool {
        matches!(self.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta)
            && self.literal_constraints.is_empty()
            && self.comparison.is_none()
            && !self.is_follow_up()
    }

    #[must_use]
    pub const fn requests_source_slice_context(&self) -> bool {
        self.source_slice.is_some()
    }

    /// Aggregate the typed `start` / `end` RFC3339 bounds from every
    /// `temporal_constraints` entry into a single conservative range
    /// (`min(start)`, `max(end)`). Returns `(None, None)` when no
    /// constraint resolves to a parseable bound — downstream retrieval
    /// then skips the temporal hard-filter and falls back to the
    /// keyword-boost path. Compiler is the canonical NL date parser; this
    /// helper does only structural RFC3339 parsing per CLAUDE.md
    /// "запрещено держать захардкоженные словари... под конкретный
    /// естественный язык".
    #[must_use]
    pub fn resolved_temporal_bounds(
        &self,
    ) -> (Option<chrono::DateTime<chrono::Utc>>, Option<chrono::DateTime<chrono::Utc>>) {
        let parse = |value: &str| -> Option<chrono::DateTime<chrono::Utc>> {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|parsed| parsed.with_timezone(&chrono::Utc))
        };
        let mut start: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut end: Option<chrono::DateTime<chrono::Utc>> = None;
        for constraint in &self.temporal_constraints {
            if let Some(parsed) = constraint.start.as_deref().and_then(parse) {
                start = Some(start.map_or(parsed, |existing| existing.min(parsed)));
            }
            if let Some(parsed) = constraint.end.as_deref().and_then(parse) {
                end = Some(end.map_or(parsed, |existing| existing.max(parsed)));
            }
        }
        (start, end)
    }

    /// Verifier strictness derived from IR, replacing the implicit
    /// "unsupported_literal → stub" guard. `Strict` = suppress on
    /// hallucinated literals; `Moderate` = warnings only; `Lenient` =
    /// metadata annotation only.
    #[must_use]
    pub fn verification_level(&self) -> VerificationLevel {
        match self.act {
            QueryAct::RetrieveValue if !self.literal_constraints.is_empty() => {
                VerificationLevel::Strict
            }
            QueryAct::Compare | QueryAct::RetrieveValue => VerificationLevel::Moderate,
            _ => VerificationLevel::Lenient,
        }
    }

    /// True only when the compiler explicitly asked for clarification.
    ///
    /// `confidence` remains an uncertainty signal for downstream
    /// ranking and verification, but low confidence alone is not a
    /// canonical reason to interrupt a grounded answer path once
    /// retrieval has enough evidence to proceed.
    #[must_use]
    pub fn should_request_clarification(&self) -> bool {
        self.needs_clarification.is_some()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    /// Drop the answer to a safe stub if any literal is unsupported.
    /// Reserved for exact-value requests where hallucination cost is high.
    Strict,
    /// Emit verification warnings but surface the answer to the user.
    Moderate,
    /// Attach metadata only; never change what the user sees.
    Lenient,
}

// =============================================================================
// JSON Schema for provider structured output.
// =============================================================================

/// Returns the OpenAI-strict-compatible JSON Schema describing [`QueryIR`].
///
/// Written by hand (rather than generated via `schemars`) so we can guarantee
/// the result validates under OpenAI's `strict: true` mode, which disallows
/// several JSON Schema constructs that generators emit by default
/// (`oneOf`, `anyOf` at top level, untyped `additionalProperties`, etc.).
///
/// For providers that don't support strict JSON Schema (Ollama, older
/// DeepSeek builds), the same schema is attached in the prompt as a
/// documentation block and the request uses `response_format: json_object`.
#[must_use]
pub fn query_ir_json_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [
            "act",
            "scope",
            "language",
            "target_types",
            "target_entities",
            "literal_constraints",
            "temporal_constraints",
            "comparison",
            "document_focus",
            "conversation_refs",
            "needs_clarification",
            "source_slice",
            "confidence"
        ],
        "properties": {
            "act": {
                "type": "string",
                "enum": [
                    "retrieve_value",
                    "describe",
                    "configure_how",
                    "compare",
                    "enumerate",
                    "meta",
                    "follow_up"
                ]
            },
            "scope": {
                "type": "string",
                "enum": ["single_document", "multi_document", "cross_library", "library_meta"]
            },
            "language": {
                "type": "string",
                "enum": ["en", "ru", "auto"]
            },
            "target_types": {
                "type": "array",
                "items": { "type": "string" }
            },
            "target_entities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["label", "role"],
                    "properties": {
                        "label": { "type": "string" },
                        "role": {
                            "type": "string",
                            "enum": ["subject", "object", "modifier"]
                        }
                    }
                }
            },
            "literal_constraints": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["text", "kind"],
                    "properties": {
                        "text": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": [
                                "url",
                                "path",
                                "identifier",
                                "version",
                                "numeric_code",
                                "other"
                            ]
                        }
                    }
                }
            },
            "temporal_constraints": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["surface", "start", "end"],
                    "properties": {
                        "surface": { "type": "string" },
                        "start": { "type": ["string", "null"] },
                        "end": { "type": ["string", "null"] }
                    }
                }
            },
            "comparison": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": ["a", "b", "dimension"],
                "properties": {
                    "a": { "type": ["string", "null"] },
                    "b": { "type": ["string", "null"] },
                    "dimension": { "type": "string" }
                }
            },
            "document_focus": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": ["hint"],
                "properties": {
                    "hint": { "type": "string" }
                }
            },
            "conversation_refs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["surface", "kind"],
                    "properties": {
                        "surface": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": ["pronoun", "deictic", "elliptic", "bare_interrogative"]
                        }
                    }
                }
            },
            "needs_clarification": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": ["reason", "suggestion"],
                "properties": {
                    "reason": {
                        "type": "string",
                        "enum": [
                            "ambiguous_too_short",
                            "multiple_interpretations",
                            "anaphora_unresolved",
                            "unknown_target_type"
                        ]
                    },
                    "suggestion": { "type": "string" }
                }
            },
            "source_slice": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": ["direction", "count"],
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["head", "tail", "all"]
                    },
                    "count": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": 30
                    }
                }
            },
            "confidence": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_descriptive_question_round_trips() {
        let ir = QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Ru,
            target_types: vec!["procedure".to_string()],
            target_entities: vec![EntityMention {
                label: "payment module".to_string(),
                role: EntityRole::Subject,
            }],
            literal_constraints: vec![],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        };
        let json = serde_json::to_value(&ir).unwrap();
        let parsed: QueryIR = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, ir);
    }

    #[test]
    fn exact_literal_question_routes_strict() {
        let ir = QueryIR {
            act: QueryAct::RetrieveValue,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::En,
            target_types: vec!["endpoint".to_string()],
            target_entities: vec![],
            literal_constraints: vec![LiteralSpan {
                text: "/system/info".to_string(),
                kind: LiteralKind::Path,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.95,
        };
        assert!(ir.is_exact_literal_technical());
        assert_eq!(ir.verification_level(), VerificationLevel::Strict);
        assert!(!ir.is_follow_up());
    }

    #[test]
    fn follow_up_detects_from_refs() {
        let ir = QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Ru,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![UnresolvedRef {
                surface: "there".to_string(),
                kind: ConversationRefKind::Deictic,
            }],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.7,
        };
        assert!(ir.is_follow_up());
        assert_eq!(ir.verification_level(), VerificationLevel::Lenient);
    }

    #[test]
    fn low_confidence_alone_does_not_trigger_clarification() {
        let ir = QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.4,
        };
        assert!(!ir.should_request_clarification());
    }

    #[test]
    fn explicit_clarification_reason_triggers_clarification() {
        let ir = QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: Some(ClarificationSpec {
                reason: ClarificationReason::AmbiguousTooShort,
                suggestion: String::new(),
            }),
            source_slice: None,
            confidence: 0.9,
        };
        assert!(ir.should_request_clarification());
    }

    #[test]
    fn broad_descriptive_ir_requests_source_coverage() {
        let ir = QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.8,
        };
        assert!(ir.requests_source_coverage_context());
    }

    #[test]
    fn source_slice_round_trips_as_typed_ir() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["record".to_string()],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: Some(SourceSliceSpec {
                direction: SourceSliceDirection::Tail,
                count: Some(20),
            }),
            confidence: 0.86,
        };

        let json = serde_json::to_value(&ir).unwrap();
        let parsed: QueryIR = serde_json::from_value(json).unwrap();

        assert!(parsed.requests_source_slice_context());
        assert_eq!(parsed.source_slice, ir.source_slice);
    }

    #[test]
    fn resolved_temporal_bounds_aggregates_min_start_and_max_end() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![
                TemporalConstraint {
                    surface: "window-A".to_string(),
                    start: Some("2026-03-01T00:00:00Z".to_string()),
                    end: Some("2026-03-31T23:59:59Z".to_string()),
                },
                TemporalConstraint {
                    surface: "window-B".to_string(),
                    start: Some("2026-02-01T00:00:00Z".to_string()),
                    end: Some("2026-02-28T23:59:59Z".to_string()),
                },
            ],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        };

        let (start, end) = ir.resolved_temporal_bounds();
        let start = start.expect("min start");
        let end = end.expect("max end");
        assert_eq!(start.to_rfc3339(), "2026-02-01T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-03-31T23:59:59+00:00");
    }

    #[test]
    fn resolved_temporal_bounds_returns_none_when_constraints_empty() {
        let ir = QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.5,
        };

        assert_eq!(ir.resolved_temporal_bounds(), (None, None));
    }

    #[test]
    fn resolved_temporal_bounds_returns_only_start_when_only_start_provided() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![TemporalConstraint {
                surface: "window-half-open".to_string(),
                start: Some("2026-04-01T00:00:00Z".to_string()),
                end: None,
            }],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.7,
        };
        let (start, end) = ir.resolved_temporal_bounds();
        assert!(start.is_some());
        assert_eq!(end, None);
        assert_eq!(start.unwrap().to_rfc3339(), "2026-04-01T00:00:00+00:00");
    }

    #[test]
    fn resolved_temporal_bounds_returns_only_end_when_only_end_provided() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![TemporalConstraint {
                surface: "window-half-closed".to_string(),
                start: None,
                end: Some("2026-04-30T23:59:59Z".to_string()),
            }],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.7,
        };
        let (start, end) = ir.resolved_temporal_bounds();
        assert_eq!(start, None);
        assert!(end.is_some());
        assert_eq!(end.unwrap().to_rfc3339(), "2026-04-30T23:59:59+00:00");
    }

    #[test]
    fn resolved_temporal_bounds_aggregates_parseable_when_mixed_with_unparseable() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![
                TemporalConstraint {
                    surface: "window-good".to_string(),
                    start: Some("2026-05-01T00:00:00Z".to_string()),
                    end: Some("2026-05-31T23:59:59Z".to_string()),
                },
                TemporalConstraint {
                    surface: "window-bad".to_string(),
                    start: Some("not a date".to_string()),
                    end: Some("also not a date".to_string()),
                },
            ],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        };
        let (start, end) = ir.resolved_temporal_bounds();
        // Only the parseable surface contributes; the broken one is silently skipped.
        assert_eq!(start.unwrap().to_rfc3339(), "2026-05-01T00:00:00+00:00");
        assert_eq!(end.unwrap().to_rfc3339(), "2026-05-31T23:59:59+00:00");
    }

    #[test]
    fn resolved_temporal_bounds_skips_unparseable_rfc3339_surfaces() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![TemporalConstraint {
                surface: "later".to_string(),
                start: Some("not a date".to_string()),
                end: None,
            }],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.4,
        };

        assert_eq!(ir.resolved_temporal_bounds(), (None, None));
    }

    #[test]
    fn temporal_constraints_round_trip_as_typed_ir() {
        let ir = QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["record".to_string()],
            target_entities: vec![],
            literal_constraints: vec![],
            temporal_constraints: vec![TemporalConstraint {
                surface: "period 2026-03".to_string(),
                start: Some("2026-03-01T00:00:00Z".to_string()),
                end: Some("2026-04-01T00:00:00Z".to_string()),
            }],
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.86,
        };

        let json = serde_json::to_value(&ir).unwrap();
        let parsed: QueryIR = serde_json::from_value(json).unwrap();

        assert_eq!(parsed.temporal_constraints, ir.temporal_constraints);
    }

    #[test]
    fn exact_and_follow_up_ir_do_not_request_source_coverage() {
        let exact_ir = QueryIR {
            act: QueryAct::RetrieveValue,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![],
            target_entities: vec![],
            literal_constraints: vec![LiteralSpan {
                text: "DATABASE_URL".to_string(),
                kind: LiteralKind::Identifier,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: vec![],
            needs_clarification: None,
            source_slice: None,
            confidence: 0.8,
        };
        let follow_up_ir = QueryIR {
            act: QueryAct::Describe,
            conversation_refs: vec![UnresolvedRef {
                surface: "that".to_string(),
                kind: ConversationRefKind::Deictic,
            }],
            ..exact_ir.clone()
        };
        assert!(!exact_ir.requests_source_coverage_context());
        assert!(!follow_up_ir.requests_source_coverage_context());
    }

    #[test]
    fn schema_has_all_top_level_properties() {
        let schema = query_ir_json_schema();
        let required = schema["required"].as_array().unwrap();
        for field in [
            "act",
            "scope",
            "language",
            "target_types",
            "target_entities",
            "literal_constraints",
            "temporal_constraints",
            "comparison",
            "document_focus",
            "conversation_refs",
            "needs_clarification",
            "source_slice",
            "confidence",
        ] {
            assert!(required.iter().any(|value| value == field), "schema should require `{field}`");
        }
    }
}

/// Canonical QueryIR cache discriminator. When compiler semantics change,
/// obsolete cache rows are discarded instead of read through compatibility
/// aliases or parallel cache generations.
pub const QUERY_IR_SCHEMA_VERSION: u16 = 6;

/// Maximum age of a Postgres-tier `query_ir_cache` row before it is
/// treated as a miss. Redis already holds its own 24h hot tier; the
/// persistent tier keeps compilations for 30 days so operators can
/// audit yesterday's "what IR did we derive for this question" decision
/// while protecting against unbounded row growth on a busy library.
pub const QUERY_IR_CACHE_MAX_AGE_DAYS: i64 = 30;

/// Self-consistency issue picked up by [`validate_ir`]. Caught in debug
/// builds as an assertion so it shouts early in tests; production paths
/// log and keep going (a minor invariant failure is not worth bringing
/// the pipeline down for).
#[derive(Debug, Clone, PartialEq)]
pub enum QueryIrValidationError {
    CompareWithoutComparison,
    FollowUpWithoutRefs,
    ConfidenceOutOfRange(f32),
}

/// Verify structural invariants the compiler prompt is supposed to
/// maintain. Returns the first error seen so downstream noise stays low.
///
/// - `act = Compare` must carry a `comparison` block so downstream
///   answer builders have both sides.
/// - `act = FollowUp` must either declare at least one
///   `conversation_ref` or be low-confidence (≥ 0.5 would mean the
///   compiler was sure about follow-up WITHOUT ever pointing at what
///   the user referenced — nonsense).
/// - `confidence` must be a finite number in `[0.0, 1.0]`.
pub fn validate_ir(ir: &QueryIR) -> Result<(), QueryIrValidationError> {
    if !(0.0..=1.0).contains(&ir.confidence) || !ir.confidence.is_finite() {
        return Err(QueryIrValidationError::ConfidenceOutOfRange(ir.confidence));
    }
    if matches!(ir.act, QueryAct::Compare) && ir.comparison.is_none() {
        return Err(QueryIrValidationError::CompareWithoutComparison);
    }
    if matches!(ir.act, QueryAct::FollowUp)
        && ir.conversation_refs.is_empty()
        && ir.confidence >= 0.5
    {
        return Err(QueryIrValidationError::FollowUpWithoutRefs);
    }
    Ok(())
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn base_ir(act: QueryAct) -> QueryIR {
        QueryIR {
            act,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::En,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn valid_descriptive_ir_passes() {
        assert!(validate_ir(&base_ir(QueryAct::Describe)).is_ok());
    }

    #[test]
    fn compare_without_comparison_fails() {
        assert_eq!(
            validate_ir(&base_ir(QueryAct::Compare)),
            Err(QueryIrValidationError::CompareWithoutComparison)
        );
    }

    #[test]
    fn follow_up_without_refs_and_confident_fails() {
        let mut ir = base_ir(QueryAct::FollowUp);
        ir.confidence = 0.9;
        assert_eq!(validate_ir(&ir), Err(QueryIrValidationError::FollowUpWithoutRefs));
    }

    #[test]
    fn follow_up_without_refs_but_low_confidence_passes() {
        let mut ir = base_ir(QueryAct::FollowUp);
        ir.confidence = 0.3;
        assert!(validate_ir(&ir).is_ok());
    }

    #[test]
    fn confidence_out_of_range_fails() {
        let mut ir = base_ir(QueryAct::Describe);
        ir.confidence = 1.5;
        assert!(matches!(validate_ir(&ir), Err(QueryIrValidationError::ConfidenceOutOfRange(_))));
    }
}
