//! Shared intermediate representation produced by `QueryCompiler`.
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
//! 2. **Behavioral classifications are typed.** `target_types` is a finite
//!    protocol enum because downstream routing depends on it. Open ontology
//!    labels belong in evidence or entity metadata and must never drive query
//!    control flow as unchecked strings.
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
//! through provider structured outputs (`OpenAI` `json_schema` strict mode,
//! or `json_object` + prompt-engineering fallback for providers that don't
//! support strict mode — see `docs/query_compiler_provider_audit.md`).

use serde::{Deserialize, Serialize};

// =============================================================================
// Core axes — finite enums the downstream pipeline dispatches on.
// =============================================================================

/// What the user is fundamentally asking the system to do.
///
/// Matches the seven acts that the golden set's labelling guide enumerates.
/// The verification guard strictness, the answer builder choice, and the
/// source-link rendering all key off this.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryLanguage {
    En,
    Ru,
    /// Other / mixed / indeterminate. Deterministic answer labels fall back to
    /// the best matching supported label resource, then to the product default.
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiteralKind {
    /// Looks like http(s)://..., including API-style paths after a method.
    Url,
    /// Filesystem or URL path (`/etc/app/config.ini`, `/api/v1/orders`).
    Path,
    /// Identifier in camelCase / `snake_case` / `SCREAMING_CASE`
    /// (`fetchUserDetails`, `DATABASE_URL`, `with_cards`).
    Identifier,
    /// Semver / release version (`4.5.1`, `1.2`).
    Version,
    /// Numeric-looking code (`71`, `500`, port number `8080`).
    NumericCode,
    /// Any other verbatim literal the user quoted (backticked string,
    /// inline SQL snippet, config line).
    Other,
}

/// Kind of conversational reference the compiler could not resolve.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceSliceDirection {
    /// Earliest units in the source order.
    Head,
    /// Latest units in the source order.
    Tail,
    /// Bounded representation of the whole ordered source.
    All,
}

/// Optional structural filter applied before taking an ordered source slice.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceSliceFilter {
    /// No structural filter; slice the ordered source exactly as stored.
    None,
    /// Keep only source units containing a version-shaped release marker.
    ReleaseMarker,
}

/// Why the compiler is unsure and would prefer clarification from the user.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
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

