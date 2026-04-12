use std::collections::HashMap;

use super::*;
use serde_json::json;

use crate::infra::arangodb::{
    document_store::{
        KnowledgeChunkRow, KnowledgeLibraryGenerationRow, KnowledgeStructuredBlockRow,
        KnowledgeTechnicalFactRow,
    },
    graph_store::KnowledgeEvidenceRow,
};
use crate::services::query::{
    planner::{QueryIntentProfile, RuntimeQueryPlan, UnsupportedCapabilityIntent},
    support::RerankOutcome,
};
use crate::shared::extraction::text_render::repair_technical_layout_noise;

#[test]
fn build_references_keeps_chunk_node_edge_order_and_ranks() {
    let references = build_references(
        &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "IronRAG".to_string(),
            node_type: "entity".to_string(),
            score: Some(0.9),
        }],
        &[RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "links".to_string(),
            from_node_id: Uuid::now_v7(),
            from_label: "spec.md".to_string(),
            to_node_id: Uuid::now_v7(),
            to_label: "IronRAG".to_string(),
            score: Some(0.7),
        }],
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: "spec.md".to_string(),
            excerpt: "IronRAG links specs to graph knowledge.".to_string(),
            score: Some(0.8),
            source_text: "IronRAG links specs to graph knowledge.".to_string(),
        }],
        3,
    );

    assert_eq!(references.len(), 3);
    assert_eq!(references[0].kind, "chunk");
    assert_eq!(references[0].rank, 1);
    assert_eq!(references[1].kind, "node");
    assert_eq!(references[1].rank, 2);
    assert_eq!(references[2].kind, "edge");
    assert_eq!(references[2].rank, 3);
}

#[test]
fn grouped_reference_candidates_prefer_document_deduping() {
    let document_id = Uuid::now_v7();
    let candidates = build_grouped_reference_candidates(
        &[],
        &[],
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "spec.md".to_string(),
                excerpt: "First excerpt".to_string(),
                score: Some(0.8),
                source_text: "First excerpt".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "spec.md".to_string(),
                excerpt: "Second excerpt".to_string(),
                score: Some(0.7),
                source_text: "Second excerpt".to_string(),
            },
        ],
        4,
    );

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].dedupe_key, format!("document:{document_id}"));
    assert_eq!(candidates[1].dedupe_key, format!("document:{document_id}"));
}

#[test]
fn assemble_bounded_context_interleaves_graph_and_document_support() {
    let context = assemble_bounded_context(
        &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "IronRAG".to_string(),
            node_type: "entity".to_string(),
            score: Some(0.9),
        }],
        &[RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "uses".to_string(),
            from_node_id: Uuid::now_v7(),
            from_label: "IronRAG".to_string(),
            to_node_id: Uuid::now_v7(),
            to_label: "Arango".to_string(),
            score: Some(0.7),
        }],
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: "spec.md".to_string(),
            excerpt: "IronRAG stores graph knowledge.".to_string(),
            score: Some(0.8),
            source_text: "IronRAG stores graph knowledge.".to_string(),
        }],
        2_000,
    );

    assert!(context.starts_with("Context\n"));
    assert!(context.contains("[document] spec.md: IronRAG stores graph knowledge."));
    assert!(context.contains("[graph-node] IronRAG (entity)"));
    assert!(context.contains("[graph-edge] IronRAG --uses--> Arango"));
    let document_index = context.find("[document]").unwrap_or_default();
    let graph_node_index = context.find("[graph-node]").unwrap_or_default();
    let graph_edge_index = context.find("[graph-edge]").unwrap_or_default();
    assert!(document_index < graph_node_index);
    assert!(graph_node_index < graph_edge_index);
}

#[test]
fn build_answer_prompt_prioritizes_library_context() {
    let prompt = build_answer_prompt(
        "What documents mention IronRAG?",
        "Library summary\n- Documents in library: 12\n\nRecent documents\n- 2026-03-30T22:15:00Z — spec.md (text/markdown; pipeline ready; graph ready)",
        None,
        None,
    );
    assert!(prompt.contains("Treat the active library as the primary source of truth"));
    assert!(prompt.contains("exhaust the provided library context"));
    assert!(prompt.contains("recent document metadata"));
    assert!(prompt.contains("Present the answer directly."));
    assert!(prompt.contains("Do not narrate the retrieval process"));
    assert!(prompt.contains("Do not ask the user to upload"));
    assert!(prompt.contains("Exact technical literals section"));
    assert!(prompt.contains("copy those literals verbatim from Context"));
    assert!(prompt.contains("grouped by document"));
    assert!(prompt.contains("matched excerpt"));
    assert!(prompt.contains("Do not combine parts from different snippets"));
    assert!(prompt.contains("prefer the next distinct item after the excluded one"));
    assert!(prompt.contains("Question: What documents mention IronRAG?"));
    assert!(prompt.contains("Documents in library: 12"));
}

#[test]
fn build_answer_prompt_includes_recent_conversation_history() {
    let prompt = build_answer_prompt(
        "давай",
        "Context\n[dummy] step-by-step instructions",
        Some("User: как в далионе перемещение сделать\nAssistant: Могу расписать пошагово."),
        None,
    );

    assert!(prompt.contains("Use the recent conversation history"));
    assert!(prompt.contains("Recent conversation:"));
    assert!(prompt.contains("Assistant: Могу расписать пошагово."));
    assert!(prompt.contains("Question: давай"));
}

#[test]
fn focused_excerpt_for_prefers_keyword_region_over_chunk_prefix() {
    let content = "\
Header section\n\
Error example creationStatusCode = -1\n\
Unrelated payload\n\
Если при добавлении акции ее код будет совпадать с уже существующей акцией,\n\
то существующая акция будет прервана, а новая добавлена.\n\
Trailing details";

    let excerpt = focused_excerpt_for(
        content,
        &["совпадать".to_string(), "существующей".to_string(), "акцией".to_string()],
        220,
    );

    assert!(excerpt.contains("существующая акция будет прервана"));
    assert!(excerpt.contains("новая добавлена"));
    assert!(!excerpt.starts_with("Header section"));
}

#[test]
fn build_exact_technical_literals_section_extracts_urls_paths_and_parameters() {
    let section = build_exact_technical_literals_section(
            "Какие параметры пейджинации и какой URL используются?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "api.pdf".to_string(),
                excerpt: "Получение списка счетов по страницам.".to_string(),
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http\n://demo.local:8080/rewards-api/rest/v1/accounts\n/bypage\npageNu\nmber\npageSize\nwithCar\nds\nnumber\n_starting",
                ),
            }],
        )
        .unwrap_or_default();

    assert!(section.contains("Document: `api.pdf`"));
    assert!(section.contains("Matched excerpt: Получение списка счетов по страницам."));
    assert!(section.contains("`http://demo.local:8080/rewards-api/rest/v1/accounts/bypage`"));
    assert!(
        section.contains("`/v1/accounts/bypage`")
            || section.contains("`/rewards-api/rest/v1/accounts/bypage`")
    );
    assert!(section.contains("`pageNumber`"));
    assert!(section.contains("`pageSize`"));
    assert!(section.contains("`withCards`"));
    assert!(section.contains("`number_starting`"));
}

