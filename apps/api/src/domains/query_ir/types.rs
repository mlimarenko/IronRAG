use super::*;

// =============================================================================
// Composite types
// =============================================================================

/// Named thing the user talks about, with role in the question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityMention {
    /// Exact non-empty, surrounding-whitespace-free substring as written by
    /// the user in the current question or supplied conversation history.
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
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
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
        } else if literal_text_is_identifier_shaped(trimmed) {
            Self::Identifier
        } else {
            Self::Other
        }
    }
}

/// Script-agnostic structural identifier signal.
///
/// A plain alphabetic word in any writing system is usually a topic/entity
/// echo, not a technical identifier. Technical identifier routing should only
/// trigger when the literal itself carries structural evidence: separators,
/// digits, mixed Unicode case, or all-uppercase acronym shape.
#[must_use]
pub fn literal_text_is_identifier_shaped(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return false;
    }

    identifier_shape_signals(trimmed).is_some_and(IdentifierShapeSignals::identifier_shaped)
}

#[derive(Default)]
struct IdentifierShapeSignals {
    has_alphabetic: bool,
    has_lowercase: bool,
    has_uppercase: bool,
    has_numeric: bool,
    has_separator: bool,
    seen_lowercase_before: bool,
    has_uppercase_after_lowercase: bool,
}

impl IdentifierShapeSignals {
    fn record(mut self, ch: char) -> Option<Self> {
        match identifier_character_kind(ch) {
            IdentifierCharacterKind::Alphabetic => {
                self.has_alphabetic = true;
                self.has_uppercase |= ch.is_uppercase();
                self.has_uppercase_after_lowercase |=
                    ch.is_uppercase() && self.seen_lowercase_before;
                self.has_lowercase |= ch.is_lowercase();
                self.seen_lowercase_before |= ch.is_lowercase();
                Some(self)
            }
            IdentifierCharacterKind::Numeric => {
                self.has_numeric = true;
                Some(self)
            }
            IdentifierCharacterKind::Separator => {
                self.has_separator = true;
                Some(self)
            }
            IdentifierCharacterKind::Invalid => None,
        }
    }

    const fn identifier_shaped(self) -> bool {
        self.has_separator
            || self.has_numeric
            || (self.has_lowercase && self.has_uppercase_after_lowercase)
            || (self.has_alphabetic && self.has_uppercase && !self.has_lowercase)
    }
}

fn identifier_shape_signals(text: &str) -> Option<IdentifierShapeSignals> {
    text.chars().try_fold(IdentifierShapeSignals::default(), IdentifierShapeSignals::record)
}

enum IdentifierCharacterKind {
    Alphabetic,
    Numeric,
    Separator,
    Invalid,
}

fn identifier_character_kind(ch: char) -> IdentifierCharacterKind {
    if ch.is_alphabetic() {
        IdentifierCharacterKind::Alphabetic
    } else if ch.is_numeric() {
        IdentifierCharacterKind::Numeric
    } else if matches!(ch, '_' | '-' | '.') {
        IdentifierCharacterKind::Separator
    } else {
        IdentifierCharacterKind::Invalid
    }
}

#[must_use]
pub fn literal_kind_has_exact_technical_shape(kind: LiteralKind, text: &str) -> bool {
    match kind {
        LiteralKind::Url | LiteralKind::Path | LiteralKind::Version | LiteralKind::NumericCode => {
            true
        }
        LiteralKind::Identifier => literal_text_is_identifier_shaped(text),
        LiteralKind::Other => {
            text.trim().chars().any(|ch| !ch.is_alphabetic() && !ch.is_whitespace())
        }
    }
}

/// When `QueryAct::Compare`, the two sides and the dimension compared.
///
/// `a` and `b` are optional because the user may ask a comparison without
/// naming both sides explicitly ("compare both services", "compare these two").
/// The resolver picks the implicit sides from session state or document
/// focus when possible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonSpec {
    #[serde(default)]
    pub a: Option<String>,
    #[serde(default)]
    pub b: Option<String>,
    /// Free-form ontology tag ("protocol", "performance",
    /// "`feature_coverage`"). Not enforced by the type system — grown via
    /// ontology entries.
    pub dimension: String,
}

/// Hint that pins the question to a specific document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentHint {
    /// Surface string the user used to identify the document
    /// (title fragment, filename, section name).
    pub hint: String,
}

/// Unresolved reference the session resolver will fill from prior turns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedRef {
    /// The exact surface form used ("here", "this", "the same", "that one").
    pub surface: String,
    pub kind: ConversationRefKind,
}

/// Ordered slice request over a sequential source. The compiler sets this
/// only when the user explicitly asks for a positional range of records/items
/// in an ordered source. Ordinary summaries and needle lookups leave it null.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceSliceSpec {
    pub direction: SourceSliceDirection,
    #[serde(default)]
    pub count: Option<u16>,
    #[serde(default = "SourceSliceSpec::default_filter")]
    pub filter: SourceSliceFilter,
}

impl SourceSliceSpec {
    const fn default_filter() -> SourceSliceFilter {
        SourceSliceFilter::None
    }
}

/// Date/time or date-range constraint normalized by the query compiler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Serialize, PartialEq, Eq, utoipa::ToSchema)]
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