/// Finite semantic target contract emitted by `QueryCompiler`.
///
/// These values are protocol identifiers, not natural-language keywords. The
/// compiler may select only a value advertised by the structured-output
/// schema; serde rejects every other value so an unknown provider response is
/// retried or recovered by the compiler boundary instead of being silently
/// ignored by downstream routing.
macro_rules! define_query_target_kinds {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            Serialize,
            Deserialize,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            utoipa::ToSchema,
        )]
        pub enum QueryTargetKind {
            $(#[serde(rename = $wire)] $variant),+
        }

        impl QueryTargetKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }

            /// Parse an exact wire identifier. This intentionally performs no
            /// trimming, case folding, aliases, or natural-language recovery.
            #[must_use]
            pub fn from_wire(value: &str) -> Option<Self> {
                match value {
                    $($wire => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

define_query_target_kinds! {
    Artifact => "artifact",
    Attribute => "attribute",
    BaseUrl => "base_url",
    Changelog => "changelog",
    Concept => "concept",
    ConfigKey => "config_key",
    Configuration => "configuration",
    ConfigurationFile => "configuration_file",
    Connection => "connection",
    ConversationTurn => "conversation_turn",
    Credential => "credential",
    Document => "document",
    Endpoint => "endpoint",
    Entity => "entity",
    Entry => "entry",
    EnvVar => "env_var",
    ErrorCode => "error_code",
    ErrorMessage => "error_message",
    Event => "event",
    Facet => "facet",
    Field => "field",
    FilesystemPath => "filesystem_path",
    Flag => "flag",
    FormatsUnderTest => "formats_under_test",
    Group => "group",
    HttpMethod => "http_method",
    Item => "item",
    Location => "location",
    Metric => "metric",
    Natural => "natural",
    Network => "network",
    Organization => "organization",
    Package => "package",
    Parameter => "parameter",
    Path => "path",
    Person => "person",
    Port => "port",
    PrimaryHeading => "primary_heading",
    Procedure => "procedure",
    Process => "process",
    Protocol => "protocol",
    Record => "record",
    Relationship => "relationship",
    Release => "release",
    Remediation => "remediation",
    Route => "route",
    SecondaryHeading => "secondary_heading",
    Service => "service",
    SoftwareModule => "software_module",
    State => "state",
    Status => "status",
    TableAverage => "table_average",
    TableFrequency => "table_frequency",
    TableRow => "table_row",
    TableSummary => "table_summary",
    Troubleshooting => "troubleshooting",
    Transition => "transition",
    Url => "url",
    Value => "value",
    Version => "version",
    Wsdl => "wsdl",
}

mod types;
pub use types::*;

// =============================================================================
// Root struct
// =============================================================================

/// Shared intermediate representation of a user question.
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryIR {
    pub act: QueryAct,
    pub scope: QueryScope,
    pub language: QueryLanguage,

    /// Typed semantic target identifiers selected by the query compiler.
    /// Unknown provider values fail deserialization at the compiler boundary;
    /// downstream stages never route on unchecked ontology strings.
    #[serde(default)]
    pub target_types: Vec<QueryTargetKind>,

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

    /// Self-contained search string for the retrieval lanes. When the
    /// current question already stands on its own this is the question
    /// verbatim; when it depends on prior turns (ellipsis, anaphora, a
    /// bare disambiguating selection) the compiler folds the recovered
    /// subject and scope into a standalone phrasing so the vector, `HyDE`,
    /// lexical, and technical-fact lanes search the resolved intent
    /// instead of the elliptic fragment. Never a translation: preserves
    /// the original writing system and spelling of every surfaced token.
    /// Optional in the Rust type so golden-eval fixtures that predate the
    /// field still deserialise; the schema requires the compiler to emit it.
    #[serde(default)]
    pub retrieval_query: Option<String>,

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
    /// Bare IR with the given act and scope and no other constraints — the
    /// shared starting point for test fixtures instead of repeated struct
    /// literals.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn test_with_act_scope(act: QueryAct, scope: QueryScope) -> Self {
        Self {
            act,
            scope,
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
            confidence: 0.9,
        }
    }

    /// Test a compiler-selected semantic target without string normalization
    /// or alias matching.
    #[must_use]
    pub fn targets(&self, kind: QueryTargetKind) -> bool {
        self.target_types.contains(&kind)
    }

    /// Test a finite target family without exposing serialized identifiers to
    /// downstream control flow.
    #[must_use]
    pub fn targets_any(&self, kinds: &[QueryTargetKind]) -> bool {
        self.target_types.iter().any(|kind| kinds.contains(kind))
    }

    /// `true` when the query targets an exact configuration value.
    /// Drives verifier strictness and fact-search bias.
    #[must_use]
    pub fn is_exact_literal_technical(&self) -> bool {
        matches!(self.act, QueryAct::RetrieveValue) && self.has_exact_technical_literal()
    }

    #[must_use]
    pub fn has_exact_technical_literal(&self) -> bool {
        self.exact_technical_literals().next().is_some()
    }

    /// Exact-shaped literal texts from `literal_constraints` — the extracted
    /// value itself (e.g. `"GET"`, `"/orion/status"`), not the surrounding
    /// sentence. Feeds the exact-literal fact-search lane, which binds
    /// against a single normalized value and cannot match a multi-word query.
    pub fn exact_technical_literals(&self) -> impl Iterator<Item = &str> {
        self.literal_constraints
            .iter()
            .filter(|literal| {
                literal_kind_has_exact_technical_shape(literal.kind, literal.text.as_str())
            })
            .map(|literal| literal.text.as_str())
    }

    /// `true` when a broad setup question may be answered by multiple
    /// independently actionable procedure documents. This derives only from
    /// compiler-selected typed fields; observed document cardinality remains a
    /// separate retrieval-stage check.
    #[must_use]
    pub fn requests_broad_procedure_variant_coverage(&self) -> bool {
        matches!(self.act, QueryAct::ConfigureHow)
            && self.document_focus.is_none()
            && self.source_slice.is_none()
            && self.literal_constraints.is_empty()
            && self.comparison.is_none()
            && !self.targets_any(&[
                QueryTargetKind::Document,
                QueryTargetKind::Version,
                QueryTargetKind::Release,
                QueryTargetKind::Changelog,
            ])
            && self.targets(QueryTargetKind::Procedure)
    }

    /// `true` when the query scope spans multiple documents.
    #[must_use]
    pub const fn is_multi_document(&self) -> bool {
        matches!(self.scope, QueryScope::MultiDocument)
    }

    /// `true` when the query is a follow-up — either explicitly declared
    /// by the compiler or evidenced by unresolved conversation refs.
    #[must_use]
    pub const fn is_follow_up(&self) -> bool {
        matches!(self.act, QueryAct::FollowUp) || !self.conversation_refs.is_empty()
    }

    /// The search string the retrieval lanes should use: the
    /// compiler-materialised standalone `retrieval_query` when present and
    /// non-blank, otherwise the verbatim user turn. Centralises the
    /// fallback so callers never branch on `Option` and never search a
    /// blank string when a provider omits the field.
    #[must_use]
    pub fn effective_retrieval_query<'a>(&'a self, question: &'a str) -> &'a str {
        self.retrieval_query
            .as_deref()
            .map(str::trim)
            .filter(|resolved| !resolved.is_empty())
            .unwrap_or(question)
    }

    /// Overview-style questions need source coverage, not just the
    /// highest-scoring local passage. The signal is structural: broad
    /// acts without exact literals or comparisons. Natural-language
    /// wording stays inside the compiler output instead of becoming
    /// downstream keyword lists.
    #[must_use]
    pub fn requests_source_coverage_context(&self) -> bool {
        matches!(
            self.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta | QueryAct::RetrieveValue
        ) && !self.has_exact_technical_literal()
            && self.comparison.is_none()
            && self.source_slice.is_none()
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
    /// keyword-boost path. Compiler is the authoritative NL date parser; this
    /// helper does only structural RFC3339 parsing — no hardcoded
    /// natural-language dictionaries are allowed at this layer.
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
    /// "`unsupported_literal` → stub" guard. `Strict` = suppress on
    /// hallucinated literals; `Moderate` = warnings only; `Lenient` =
    /// metadata annotation only.
    #[must_use]
    pub fn verification_level(&self) -> VerificationLevel {
        match self.act {
            QueryAct::RetrieveValue if self.has_exact_technical_literal() => {
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
    /// sufficient reason to interrupt a grounded answer path once
    /// retrieval has enough evidence to proceed.
    #[must_use]
    pub const fn should_request_clarification(&self) -> bool {
        self.needs_clarification.is_some()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    /// Surface only answers that pass the deterministic literal, named-claim,
    /// conflict, and structural checks; replace every other result with a safe
    /// response after the final repair and verification pass. Reserved for
    /// exact-value requests where hallucination cost is high.
    Strict,
    /// Emit verification warnings but surface the answer to the user.
    Moderate,
    /// Attach metadata only; never change what the user sees.
    Lenient,
}

mod schema;
pub use schema::*;

#[cfg(test)]
mod tests;

/// Current `QueryIR` cache discriminator. When compiler semantics change,
/// obsolete cache rows are discarded instead of read through compatibility
/// aliases or parallel cache generations.
pub const QUERY_IR_SCHEMA_VERSION: u16 = 12;

/// Maximum age of a Postgres-tier `query_ir_cache` row before it is
/// treated as a miss. Redis already holds its own 24h hot tier; the
/// persistent tier keeps compilations for 30 days so operators can
/// audit yesterday's "what IR did we derive for this question" decision
/// while protecting against unbounded row growth on a busy library.
pub const QUERY_IR_CACHE_MAX_AGE_DAYS: i64 = 30;

/// Self-consistency issue picked up by [`validate_ir`]. The query compiler
/// rejects these invariants in every build before an IR can reach planning or
/// retrieval; tests exercise the same validation path used in production.
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
mod validation_tests;
