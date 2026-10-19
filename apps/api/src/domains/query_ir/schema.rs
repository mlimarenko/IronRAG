use super::*;
use serde_json::{Value, json};

// =============================================================================
// JSON Schema for provider structured output.
// =============================================================================

/// Returns the OpenAI-strict-compatible JSON Schema describing [`QueryIR`].
///
/// Written by hand (rather than generated via `schemars`) so we can guarantee
/// the result validates under `OpenAI`'s `strict: true` mode, which disallows
/// several JSON Schema constructs that generators emit by default
/// (`oneOf`, `anyOf` at top level, untyped `additionalProperties`, etc.).
///
/// For providers that don't support strict JSON Schema (Ollama, older
/// `DeepSeek` builds), the same schema is attached in the prompt as a
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
            "retrieval_query",
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
                "items": {
                    "type": "string",
                    "enum": QueryTargetKind::ALL
                        .iter()
                        .map(|kind| kind.as_str())
                        .collect::<Vec<_>>()
                }
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
                "required": ["direction", "count", "filter"],
                "properties": {
                    "direction": {
                        "type": "string",
                        "enum": ["head", "tail", "all"]
                    },
                    "count": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": 30
                    },
                    "filter": {
                        "type": "string",
                        "enum": ["none", "release_marker"]
                    }
                }
            },
            "retrieval_query": {
                "type": "string"
            },
            "confidence": {
                "type": "number",
                "minimum": 0.0,
                "maximum": 1.0
            }
        }
    })
}
