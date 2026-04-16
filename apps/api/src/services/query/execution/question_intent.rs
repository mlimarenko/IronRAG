//! Typed question intent classification.
//!
//! Replaces the scattered `contains("url")` / `contains("адрес")`
//! keyword matching across `technical_literals.rs` and
//! `focused_document_answer.rs` with a single data-driven classifier.
//! Each intent maps to a set of bilingual keyword triggers (EN + RU).
//! The classifier runs once per question and produces a set of intents
//! that downstream answer builders can route on.
//!
//! This is Phase 3 of the extraction pipeline refactor — it does NOT
//! introduce embedding-based or LLM-based intent classification yet.
//! The keywords are the same ones that were previously inline; the
//! improvement is that they live in one canonical table instead of
//! being copy-pasted across 4 files.

/// A recognized question intent. Downstream builders use these to
/// pick the right answer strategy (fact-store lookup, evidence scan,
/// LLM synthesis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuestionIntent {
    /// "What is the URL/endpoint/WSDL for..."
    Endpoint,
    /// "What parameters does X accept?"
    Parameter,
    /// "What HTTP method / GET or POST?"
    HttpMethod,
    /// "What version / which release?"
    Version,
    /// "What is the error code / what does E1234 mean?"
    ErrorCode,
    /// "What environment variable / $DATABASE_URL?"
    EnvVar,
    /// "What is the config key / setting / default value?"
    ConfigKey,
    /// "What protocol — REST, SOAP, GraphQL?"
    Protocol,
    /// "What is the base URL / prefix?"
    BasePrefix,
    /// "What port does X use?"
    Port,
    /// "Which formats are listed under test in this document?"
    FocusedFormatsUnderTest,
    /// "What report name / validating heading does this document contain?"
    FocusedSecondaryHeading,
    /// "What is the title / primary heading of this document?"
    FocusedPrimaryHeading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactUrlLookupKind {
    Url,
    Wsdl,
}

/// Bilingual keyword set for one intent.
struct IntentKeywords {
    intent: QuestionIntent,
    keywords: &'static [&'static str],
}

