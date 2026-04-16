use chrono::Utc;
use uuid::Uuid;

use ironrag_backend::services::ingest::structured_preparation::PrepareStructuredRevisionCommand;
use ironrag_backend::shared::extraction::build_text_layout_from_content;

pub fn canonical_prepare_command() -> PrepareStructuredRevisionCommand {
    let text = concat!(
        "# REST API\n\n",
        "Base URL: http://demo.local:8080\n\n",
        "## Authentication\n\n",
        "- login required\n",
        "- token header required\n\n",
        "| Method | Path | Description |\n",
        "| --- | --- | --- |\n",
        "| GET | /v1/accounts | Returns account list |\n",
        "| POST | /v1/accounts | Creates account |\n\n",
        "```json\n",
        "{ \"status\": 200 }\n",
        "```\n\n",
        "GET /v1/accounts?pageNumber=1&withCards=true\n",
    )
    .to_string();

    PrepareStructuredRevisionCommand {
        revision_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        preparation_state: "prepared".to_string(),
        normalization_profile: "technical_layout_repair_v1".to_string(),
        source_format: "pdf".to_string(),
        language_code: Some("en".to_string()),
        source_text: text.clone(),
        normalized_text: text.clone(),
        structure_hints: build_text_layout_from_content(&text).structure_hints,
        typed_fact_count: 0,
        prepared_at: Utc::now(),
    }
}
