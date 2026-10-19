use super::*;

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct GroundedAnswerArgs {
    pub(super) library: Option<String>,
    pub(super) query: String,
    pub(super) conversation_turns: Option<Vec<GroundedAnswerConversationTurn>>,
    pub(super) top_k: Option<usize>,
    pub(super) include_debug: Option<bool>,
    pub(super) response_profile: Option<McpGroundedAnswerResponseProfile>,
    pub(super) max_references: Option<usize>,
}

pub(super) struct PreparedGroundedAnswerRequest {
    pub(super) library: Option<String>,
    pub(super) query: String,
    pub(super) external_prior_turns: Vec<ExternalConversationTurn>,
    pub(super) top_k: usize,
    pub(super) include_debug: bool,
    pub(super) response_profile: McpGroundedAnswerResponseProfile,
    pub(super) compact_reference_limit: Option<usize>,
}

pub(super) fn prepare_grounded_answer_request(
    arguments: &Value,
) -> Result<PreparedGroundedAnswerRequest, ApiError> {
    let mut parsed: GroundedAnswerArgs =
        serde_json::from_value(arguments.clone()).map_err(|error| {
            ApiError::invalid_mcp_tool_call(format!("invalid grounded_answer arguments: {error}"))
        })?;
    validate_and_normalize_grounded_answer_args(&mut parsed)?;
    let has_contextual_turns =
        parsed.conversation_turns.as_ref().is_some_and(|turns| !turns.is_empty());
    let response_profile =
        resolve_grounded_answer_response_profile(parsed.include_debug, parsed.response_profile);
    let compact_reference_limit =
        resolve_compact_reference_limit(response_profile, parsed.max_references)?;
    let external_prior_turns = normalize_external_prior_turns(parsed.conversation_turns)?;
    Ok(PreparedGroundedAnswerRequest {
        library: parsed.library,
        query: parsed.query,
        external_prior_turns,
        top_k: resolve_grounded_answer_top_k(parsed.top_k, has_contextual_turns),
        include_debug: parsed.include_debug.unwrap_or(false),
        response_profile,
        compact_reference_limit,
    })
}

pub(super) fn validate_and_normalize_grounded_answer_args(
    parsed: &mut GroundedAnswerArgs,
) -> Result<(), ApiError> {
    if let Some(library) = parsed.library.as_mut() {
        *library = library.trim().to_string();
        validate_non_empty_bounded_text("library", library, MCP_GROUNDED_MAX_LIBRARY_CHARS)?;
    }
    parsed.query = parsed.query.trim().to_string();
    validate_non_empty_bounded_text("query", &parsed.query, MCP_GROUNDED_MAX_QUERY_CHARS)?;
    if parsed.top_k.is_some_and(|top_k| !(1..=MAX_TOP_K).contains(&top_k)) {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "topK must be between 1 and {MAX_TOP_K}",
        )));
    }
    if parsed
        .conversation_turns
        .as_ref()
        .is_some_and(|turns| turns.len() > MCP_GROUNDED_MAX_CONVERSATION_TURNS)
    {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "conversationTurns must contain at most {MCP_GROUNDED_MAX_CONVERSATION_TURNS} items",
        )));
    }
    if let Some(turns) = parsed.conversation_turns.as_mut() {
        for (index, turn) in turns.iter_mut().enumerate() {
            turn.content = turn.content.trim().to_string();
            validate_non_empty_bounded_text(
                &format!("conversationTurns[{index}].content"),
                &turn.content,
                MCP_GROUNDED_MAX_CONVERSATION_TURN_CHARS,
            )?;
        }
    }
    if parsed.include_debug == Some(true)
        && parsed.response_profile == Some(McpGroundedAnswerResponseProfile::Compact)
    {
        return Err(ApiError::invalid_mcp_tool_call(
            "includeDebug=true requires responseProfile=full or an omitted responseProfile",
        ));
    }
    Ok(())
}

pub(super) fn validate_non_empty_bounded_text(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), ApiError> {
    let char_count = value.chars().count();
    if char_count == 0 {
        return Err(ApiError::invalid_mcp_tool_call(format!("{field} must not be empty")));
    }
    if char_count > max_chars {
        return Err(ApiError::invalid_mcp_tool_call(format!(
            "{field} must contain at most {max_chars} characters",
        )));
    }
    Ok(())
}

pub(super) fn resolve_compact_reference_limit(
    response_profile: McpGroundedAnswerResponseProfile,
    requested_limit: Option<usize>,
) -> Result<Option<usize>, ApiError> {
    match (response_profile, requested_limit) {
        (McpGroundedAnswerResponseProfile::Full, None) => Ok(None),
        (McpGroundedAnswerResponseProfile::Full, Some(_)) => Err(ApiError::invalid_mcp_tool_call(
            "maxReferences is valid only when responseProfile is compact",
        )),
        (McpGroundedAnswerResponseProfile::Compact, None) => {
            Ok(Some(MCP_COMPACT_DEFAULT_REFERENCES))
        }
        (McpGroundedAnswerResponseProfile::Compact, Some(limit))
            if (1..=agent_policy::AGENT_COMPACT_REFERENCE_LIMIT).contains(&limit) =>
        {
            Ok(Some(limit))
        }
        (McpGroundedAnswerResponseProfile::Compact, Some(_)) => {
            Err(ApiError::invalid_mcp_tool_call(format!(
                "maxReferences must be between 1 and {}",
                agent_policy::AGENT_COMPACT_REFERENCE_LIMIT,
            )))
        }
    }
}

pub(super) fn resolve_grounded_answer_response_profile(
    include_debug: Option<bool>,
    requested: Option<McpGroundedAnswerResponseProfile>,
) -> McpGroundedAnswerResponseProfile {
    if include_debug == Some(true) {
        McpGroundedAnswerResponseProfile::Full
    } else {
        requested.unwrap_or(McpGroundedAnswerResponseProfile::Compact)
    }
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct GroundedAnswerConversationTurn {
    pub(super) role: GroundedAnswerConversationTurnRole,
    pub(super) content: String,
}

#[derive(serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum GroundedAnswerConversationTurnRole {
    User,
    Assistant,
}

pub(super) fn normalize_external_prior_turns(
    turns: Option<Vec<GroundedAnswerConversationTurn>>,
) -> Result<Vec<ExternalConversationTurn>, ApiError> {
    turns
        .unwrap_or_default()
        .into_iter()
        .map(|turn| {
            let content_text = turn.content.trim().to_string();
            if content_text.is_empty() {
                return Err(ApiError::invalid_mcp_tool_call(
                    "invalid grounded_answer arguments: conversationTurns.content must not be empty"
                        .to_string(),
                ));
            }
            let turn_kind = match turn.role {
                GroundedAnswerConversationTurnRole::User => QueryTurnKind::User,
                GroundedAnswerConversationTurnRole::Assistant => QueryTurnKind::Assistant,
            };
            Ok(ExternalConversationTurn { turn_kind, content_text })
        })
        .collect()
}