#[test]
fn build_exact_technical_literals_section_groups_literals_by_document() {
    let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.9),
                    source_text: repair_technical_layout_noise(
                        "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(0.8),
                    source_text: repair_technical_layout_noise(
                        "http://demo.local:8080/rewards-api/rest/v1/version\n/v1/accounts\nGET",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    let checkout_index =
        section.find("Document: `checkout_server_reference.pdf`").unwrap_or(usize::MAX);
    let rewards_index =
        section.find("Document: `rewards_service_reference.pdf`").unwrap_or(usize::MAX);
    let system_info_index = section.find("`/system/info`").unwrap_or(usize::MAX);
    let accounts_index = section.find("`/v1/accounts`").unwrap_or(usize::MAX);

    assert!(checkout_index < rewards_index);
    assert!(checkout_index < system_info_index);
    assert!(rewards_index < accounts_index);
    assert!(section.contains("текущего статуса checkout server"));
    assert!(section.contains("список счетов rewards service"));
}

#[test]
fn build_exact_technical_literals_section_prefers_question_matched_window_per_document() {
    let section = build_exact_technical_literals_section(
            "Какой endpoint возвращает список счетов rewards service?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                document_label: "rewards_service_reference.pdf".to_string(),
                excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                score: Some(0.9),
                source_text: repair_technical_layout_noise(
                    "http://demo.local:8080/rewards-api/rest/v1/version\nGET\nВерсия rewards service\n/v1/accounts\nGET\nПолучить список счетов rewards service.",
                ),
            }],
        )
        .unwrap_or_default();

    assert!(section.contains("`/v1/accounts`"));
    assert!(!section.contains("`/rewards-api/rest/v1/version`"));
}

#[test]
fn build_exact_technical_literals_section_balances_documents_before_second_same_doc_chunk() {
    let rewards_document_id = Uuid::now_v7();
    let checkout_document_id = Uuid::now_v7();
    let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise("/v1/accounts\nGET\nПолучить список счетов rewards service."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/cards/bypage возвращает список карт rewards service.".to_string(),
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise("/v1/cards/bypage\nGET\nПолучить список карт rewards service."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/cards возвращает список карт.".to_string(),
                    score: Some(0.97),
                    source_text: repair_technical_layout_noise("/v1/cards\nGET\nПолучить список карт."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.6),
                    source_text: repair_technical_layout_noise("http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

    assert!(section.contains("Document: `checkout_server_reference.pdf`"));
    assert!(section.contains("`/system/info`"), "{section}");
}

#[test]
fn build_port_answer_returns_insufficient_when_focused_document_has_no_grounded_port() {
    let control_document_id = Uuid::now_v7();
    let telegram_document_id = Uuid::now_v7();

    let answer = build_port_answer(
            "Какой порт использует Acme Control Center?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: control_document_id,
                    document_label: "Acme Control Center - Example".to_string(),
                    excerpt: "Acme Control Center — программное обеспечение для управления конфигурацией объектов управления.".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise(
                        "Acme Control Center\nОписание\nAcme Control Center — программное обеспечение для управления конфигурацией объектов управления.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: telegram_document_id,
                    document_label: "Acme Telegram Bot - Example".to_string(),
                    excerpt: "Для интеграции используется localhost:2026.".to_string(),
                    score: Some(0.91),
                    source_text: repair_technical_layout_noise(
                        "Acme Telegram Bot\nНастройки\nport: 2026\nlocalhost:2026",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("Acme Control Center"));
    assert!(answer.contains("не подтвержден"));
    assert!(!answer.contains("2026"));
}

#[test]
fn technical_literal_focus_keyword_segments_splits_english_multi_clause_questions() {
    let segments = technical_literal_focus_keyword_segments(
        "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
    );

    assert!(segments.len() >= 2);
    assert!(segments.iter().any(|segment| segment.iter().any(|keyword| keyword == "rewards")));
    assert!(segments.iter().any(|segment| segment.iter().any(|keyword| keyword == "profile")));
}

#[test]
fn build_port_answer_skips_port_plus_protocol_questions() {
    let rewards_document_id = Uuid::now_v7();
    let loyalty_document_id = Uuid::now_v7();

    let answer = build_port_answer(
        "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: rewards_document_id,
                document_label: "rewards_accounts_rest_reference.md".to_string(),
                excerpt: "Default port: 8081".to_string(),
                score: Some(0.99),
                source_text: repair_technical_layout_noise(
                    "Rewards Accounts REST API Reference\nDefault port: 8081\nProtocol: REST over HTTP",
                ),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: loyalty_document_id,
                document_label: "customer_profile_soap_reference.md".to_string(),
                excerpt: "Protocol: SOAP over HTTP".to_string(),
                score: Some(0.98),
                source_text: repair_technical_layout_noise(
                    "Customer Profile SOAP API Reference\nProtocol: SOAP over HTTP",
                ),
            },
        ],
    );

    assert!(answer.is_none());
}

#[test]
fn build_port_and_protocol_answer_handles_english_multi_document_question() {
    let rewards_document_id = Uuid::now_v7();
    let loyalty_document_id = Uuid::now_v7();

    let answer = build_port_and_protocol_answer(
            "What is the default port for the Rewards Accounts REST API, and which protocol does the Customer Profile API use?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "Default port: 8081".to_string(),
                    score: Some(0.99),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nDefault port: 8081\nBase REST URL: http://demo.local:8081/rewards-api/rest",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: loyalty_document_id,
                    document_label: "customer_profile_soap_reference.md".to_string(),
                    excerpt: "Protocol: SOAP over HTTP".to_string(),
                    score: Some(0.98),
                    source_text: repair_technical_layout_noise(
                        "Customer Profile SOAP API Reference\nProtocol: SOAP over HTTP\nWSDL URL: http://demo.local:8080/customer-profile/ws/customer-profile.wsdl",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("8081"), "{answer}");
    assert!(answer.contains("SOAP"), "{answer}");
}

#[test]
fn build_multi_document_endpoint_answer_handles_english_checkout_rewards_question() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();

    let answer = build_multi_document_endpoint_answer_from_chunks(
            "If an agent needs the current Checkout Server status and then the Rewards Accounts list, which two endpoints should it call?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_accounts_rest_reference.md".to_string(),
                    excerpt: "List accounts: GET /v1/accounts".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise(
                        "Rewards Accounts REST API Reference\nList accounts: GET /v1/accounts\nList cards: GET /v1/cards",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_rest_reference.md".to_string(),
                    excerpt: "Health check: GET /health".to_string(),
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API Reference\nHealth check: GET /health",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_rest_reference.md".to_string(),
                    excerpt: "Current server information: GET /system/info".to_string(),
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API Reference\nCurrent server information: GET /system/info\n/system/info returns the current checkout server status and runtime metadata.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("/system/info"), "{answer}");
    assert!(answer.contains("/v1/accounts"), "{answer}");
    assert!(!answer.contains("/health"), "{answer}");
}

#[test]
fn build_exact_technical_literals_section_picks_best_matching_chunk_within_document() {
    let cash_document_id = Uuid::now_v7();
    let section = build_exact_technical_literals_section(
            "Какой endpoint возвращает текущий статус checkout server?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "GET /cashes возвращает список касс.".to_string(),
                    score: Some(0.95),
                    source_text: repair_technical_layout_noise("/cashes\nGET\nПолучить список касс."),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: cash_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.7),
                    source_text: repair_technical_layout_noise("http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info"),
                },
            ],
        )
        .unwrap_or_default();

    assert!(section.contains("system/info"));
    assert!(!section.contains("`/cashes`"));
}

#[test]
fn build_exact_technical_literals_section_prefers_document_local_clause_in_multi_doc_question() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let checkout_list = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: checkout_document_id,
        document_label: "checkout_server_reference.pdf".to_string(),
        excerpt: "GET /cashes возвращает список касс.".to_string(),
        score: Some(0.95),
        source_text: repair_technical_layout_noise("/cashes\nGET\nПолучить список касс."),
    };
    let checkout_system_info = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
            score: Some(0.7),
            source_text: repair_technical_layout_noise(
                "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info",
            ),
        };
    let rewards_bypage = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: rewards_document_id,
        document_label: "rewards_service_reference.pdf".to_string(),
        excerpt: "GET /v1/accounts/bypage возвращает список счетов с пагинацией.".to_string(),
        score: Some(0.95),
        source_text: repair_technical_layout_noise(
            "/v1/accounts/bypage\nGET\npageNumber\npageSize\nПолучить список счетов rewards service.",
        ),
    };
    let rewards_accounts = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: rewards_document_id,
        document_label: "rewards_service_reference.pdf".to_string(),
        excerpt: "GET /v1/accounts возвращает список счетов без параметров пейджинации."
            .to_string(),
        score: Some(0.7),
        source_text: repair_technical_layout_noise(
            "/v1/accounts\nGET\nПолучить список счетов rewards service.",
        ),
    };
    let question = "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?";
    let section = build_exact_technical_literals_section(
        question,
        &[checkout_list, checkout_system_info, rewards_bypage, rewards_accounts],
    )
    .unwrap_or_default();

    assert!(section.contains("Document: `checkout_server_reference.pdf`"));
    assert!(section.contains("Document: `rewards_service_reference.pdf`"));
    assert!(section.contains("`/system/info`"));
    assert!(!section.contains("`/cashes`"));
    assert!(section.contains("`/v1/accounts`"));
    assert!(!section.contains("`/v1/accounts/bypage`"));
}