/// Canonical keyword table. Each row maps an intent to the set of
/// substrings that trigger it. The same substring can appear in
/// multiple intents (e.g. "endpoint" triggers both Endpoint
/// and HttpMethod).
static INTENT_TABLE: &[IntentKeywords] = &[
    IntentKeywords {
        intent: QuestionIntent::Endpoint,
        keywords: &[
            "url",
            "wsdl",
            "адрес",
            "ссылка",
            "endpoint",
            "эндпоинт",
            "api address",
            "api route",
            "маршрут api",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::BasePrefix,
        keywords: &["префикс", "base url", "базовый url", "base path"],
    },
    IntentKeywords {
        intent: QuestionIntent::Parameter,
        keywords: &[
            "параметр",
            "аргумент",
            "пейджинац",
            "query parameter",
            "request parameter",
            "parameter name",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::HttpMethod,
        keywords: &[
            "http method",
            "метод http",
            "get ",
            "post ",
            "put ",
            "patch ",
            "delete ",
            "http verb",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::Version,
        keywords: &["version", "версия", "release", "релиз", "обновлен"],
    },
    IntentKeywords {
        intent: QuestionIntent::ErrorCode,
        keywords: &[
            "error code",
            "код ошибки",
            "error message",
            "exception",
            "ошибка",
            "status code",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::EnvVar,
        keywords: &[
            "environment variable",
            "env var",
            "переменная окружения",
            "переменная среды",
            "$",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::ConfigKey,
        keywords: &[
            "config",
            "конфиг",
            "настройк",
            "setting",
            "default value",
            "значение по умолчанию",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::Protocol,
        keywords: &[
            "protocol",
            "протокол",
            "graphql",
            "soap",
            "rest ",
            "restful",
            "grpc",
            "websocket",
        ],
    },
    IntentKeywords {
        intent: QuestionIntent::Port,
        keywords: &["port", "порт", "listen on", "слушает на"],
    },
];

/// Classify a question into a set of intents by scanning the
/// canonical keyword table. Returns all matching intents — a question
/// like "What is the REST endpoint URL?" will match both
/// Endpoint and Protocol.
pub fn classify_question_intents(question: &str) -> Vec<QuestionIntent> {
    let lowered = question.to_lowercase();
    let mut intents = Vec::new();
    for entry in INTENT_TABLE {
        let matches = if entry.intent == QuestionIntent::Port {
            question_mentions_port(question)
        } else {
            entry.keywords.iter().any(|kw| lowered.contains(kw))
        };
        if matches {
            intents.push(entry.intent);
        }
    }

    if matches_formats_under_test(&lowered) {
        intents.push(QuestionIntent::FocusedFormatsUnderTest);
    }
    if matches_secondary_heading_request(&lowered) {
        intents.push(QuestionIntent::FocusedSecondaryHeading);
    }
    if matches_primary_heading_request(&lowered) {
        intents.push(QuestionIntent::FocusedPrimaryHeading);
    }

    intents
}

/// Check if any of the classified intents indicate the question is
/// about technical API/endpoint surface (as opposed to general prose).
pub fn has_technical_surface_intent(intents: &[QuestionIntent]) -> bool {
    intents.iter().any(|i| {
        matches!(
            i,
            QuestionIntent::Endpoint
                | QuestionIntent::Parameter
                | QuestionIntent::HttpMethod
                | QuestionIntent::BasePrefix
                | QuestionIntent::Port
        )
    })
}

pub fn has_question_intent(intents: &[QuestionIntent], intent: QuestionIntent) -> bool {
    intents.contains(&intent)
}

pub fn question_mentions_graphql(question: &str) -> bool {
    question.to_lowercase().contains("graphql")
}

pub fn classify_exact_url_lookup(
    question: &str,
    intents: &[QuestionIntent],
) -> Option<ExactUrlLookupKind> {
    if !has_question_intent(intents, QuestionIntent::Endpoint) {
        return None;
    }

    let lowered = question.to_lowercase();
    let asks_wsdl = lowered.contains("wsdl");
    let asks_url_like = asks_wsdl
        || (["url", "адрес", "ссылка", "link"].iter().any(|needle| lowered.contains(needle))
            && !["endpoint", "эндпоинт"].iter().any(|needle| lowered.contains(needle)));

    asks_url_like.then_some(if asks_wsdl {
        ExactUrlLookupKind::Wsdl
    } else {
        ExactUrlLookupKind::Url
    })
}

pub fn question_mentions_port(question: &str) -> bool {
    question.to_lowercase().split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|token| {
        matches!(token, "port" | "ports" | "tcp_port" | "udp_port" | "порт" | "порта" | "порты")
    })
}

pub fn question_blocks_endpoint_lookup(question: &str) -> bool {
    let lowered = question.to_lowercase();
    ["сравн", "compare", "difference", "differ from", "differs", "протокол", "protocol"]
        .iter()
        .any(|needle| lowered.contains(needle))
        || question_mentions_port(question)
}

pub fn question_asks_transport_comparison(question: &str) -> bool {
    let lowered = question.to_lowercase();
    let asks_comparison = ["отлич", "compare", "difference", "differ from", "differs"]
        .iter()
        .any(|needle| lowered.contains(needle));
    let mentions_transport =
        ["transport", "протокол", "транспорт"].iter().any(|needle| lowered.contains(needle));
    let mentions_rest = lowered.contains("rest");
    let mentions_wsdl_or_soap = lowered.contains("wsdl") || lowered.contains("soap");

    asks_comparison && mentions_transport && mentions_rest && mentions_wsdl_or_soap
}

pub fn classify_focused_document_intent(question: &str) -> Option<QuestionIntent> {
    let intents = classify_question_intents(question);
    [
        QuestionIntent::FocusedFormatsUnderTest,
        QuestionIntent::FocusedSecondaryHeading,
        QuestionIntent::FocusedPrimaryHeading,
    ]
    .into_iter()
    .find(|intent| intents.contains(intent))
}

fn matches_secondary_heading_request(lowered_question: &str) -> bool {
    lowered_question.contains("report name")
        || lowered_question.contains("название отч")
        || lowered_question.contains("имя отч")
        || ((lowered_question.contains("what does") || lowered_question.contains("что"))
            && (lowered_question.contains("validate")
                || lowered_question.contains("проверя")
                || lowered_question.contains("валид")))
}

fn matches_primary_heading_request(lowered_question: &str) -> bool {
    lowered_question.contains("what is the title")
        || lowered_question.contains("title of")
        || lowered_question.contains("заголов")
        || lowered_question.contains("название")
}

fn matches_formats_under_test(lowered_question: &str) -> bool {
    (lowered_question.contains("format") || lowered_question.contains("формат"))
        && (lowered_question.contains("under test")
            || lowered_question.contains("listed under test")
            || lowered_question.contains("под тест")
            || lowered_question.contains("перечис"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_endpoint_question_en() {
        let intents = classify_question_intents("What is the URL for the checkout API?");
        assert!(intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn classifies_endpoint_question_ru() {
        let intents = classify_question_intents("Какой адрес эндпоинта оплаты?");
        assert!(intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn classifies_parameter_question() {
        let intents =
            classify_question_intents("What query parameters does the search endpoint accept?");
        assert!(intents.contains(&QuestionIntent::Parameter));
        assert!(intents.contains(&QuestionIntent::Endpoint));
    }

    #[test]
    fn classifies_version_question() {
        let intents = classify_question_intents("What version of PostgreSQL is required?");
        assert!(intents.contains(&QuestionIntent::Version));
    }

    #[test]
    fn classifies_config_question_ru() {
        let intents = classify_question_intents("Какие настройки подключения к базе?");
        assert!(intents.contains(&QuestionIntent::ConfigKey));
    }

    #[test]
    fn empty_on_unrelated_question() {
        let intents = classify_question_intents("Tell me about the company history");
        assert!(intents.is_empty());
    }

    #[test]
    fn classifies_exact_wsdl_lookup() {
        let intents = classify_question_intents("What is the WSDL URL for inventory service?");
        assert_eq!(
            classify_exact_url_lookup("What is the WSDL URL for inventory service?", &intents),
            Some(ExactUrlLookupKind::Wsdl)
        );
    }

    #[test]
    fn blocks_endpoint_lookup_for_transport_questions() {
        assert!(question_blocks_endpoint_lookup(
            "How does REST transport differ from SOAP/WSDL here?"
        ));
        assert!(question_asks_transport_comparison(
            "How does REST transport differ from SOAP/WSDL here?"
        ));
    }

    #[test]
    fn classifies_port_question_without_report_false_positive() {
        let report_intents =
            classify_question_intents("What report name appears in the runtime PDF upload check?");
        assert!(!report_intents.contains(&QuestionIntent::Port));

        let port_intents = classify_question_intents("Which port does the service use?");
        assert!(port_intents.contains(&QuestionIntent::Port));
        assert!(question_mentions_port("Which port does the service use?"));
    }

    #[test]
    fn classifies_focused_secondary_heading_request() {
        assert_eq!(
            classify_focused_document_intent(
                "What report name appears in the runtime PDF upload check?"
            ),
            Some(QuestionIntent::FocusedSecondaryHeading)
        );
    }

    #[test]
    fn classifies_focused_formats_under_test_request() {
        assert_eq!(
            classify_focused_document_intent(
                "Which formats are listed under test in this document?"
            ),
            Some(QuestionIntent::FocusedFormatsUnderTest)
        );
    }
}