#[test]
fn build_exact_technical_literals_section_prefers_cash_current_info_clause_over_generic_cash_list()
{
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let checkout_clients = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "GET /checkout-api/rest/dictionaries/clients возвращает список клиентов checkout server.".to_string(),
            score: Some(0.92),
            source_text: repair_technical_layout_noise(
                "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/clients\nПолучение списка клиентов checkout server.",
            ),
        };
    let checkout_system_info = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: checkout_document_id,
            document_label: "checkout_server_reference.pdf".to_string(),
            excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
            score: Some(0.71),
            source_text: repair_technical_layout_noise(
                "http://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nДля получения текущего статуса checkout server.",
            ),
        };
    let rewards_accounts = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: rewards_document_id,
        document_label: "rewards_service_reference.pdf".to_string(),
        excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
        score: Some(0.94),
        source_text: repair_technical_layout_noise(
            "/v1/accounts\nGET\nПолучить список счетов rewards service.",
        ),
    };
    let section = build_exact_technical_literals_section(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[rewards_accounts, checkout_clients, checkout_system_info],
        )
        .unwrap_or_default();

    assert!(section.contains("`/system/info`"));
    assert!(!section.contains("`/checkout-api/rest/dictionaries/clients`"));
    assert!(section.contains("`/v1/accounts`"));
}

#[test]
fn build_multi_document_endpoint_answer_from_chunks_prefers_current_info_for_cash_document() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let answer = build_multi_document_endpoint_answer_from_chunks(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(0.94),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nПолучить список счетов rewards service.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "GET /checkout-api/rest/dictionaries/cardChanged возвращает историю изменений карт checkout server.".to_string(),
                    score: Some(0.96),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nПолучить историю изменений карт checkout server.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Для получения текущего статуса checkout server надо выполнить запрос GET на URL /system/info.".to_string(),
                    score: Some(0.71),
                    source_text: repair_technical_layout_noise(
                        "Публичное API checkout server.\nhttp://demo.local:8080/checkout-api/rest/system/info\nGET\n/system/info\nДля получения текущего статуса checkout server.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("`GET /v1/accounts`"));
    assert!(answer.contains("`GET /system/info`"));
    assert!(!answer.contains("cardChanged"));
}

#[test]
fn build_multi_document_endpoint_answer_from_chunks_handles_live_checkout_server_chunk_layout() {
    let checkout_document_id = Uuid::now_v7();
    let rewards_document_id = Uuid::now_v7();
    let wsdl_document_id = Uuid::now_v7();
    let answer = build_multi_document_endpoint_answer_from_chunks(
            "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rewards_document_id,
                    document_label: "rewards_service_reference.pdf".to_string(),
                    excerpt: "GET /v1/accounts возвращает список счетов rewards service.".to_string(),
                    score: Some(69858.0),
                    source_text: repair_technical_layout_noise(
                        "/v1/accounts\nGET\nПолучить список счетов rewards service.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Получить историю изменений карт checkout server.".to_string(),
                    score: Some(70000.0),
                    source_text: repair_technical_layout_noise(
                        "GET\nhttp://demo.local:8080/checkout-api/rest/dictionaries/cardChanged\nПолучить историю изменений карт checkout server.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: checkout_document_id,
                    document_label: "checkout_server_reference.pdf".to_string(),
                    excerpt: "Публичное API checkout server. Checkout server предоставляет REST-интерфейс для внешних сервисов и приложений.".to_string(),
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Checkout Server REST API\nCheckout server предоставляет REST-интерфейс для внешних сервисов и приложений. Запросы осуществляются через http-протокол, данные передаются json-сериализованными. Префикс для REST-интерфейса checkout server: http://<host>:<port>/checkout-api/rest/<request>\nhttp://demo.local:8080/checkout-api/rest/system/info\nДля получения текущего статуса checkout server надо выполнить запрос типа GET на URL /system/info.\nResult fields include version, buildNumber and buildDate.",
                    ),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: wsdl_document_id,
                    document_label: "customer_profile_service_reference.pdf".to_string(),
                    excerpt: "WSDL customer profile service доступен по префиксу /customer-profile/ws/.".to_string(),
                    score: Some(65000.0),
                    source_text: repair_technical_layout_noise(
                        "Получить WSDL можно через http://demo.local:8080/customer-profile/ws/customer-profile.wsdl. Базовый префикс /customer-profile/ws/.",
                    ),
                },
            ],
        )
        .unwrap_or_default();

    assert!(answer.contains("`GET /v1/accounts`"));
    assert!(answer.contains("`GET /system/info`"));
    assert!(!answer.contains("cardChanged"));
    assert!(!answer.contains("/customer-profile/ws/"));
}

#[test]
fn assemble_answer_context_prefixes_library_summary_and_recent_documents() {
    let summary = RuntimeQueryLibrarySummary {
        document_count: 12,
        graph_ready_count: 8,
        processing_count: 3,
        failed_count: 1,
        graph_status: "partial",
    };
    let recent_documents = vec![RuntimeQueryRecentDocument {
        title: "spec.md".to_string(),
        uploaded_at: "2026-03-30T22:15:00+00:00".to_string(),
        mime_type: Some("text/markdown".to_string()),
        pipeline_state: "ready",
        graph_state: "ready",
        preview_excerpt: Some("IronRAG stores graph knowledge.".to_string()),
    }];

    let retrieved_documents = vec![RuntimeRetrievedDocumentBrief {
        title: "spec.md".to_string(),
        preview_excerpt: "IronRAG stores graph knowledge.".to_string(),
    }];
    let context = assemble_answer_context(
        &summary,
        &recent_documents,
        &retrieved_documents,
        Some("Exact technical literals\n- URLs: `http://demo.local:8080/wsdl`"),
        "Context\n[document] spec.md: IronRAG",
    );

    assert!(context.contains("Context\n[document] spec.md: IronRAG"));
    assert!(context.contains("Library summary\n- Documents in library: 12"));
    assert!(context.contains("- Graph-ready documents: 8"));
    assert!(context.contains("- Documents still processing: 3"));
    assert!(context.contains("- Documents failed in pipeline: 1"));
    assert!(context.contains("- Graph coverage status: partial"));
    assert!(context.contains("Recent documents"));
    assert!(context.contains("2026-03-30T22:15:00+00:00 — spec.md"));
    assert!(context.contains("Preview: IronRAG stores graph knowledge."));
    assert!(context.contains("Retrieved document briefs"));
    assert!(context.contains("Exact technical literals\n- URLs: `http://demo.local:8080/wsdl`"));
}

#[test]
fn build_structured_query_diagnostics_emits_typed_response_shape() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        intent_profile: QueryIntentProfile::default(),
        keywords: vec!["ironrag".to_string(), "graph".to_string()],
        high_level_keywords: vec!["ironrag".to_string()],
        low_level_keywords: vec!["graph".to_string()],
        entity_keywords: vec!["ironrag".to_string()],
        concept_keywords: vec!["graph".to_string()],
        expanded_keywords: vec!["ironrag".to_string(), "graph".to_string()],
        top_k: 8,
        context_budget_chars: 6_000,
        hyde_recommended: false,
    };
    let bundle = RetrievalBundle {
        entities: vec![RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "IronRAG".to_string(),
            node_type: "entity".to_string(),
            score: Some(0.91),
        }],
        relationships: vec![RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "mentions".to_string(),
            from_node_id: Uuid::now_v7(),
            from_label: "spec.md".to_string(),
            to_node_id: Uuid::now_v7(),
            to_label: "IronRAG".to_string(),
            score: Some(0.61),
        }],
        chunks: vec![RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: "spec.md".to_string(),
            excerpt: "IronRAG query runtime returns structured references.".to_string(),
            score: Some(0.73),
            source_text: "IronRAG query runtime returns structured references.".to_string(),
        }],
    };
    let graph_index = QueryGraphIndex { nodes: HashMap::new(), edges: Vec::new() };
    let enrichment = QueryExecutionEnrichment {
        planning: crate::domains::query::QueryPlanningMetadata {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
            keywords: crate::domains::query::IntentKeywords {
                high_level: vec!["ironrag".to_string()],
                low_level: vec!["graph".to_string()],
            },
            warnings: Vec::new(),
        },
        rerank: crate::domains::query::RerankMetadata {
            status: crate::domains::query::RerankStatus::Skipped,
            candidate_count: 3,
            reordered_count: None,
        },
        context_assembly: crate::domains::query::ContextAssemblyMetadata {
            status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
            warning: None,
        },
        grouped_references: Vec::new(),
    };

    let diagnostics = build_structured_query_diagnostics(
        &plan,
        &bundle,
        &graph_index,
        &enrichment,
        true,
        "Bounded context",
    );

    assert_eq!(diagnostics.planned_mode, RuntimeQueryMode::Hybrid);
    assert_eq!(diagnostics.requested_mode, RuntimeQueryMode::Hybrid);
    assert_eq!(diagnostics.reference_counts.entity_count, 1);
    assert_eq!(diagnostics.reference_counts.relationship_count, 1);
    assert_eq!(diagnostics.reference_counts.chunk_count, 1);
    assert_eq!(diagnostics.reference_counts.graph_node_count, 0);
    assert_eq!(diagnostics.reference_counts.graph_edge_count, 0);
    assert_eq!(
        diagnostics.planning.intent_cache_status,
        crate::domains::query::QueryIntentCacheStatus::Miss
    );
    assert_eq!(
        diagnostics.context_assembly.status,
        crate::domains::query::ContextAssemblyStatus::BalancedMixed
    );
    assert!(diagnostics.grouped_references.is_empty());
    assert_eq!(diagnostics.context_text.as_deref(), Some("Bounded context"));
}

#[test]
fn apply_query_execution_warning_sets_typed_fields() {
    let mut diagnostics = RuntimeStructuredQueryDiagnostics {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        keywords: Vec::new(),
        high_level_keywords: Vec::new(),
        low_level_keywords: Vec::new(),
        top_k: 8,
        reference_counts: RuntimeStructuredQueryReferenceCounts {
            entity_count: 0,
            relationship_count: 0,
            chunk_count: 0,
            graph_node_count: 0,
            graph_edge_count: 0,
        },
        planning: crate::domains::query::QueryPlanningMetadata {
            requested_mode: RuntimeQueryMode::Hybrid,
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
            keywords: crate::domains::query::IntentKeywords::default(),
            warnings: Vec::new(),
        },
        rerank: crate::domains::query::RerankMetadata {
            status: crate::domains::query::RerankStatus::Skipped,
            candidate_count: 0,
            reordered_count: None,
        },
        context_assembly: crate::domains::query::ContextAssemblyMetadata {
            status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
            warning: None,
        },
        grouped_references: Vec::new(),
        context_text: None,
        warning: None,
        warning_kind: None,
        library_summary: None,
    };
    apply_query_execution_warning(
        &mut diagnostics,
        Some(&RuntimeQueryWarning {
            warning: "Graph coverage is still converging.".to_string(),
            warning_kind: "partial_convergence",
        }),
    );

    assert_eq!(diagnostics.warning.as_deref(), Some("Graph coverage is still converging."));
    assert_eq!(diagnostics.warning_kind, Some("partial_convergence"));
}

#[test]
fn enrich_query_candidate_summary_overwrites_canonical_reference_counts() {
    let enriched = enrich_query_candidate_summary(
        serde_json::json!({
            "finalChunkReferences": 1,
            "finalEntityReferences": 3,
            "finalRelationReferences": 2
        }),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: vec![
                sample_chunk_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                sample_chunk_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
            ],
            structured_blocks: vec![sample_structured_block_row(
                Uuid::now_v7(),
                Uuid::now_v7(),
                Uuid::now_v7(),
            )],
            technical_facts: vec![
                sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
                sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()),
            ],
        },
    );

    assert_eq!(enriched["finalChunkReferences"], serde_json::json!(2));
    assert_eq!(enriched["finalPreparedSegmentReferences"], serde_json::json!(1));
    assert_eq!(enriched["finalTechnicalFactReferences"], serde_json::json!(2));
    assert_eq!(enriched["finalEntityReferences"], serde_json::json!(3));
}

#[test]
fn enrich_query_assembly_diagnostics_emits_verification_and_graph_participation() {
    let diagnostics = enrich_query_assembly_diagnostics(
        serde_json::json!({
            "bundleId": Uuid::nil(),
        }),
        &RuntimeAnswerVerification {
            state: QueryVerificationState::Verified,
            warnings: vec![QueryVerificationWarning {
                code: "grounded".to_string(),
                message: "Answer is grounded.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
        },
        &serde_json::json!({
            "finalChunkReferences": 2,
            "finalPreparedSegmentReferences": 4,
            "finalTechnicalFactReferences": 3,
            "finalEntityReferences": 5,
            "finalRelationReferences": 2
        }),
    );

    assert_eq!(diagnostics["verificationState"], "verified");
    assert_eq!(diagnostics["verificationWarnings"][0]["code"], "grounded");
    assert_eq!(diagnostics["graphParticipation"]["entityReferenceCount"], 5);
    assert_eq!(diagnostics["graphParticipation"]["relationReferenceCount"], 2);
    assert_eq!(diagnostics["graphParticipation"]["graphBacked"], true);
    assert_eq!(diagnostics["structuredEvidence"]["preparedSegmentReferenceCount"], 4);
    assert_eq!(diagnostics["structuredEvidence"]["technicalFactReferenceCount"], 3);
    assert_eq!(diagnostics["structuredEvidence"]["chunkReferenceCount"], 2);
}

#[test]
fn selected_fact_ids_for_canonical_evidence_stays_bounded() {
    let selected_fact_id = Uuid::now_v7();
    let evidence_fact_id = Uuid::now_v7();
    let evidence_rows = vec![KnowledgeEvidenceRow {
        key: Uuid::now_v7().to_string(),
        arango_id: None,
        arango_rev: None,
        evidence_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_id: None,
        block_id: Some(Uuid::now_v7()),
        fact_id: Some(evidence_fact_id),
        span_start: None,
        span_end: None,
        quote_text: "GET /system/info".to_string(),
        literal_spans_json: json!([]),
        evidence_kind: "relation_fact_support".to_string(),
        extraction_method: "graph_extract".to_string(),
        confidence: Some(0.9),
        evidence_state: "active".to_string(),
        freshness_generation: 1,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }];
    let chunk_supported_facts = (0..40)
        .map(|_| sample_technical_fact_row(Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()))
        .collect::<Vec<_>>();

    let fact_ids = selected_fact_ids_for_canonical_evidence(
        &[selected_fact_id],
        &evidence_rows,
        &chunk_supported_facts,
    );
    assert_eq!(fact_ids.len(), 2);
    assert_eq!(fact_ids[0], selected_fact_id);
    assert_eq!(fact_ids[1], evidence_fact_id);
}

#[test]
fn focused_answer_document_id_prefers_dominant_single_document() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: primary_document_id,
            document_label: "vector_database_wikipedia.md".to_string(),
            excerpt:
                "Vector databases typically implement approximate nearest neighbor algorithms."
                    .to_string(),
            score: Some(1.0),
            source_text:
                "Vector databases typically implement approximate nearest neighbor algorithms."
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: primary_document_id,
            document_label: "vector_database_wikipedia.md".to_string(),
            excerpt: "Use-cases include multi-modal search and recommendation engines.".to_string(),
            score: Some(0.8),
            source_text: "Use-cases include multi-modal search and recommendation engines."
                .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: secondary_document_id,
            document_label: "large_language_model_wikipedia.md".to_string(),
            excerpt: "LLMs generate, summarize, translate, and reason over text.".to_string(),
            score: Some(0.25),
            source_text: "LLMs generate, summarize, translate, and reason over text.".to_string(),
        },
    ];

    assert_eq!(
        focused_answer_document_id(
            "Which algorithms do vector databases typically implement, and name one use case mentioned besides semantic search.",
            &chunks,
        ),
        Some(primary_document_id)
    );
}

#[test]
fn question_mentions_port_does_not_match_report_word() {
    assert!(!question_mentions_port("What report name appears in the runtime PDF upload check?"));
    assert!(question_mentions_port("Which port does the service use?"));
}

#[test]
fn question_requests_multi_document_scope_detects_role_pairing_questions() {
    assert!(question_requests_multi_document_scope(
        "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?"
    ));
    assert!(question_requests_multi_document_scope(
        "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?"
    ));
}

#[test]
fn build_document_literal_answer_extracts_report_name_from_focused_document() {
    let document_id = Uuid::now_v7();
    let answer = build_document_literal_answer(
        "What report name appears in the runtime PDF upload check?",
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "runtime_upload_check.pdf".to_string(),
            excerpt: "Runtime PDF upload check".to_string(),
            score: Some(1.0),
            source_text: "Runtime PDF upload check\n\nQuarterly graph report".to_string(),
        }],
    );
    assert_eq!(answer.as_deref(), Some("Quarterly graph report"));
}

#[test]
fn build_document_literal_answer_extracts_formats_under_test() {
    let document_id = Uuid::now_v7();
    let answer = build_document_literal_answer(
            "Which formats are explicitly listed under test in the PDF smoke fixture?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "upload_smoke_fixture.pdf".to_string(),
                excerpt: "IronRAG PDF smoke fixture".to_string(),
                score: Some(1.0),
                source_text: "IronRAG PDF smoke fixture\n\nExpected formats under test: PDF, DOCX, PPTX, PNG, JPG.".to_string(),
            }],
        );
    assert_eq!(answer.as_deref(), Some("PDF, DOCX, PPTX, PNG, JPG."));
}

#[test]
fn build_document_literal_answer_extracts_vectorized_modalities() {
    let document_id = Uuid::now_v7();
    let answer = build_document_literal_answer(
            "According to the vector database article, what kinds of data can all be vectorized?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "vector_database_wikipedia.md".to_string(),
                excerpt:
                    "Words, phrases, or entire documents, as well as images and audio, can all be vectorized."
                        .to_string(),
                score: Some(1.0),
                source_text:
                    "Words, phrases, or entire documents, as well as images and audio, can all be vectorized."
                        .to_string(),
            }],
        );
    assert_eq!(
        answer.as_deref(),
        Some("Words, phrases, entire documents, images, and audio can all be vectorized.")
    );
}

#[test]
fn build_canonical_answer_context_limits_sections_to_focused_document() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();

    let context = build_canonical_answer_context(
        "Which search engines and assistants or services are named as examples in the knowledge graph article?",
        &RuntimeStructuredQueryResult {
            planned_mode: RuntimeQueryMode::Hybrid,
            embedding_usage: None,
            intent_profile: QueryIntentProfile::default(),
            context_text: String::new(),
            technical_literals_text: None,
            technical_literal_chunks: Vec::new(),
            diagnostics: RuntimeStructuredQueryDiagnostics {
                requested_mode: RuntimeQueryMode::Hybrid,
                planned_mode: RuntimeQueryMode::Hybrid,
                keywords: Vec::new(),
                high_level_keywords: Vec::new(),
                low_level_keywords: Vec::new(),
                top_k: 8,
                reference_counts: RuntimeStructuredQueryReferenceCounts {
                    entity_count: 0,
                    relationship_count: 0,
                    chunk_count: 0,
                    graph_node_count: 0,
                    graph_edge_count: 0,
                },
                planning: crate::domains::query::QueryPlanningMetadata {
                    requested_mode: RuntimeQueryMode::Hybrid,
                    planned_mode: RuntimeQueryMode::Hybrid,
                    intent_cache_status: crate::domains::query::QueryIntentCacheStatus::Miss,
                    keywords: crate::domains::query::IntentKeywords::default(),
                    warnings: Vec::new(),
                },
                rerank: crate::domains::query::RerankMetadata {
                    status: crate::domains::query::RerankStatus::Skipped,
                    candidate_count: 0,
                    reordered_count: None,
                },
                context_assembly: crate::domains::query::ContextAssemblyMetadata {
                    status: crate::domains::query::ContextAssemblyStatus::BalancedMixed,
                    warning: None,
                },
                grouped_references: Vec::new(),
                context_text: None,
                warning: None,
                warning_kind: None,
                library_summary: None,
            },
            retrieved_documents: Vec::new(),
        },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![
                KnowledgeStructuredBlockRow {
                    normalized_text:
                        "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named.".to_string(),
                    text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                        .to_string(),
                    heading_trail: vec!["Examples".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeStructuredBlockRow {
                    normalized_text: "LLMs generate, summarize, translate, and reason over text."
                        .to_string(),
                    text: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                    heading_trail: vec!["Capabilities".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    display_value: "Google".to_string(),
                    canonical_value_text: "Google".to_string(),
                    canonical_value_exact: "Google".to_string(),
                    canonical_value_json: serde_json::json!("Google"),
                    fact_kind: "example".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    display_value: "translate".to_string(),
                    canonical_value_text: "translate".to_string(),
                    canonical_value_exact: "translate".to_string(),
                    canonical_value_json: serde_json::json!("translate"),
                    fact_kind: "capability".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: focused_document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                    .to_string(),
                score: Some(1.0),
                source_text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                    .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: other_document_id,
                document_label: "large_language_model_wikipedia.md".to_string(),
                excerpt: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                score: Some(0.2),
                source_text: "LLMs generate, summarize, translate, and reason over text."
                    .to_string(),
            },
        ],
        "",
        None,
    );

    assert!(context.contains("Focused grounded document\n- knowledge_graph_wikipedia.md"));
    assert!(context.contains("Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa"));
    assert!(!context.contains("LLMs generate, summarize, translate, and reason over text."));
    assert!(!context.contains("capability: `translate`"));
}

#[test]
fn render_canonical_chunk_section_uses_longer_question_focused_source_excerpt() {
    let document_id = Uuid::now_v7();
    let section = render_canonical_chunk_section(
            "Which search engines and assistants or services are named as examples in the knowledge graph article?",
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Google, Bing, and Yahoo are named as examples.".to_string(),
                score: Some(1.0),
                source_text: "Knowledge graphs are used by search engines such as Google, Bing, and Yahoo; knowledge engines and question-answering services such as WolframAlpha, Apple's Siri, and Amazon Alexa."
                    .to_string(),
            }],
            false,
        );

    assert!(section.contains("Google, Bing, and Yahoo"));
    assert!(section.contains("WolframAlpha"));
    assert!(section.contains("Siri"));
    assert!(section.contains("Alexa"));
}

#[test]
fn build_multi_document_role_answer_selects_distinct_corpus_technologies() {
    let vector_document_id = Uuid::now_v7();
    let llm_document_id = Uuid::now_v7();
    let answer = build_multi_document_role_answer(
            "If a system needs semantic similarity search over embeddings and also text generation or reasoning, which two technologies from this corpus fit those roles?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: vector_document_id,
                    document_label: "vector_database_wikipedia.md".to_string(),
                    excerpt: "Vector databases typically implement approximate nearest neighbor algorithms."
                        .to_string(),
                    score: Some(0.9),
                    source_text: "Vector database\n\nA vector database stores and retrieves embeddings of data in vector space. Use-cases include semantic search and retrieval-augmented generation."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: llm_document_id,
                    document_label: "large_language_model_wikipedia.md".to_string(),
                    excerpt:
                        "LLMs are designed for natural language processing tasks, especially language generation."
                            .to_string(),
                    score: Some(0.85),
                    source_text: "Large language model\n\nLLMs are designed for natural language processing tasks, especially language generation. They generate, summarize, translate, and reason over text."
                        .to_string(),
                },
            ],
        )
        .expect("expected deterministic multi-document role answer");

    assert!(answer.contains("Vector database"));
    assert!(answer.contains("Large language model"));
    assert!(!answer.contains("RAG"));
}

#[test]
fn build_multi_document_role_answer_distinguishes_rust_and_llm_roles() {
    let rust_document_id = Uuid::now_v7();
    let llm_document_id = Uuid::now_v7();
    let answer = build_multi_document_role_answer(
            "Which item in this corpus is a programming language focused on memory safety, and which item is a model family used for natural language processing?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: llm_document_id,
                    document_label: "large_language_model_wikipedia.md".to_string(),
                    excerpt: "A large language model is designed for natural language processing tasks."
                        .to_string(),
                    score: Some(0.9),
                    source_text: "Large language model\n\nA large language model is designed for natural language processing tasks."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: rust_document_id,
                    document_label: "rust_programming_language_wikipedia.md".to_string(),
                    excerpt: "Rust is a general-purpose programming language with an emphasis on memory safety."
                        .to_string(),
                    score: Some(0.88),
                    source_text: "Rust (programming language)\n\nRust is a general-purpose programming language with an emphasis on memory safety."
                        .to_string(),
                },
            ],
        )
        .expect("expected deterministic distinction answer");

    assert!(answer.contains("Rust"));
    assert!(answer.contains("Large language model"));
    assert!(!answer.contains("does not contain"));
}

#[test]
fn build_multi_document_role_answer_distinguishes_semantic_web_and_knowledge_graph() {
    let semantic_web_document_id = Uuid::now_v7();
    let knowledge_graph_document_id = Uuid::now_v7();
    let answer = build_multi_document_role_answer(
            "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
            &[
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: semantic_web_document_id,
                    document_label: "semantic_web_wikipedia.md".to_string(),
                    excerpt: "The Semantic Web is an extension of the World Wide Web that enables data to be shared and reused across applications."
                        .to_string(),
                    score: Some(0.92),
                    source_text: "Semantic Web\n\nThe Semantic Web is an extension of the World Wide Web that enables data to be shared and reused across applications. It is based on standards such as RDF and OWL."
                        .to_string(),
                },
                RuntimeMatchedChunk {
                    chunk_id: Uuid::now_v7(),
                    document_id: knowledge_graph_document_id,
                    document_label: "knowledge_graph_wikipedia.md".to_string(),
                    excerpt: "A knowledge graph stores interlinked descriptions of entities and concepts."
                        .to_string(),
                    score: Some(0.9),
                    source_text: "Knowledge graph\n\nA knowledge graph stores interlinked descriptions of entities and concepts."
                        .to_string(),
                },
            ],
        )
        .expect("expected deterministic multi-document role answer");

    assert!(answer.contains("Semantic web"));
    assert!(answer.contains("Knowledge graph"));
}

#[test]
fn extract_multi_document_role_clauses_supports_which_one_stores_questions() {
    let clauses = extract_multi_document_role_clauses(
        "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
    );

    assert_eq!(clauses.len(), 2);
    assert!(clauses[0].contains("machine-readable"));
    assert_eq!(clauses[1], "stores interlinked descriptions of entities and concepts");
}

#[test]
fn verify_answer_accepts_semantic_web_and_knowledge_graph_targets() {
    let verification = verify_answer_against_canonical_evidence(
        "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
        "Semantic web makes Internet data machine-readable through RDF and OWL. Knowledge graph stores interlinked descriptions of entities and concepts.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "wrong_canonical_target"));
}

#[test]
fn verify_answer_accepts_method_path_literal_when_method_and_path_are_grounded() {
    let verification = verify_answer_against_canonical_evidence(
            "Какие endpoint'ы нужны?",
            "Нужен endpoint `GET /system/info`.",
            &QueryIntentProfile {
                exact_literal_technical: true,
                ..QueryIntentProfile::default()
            },
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: vec![KnowledgeChunkRow {
                    key: Uuid::now_v7().to_string(),
                    arango_id: None,
                    arango_rev: None,
                    chunk_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: Some("paragraph".to_string()),
                    content_text: "Для получения текущего статуса checkout server надо выполнить запрос типа GET на URL /system/info".to_string(),
                    normalized_text: "Для получения текущего статуса checkout server надо выполнить запрос типа GET на URL /system/info".to_string(),
                    span_start: Some(0),
                    span_end: Some(80),
                    token_count: Some(12),
                    support_block_ids: vec![],
                    section_path: vec![],
                    heading_trail: vec![],
                    literal_digest: None,
                    chunk_state: "active".to_string(),
                    text_generation: Some(1),
                    vector_generation: Some(1),
                    quality_score: None,
                }],
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
            "",
        );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_ignores_background_conflicts_when_grounded_literals_are_explicit() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_canonical_evidence(
        "Use the exact WSDL URL.",
        "Use `http://demo.local:8080/customer-profile/ws/customer-profile.wsdl`.",
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    canonical_value_exact: "http://demo.local:8080/customer-profile/ws/"
                        .to_string(),
                    canonical_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/"
                    ),
                    display_value: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    canonical_value_text:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    canonical_value_exact:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    canonical_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                    ),
                    display_value:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    conflict_group_id: Some(conflict_group_id),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        &[],
        "",
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.iter().all(|warning| warning.code != "conflicting_evidence"));
}

#[test]
fn verify_unsupported_capability_answer_skips_unrelated_conflict_warnings() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_canonical_evidence(
        "Does the library describe GraphQL?",
        "No, this library does not describe any GraphQL API or GraphQL endpoint.",
        &QueryIntentProfile {
            exact_literal_technical: true,
            unsupported_capability: Some(UnsupportedCapabilityIntent::GraphQlApi),
            ..QueryIntentProfile::default()
        },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    canonical_value_exact: "http://demo.local:8080/customer-profile/ws/"
                        .to_string(),
                    canonical_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/"
                    ),
                    display_value: "http://demo.local:8080/customer-profile/ws/".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    canonical_value_text:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    canonical_value_exact:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    canonical_value_json: serde_json::json!(
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                    ),
                    display_value:
                        "http://demo.local:8080/customer-profile/ws/customer-profile.wsdl"
                            .to_string(),
                    conflict_group_id: Some(conflict_group_id),
                    fact_kind: "url".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        &[],
        "",
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn verify_answer_marks_conflicting_when_exact_literal_question_stays_ambiguous() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let conflict_group_id = format!("url:{}", Uuid::now_v7());
    let verification = verify_answer_against_canonical_evidence(
        "What exact endpoint is described?",
        "The exact endpoint is described in the selected evidence.",
        &QueryIntentProfile { exact_literal_technical: true, ..QueryIntentProfile::default() },
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "/system/info".to_string(),
                    canonical_value_exact: "/system/info".to_string(),
                    canonical_value_json: serde_json::json!("/system/info"),
                    display_value: "/system/info".to_string(),
                    conflict_group_id: Some(conflict_group_id.clone()),
                    fact_kind: "endpoint_path".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
                KnowledgeTechnicalFactRow {
                    canonical_value_text: "/system/status".to_string(),
                    canonical_value_exact: "/system/status".to_string(),
                    canonical_value_json: serde_json::json!("/system/status"),
                    display_value: "/system/status".to_string(),
                    conflict_group_id: Some(conflict_group_id),
                    fact_kind: "endpoint_path".to_string(),
                    ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
                },
            ],
        },
        &[],
        "",
    );

    assert_eq!(verification.state, QueryVerificationState::Conflicting);
    assert!(verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence"));
}

#[test]
fn expanded_candidate_limit_prefers_deeper_combined_mode_search() {
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, true, 24), 24);
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Mix, 10, true, 24), 30);
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Document, 8, true, 24), 8);
    assert_eq!(expanded_candidate_limit(RuntimeQueryMode::Hybrid, 8, false, 24), 24);
}

#[test]
fn technical_literal_candidate_limit_expands_document_recall_for_endpoint_questions() {
    assert_eq!(
        technical_literal_candidate_limit(
            detect_technical_literal_intent("Какие endpoint'ы нужны для двух серверов?"),
            8,
        ),
        32
    );
    assert_eq!(
        technical_literal_candidate_limit(
            detect_technical_literal_intent("Какие параметры пейджинации доступны?"),
            8,
        ),
        24
    );
    assert_eq!(
        technical_literal_candidate_limit(
            detect_technical_literal_intent("Расскажи кратко, о чём библиотека."),
            8,
        ),
        8
    );
}

#[test]
fn build_lexical_queries_keeps_broader_unique_query_set() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Mix,
        planned_mode: RuntimeQueryMode::Mix,
        intent_profile: QueryIntentProfile { exact_literal_technical: true, ..Default::default() },
        keywords: vec![
            "program".to_string(),
            "profile".to_string(),
            "discount".to_string(),
            "tier".to_string(),
        ],
        high_level_keywords: vec!["program".to_string(), "profile".to_string()],
        low_level_keywords: vec!["discount".to_string(), "tier".to_string()],
        entity_keywords: vec![],
        concept_keywords: vec![],
        expanded_keywords: vec![
            "discount".to_string(),
            "profile".to_string(),
            "program".to_string(),
            "tier".to_string(),
        ],
        top_k: 48,
        context_budget_chars: 22_000,
        hyde_recommended: false,
    };

    let question = "Если агенту нужно получить текущий статус checkout server и отдельно список счетов rewards service, какие два endpoint'а ему нужны?";
    let queries = build_lexical_queries(question, &plan);

    assert_eq!(queries[0], "program profile discount tier");
    assert!(queries.contains(&question.to_string()));
    assert!(queries.contains(&"текущий статус checkout server".to_string()));
    assert!(queries.contains(&"список счетов rewards service".to_string()));
    assert!(queries.contains(&"program profile".to_string()));
    assert!(queries.contains(&"discount tier".to_string()));
    assert!(queries.contains(&"program".to_string()));
    assert!(queries.contains(&"profile".to_string()));
}

#[test]
fn build_lexical_queries_expands_canonical_role_targets() {
    let plan = RuntimeQueryPlan {
        requested_mode: RuntimeQueryMode::Hybrid,
        planned_mode: RuntimeQueryMode::Hybrid,
        intent_profile: QueryIntentProfile::default(),
        keywords: Vec::new(),
        high_level_keywords: Vec::new(),
        low_level_keywords: Vec::new(),
        entity_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        expanded_keywords: Vec::new(),
        top_k: 8,
        context_budget_chars: 22_000,
        hyde_recommended: false,
    };

    let queries = build_lexical_queries(
        "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?",
        &plan,
    );

    assert!(queries.contains(&"retrieval-augmented generation".to_string()));
    assert!(queries.contains(&"vector database".to_string()));
}

#[test]
fn verify_answer_rejects_wrong_canonical_targets_for_role_question() {
    let verification = verify_answer_against_canonical_evidence(
        "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?",
        "The two technologies are Information retrieval and Knowledge graph.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert!(verification.warnings.iter().any(|warning| warning.code == "wrong_canonical_target"));
}

#[test]
fn verify_answer_rejects_conflated_semantic_web_and_knowledge_graph_role_question() {
    let verification = verify_answer_against_canonical_evidence(
        "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
        "The technology that focuses on making Internet data machine-readable through standards like RDF and OWL is the Semantic Web. The technology that stores interlinked descriptions of entities and concepts is also the Semantic Web.",
        &QueryIntentProfile::default(),
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        "",
    );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert!(verification.warnings.iter().any(|warning| warning.code == "wrong_canonical_target"));
}

#[test]
fn build_document_literal_answer_extracts_ocr_source_materials() {
    let document_id = Uuid::now_v7();
    let answer = build_document_literal_answer(
            "Which kinds of source material are explicitly listed as OCR inputs in the OCR article?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "optical_character_recognition_wikipedia.md".to_string(),
                excerpt: "machine-encoded text, whether from a scanned document, a photo of a document, a scene photo or from subtitle text.".to_string(),
                score: Some(1.0),
                source_text: "Optical character recognition converts images into machine-encoded text, whether from a scanned document, a photo of a document, a scene photo (for example the text on signs and billboards in a landscape photo) or from subtitle text superimposed on an image.".to_string(),
            }],
        )
        .expect("expected OCR literal answer");

    assert!(answer.contains("scanned document"));
    assert!(answer.contains("photo of a document"));
    assert!(answer.contains("scene photo"));
    assert!(answer.contains("subtitle text"));
    assert!(answer.contains("signs and billboards"));
}

#[test]
fn build_document_literal_answer_extracts_ocr_machine_encoded_text_and_sources() {
    let document_id = Uuid::now_v7();
    let answer = build_document_literal_answer(
            "What does OCR convert images of text into, and what kinds of source material are explicitly named?",
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "optical_character_recognition_wikipedia.md".to_string(),
                excerpt: "machine-encoded text from a scanned document and subtitle text.".to_string(),
                score: Some(1.0),
                source_text: "Optical character recognition converts images of text into machine-encoded text, whether from a scanned document, a photo of a document, a scene photo (for example the text on signs and billboards in a landscape photo) or from subtitle text superimposed on an image.".to_string(),
            }],
        )
        .expect("expected OCR combined answer");

    assert!(answer.contains("machine-encoded text"));
    assert!(answer.contains("scanned document"));
    assert!(answer.contains("photo of a document"));
    assert!(answer.contains("signs and billboards"));
    assert!(answer.contains("subtitle text"));
}

#[test]
fn build_graph_query_language_answer_requires_grounded_standard_literal() {
    let question = "Which technology in this corpus mentions Gremlin, SPARQL, and Cypher, and what standard query language proposal was approved in 2019?";
    let chunks = [RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: "graph_database_wikipedia.md".to_string(),
            excerpt: "Early standardization efforts led to Gremlin, SPARQL, and Cypher."
                .to_string(),
            score: Some(1.0),
            source_text: "Early standardization efforts led to multi-vendor query languages like Gremlin, SPARQL, and Cypher."
                .to_string(),
        }];

    let answer = build_graph_query_language_answer(
        question,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &chunks,
    );

    assert!(answer.is_none());
}

#[test]
fn verify_answer_rejects_unsupported_graph_query_language_claims() {
    let verification = verify_answer_against_canonical_evidence(
            "Which technology in this corpus mentions Gremlin, SPARQL, and Cypher, and what standard query language proposal was approved in 2019?",
            "The technology is the Graph database. The standard query language proposal approved in 2019 was GQL.",
            &QueryIntentProfile::default(),
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: vec![KnowledgeChunkRow {
                    key: Uuid::now_v7().to_string(),
                    arango_id: None,
                    arango_rev: None,
                    chunk_id: Uuid::now_v7(),
                    workspace_id: Uuid::now_v7(),
                    library_id: Uuid::now_v7(),
                    document_id: Uuid::now_v7(),
                    revision_id: Uuid::now_v7(),
                    chunk_index: 0,
                    chunk_kind: Some("paragraph".to_string()),
                    content_text: "Early standardization efforts led to multi-vendor query languages like Gremlin, SPARQL, and Cypher.".to_string(),
                    normalized_text: "Early standardization efforts led to multi-vendor query languages like Gremlin, SPARQL, and Cypher.".to_string(),
                    span_start: Some(0),
                    span_end: Some(90),
                    token_count: Some(12),
                    support_block_ids: vec![],
                    section_path: vec![],
                    heading_trail: vec![],
                    literal_digest: None,
                    chunk_state: "active".to_string(),
                    text_generation: Some(1),
                    vector_generation: Some(1),
                    quality_score: None,
                }],
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &[],
            "",
        );

    assert_eq!(verification.state, QueryVerificationState::InsufficientEvidence);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim")
    );
}

#[test]
fn apply_rerank_outcome_reorders_bundle_before_final_truncation() {
    let entity_a = Uuid::now_v7();
    let entity_b = Uuid::now_v7();
    let chunk_a = Uuid::now_v7();
    let chunk_b = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: vec![
            RuntimeMatchedEntity {
                node_id: entity_a,
                label: "Alpha".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.9),
            },
            RuntimeMatchedEntity {
                node_id: entity_b,
                label: "Budget".to_string(),
                node_type: "entity".to_string(),
                score: Some(0.4),
            },
        ],
        relationships: Vec::new(),
        chunks: vec![
            RuntimeMatchedChunk {
                chunk_id: chunk_a,
                document_id: Uuid::now_v7(),
                document_label: "alpha.md".to_string(),
                excerpt: "Alpha excerpt".to_string(),
                score: Some(0.8),
                source_text: "Alpha excerpt".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: chunk_b,
                document_id: Uuid::now_v7(),
                document_label: "budget.md".to_string(),
                excerpt: "Budget approval memo".to_string(),
                score: Some(0.2),
                source_text: "Budget approval memo".to_string(),
            },
        ],
    };

    apply_rerank_outcome(
        &mut bundle,
        &RerankOutcome {
            entities: vec![entity_b.to_string(), entity_a.to_string()],
            relationships: Vec::new(),
            chunks: vec![chunk_b.to_string(), chunk_a.to_string()],
            metadata: crate::domains::query::RerankMetadata {
                status: crate::domains::query::RerankStatus::Applied,
                candidate_count: 4,
                reordered_count: Some(4),
            },
        },
    );
    truncate_bundle(&mut bundle, 1);

    assert_eq!(bundle.entities[0].node_id, entity_b);
    assert_eq!(bundle.chunks[0].chunk_id, chunk_b);
}

#[test]
fn maps_query_graph_status_from_library_generation() {
    let ready_generation = KnowledgeLibraryGenerationRow {
        key: "ready".to_string(),
        arango_id: None,
        arango_rev: None,
        generation_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        active_text_generation: 3,
        active_vector_generation: 5,
        active_graph_generation: 7,
        degraded_state: "ready".to_string(),
        updated_at: chrono::Utc::now(),
    };
    let degraded_generation = KnowledgeLibraryGenerationRow {
        degraded_state: "degraded".to_string(),
        ..ready_generation.clone()
    };
    let empty_generation = KnowledgeLibraryGenerationRow {
        active_graph_generation: 0,
        degraded_state: "degraded".to_string(),
        ..ready_generation
    };

    assert_eq!(query_graph_status(Some(&degraded_generation)), "partial");
    assert_eq!(query_graph_status(Some(&empty_generation)), "empty");
    assert_eq!(query_graph_status(None), "empty");
}
