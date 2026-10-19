use super::*;

pub(super) fn validate_ui_agent_tool_allowed(
    tool_name: &str,
    allowed_tool_names: &BTreeSet<String>,
) -> Result<(), String> {
    allowed_tool_names.contains(tool_name).then_some(()).ok_or_else(|| {
        format!("tool '{tool_name}' is not available on the UI assistant answer surface")
    })
}

pub(super) fn validate_agent_tool_library_scope(
    tool_name: &str,
    arguments: &Value,
    library_ref: &str,
) -> Result<(), String> {
    let Value::Object(object) = arguments else {
        return Ok(());
    };
    if tool_uses_single_library_scope(tool_name)
        && let Some(requested) = object.get("library").and_then(Value::as_str)
        && requested != library_ref
    {
        return Err(format!(
            "tool argument library scope mismatch: {tool_name} requested library `{requested}`, but this UI assistant session is scoped to `{library_ref}`"
        ));
    }
    if tool_name == SEARCH_DOCUMENTS_TOOL_NAME
        && let Some(requested_libraries) = object.get("libraries")
    {
        let Some(items) = requested_libraries.as_array() else {
            return Err(format!(
                "tool argument library scope mismatch: {tool_name} `libraries` must be an array scoped to `{library_ref}`"
            ));
        };
        if !items.is_empty()
            && (items.len() != 1 || items.first().and_then(Value::as_str) != Some(library_ref))
        {
            let requested = items
                .iter()
                .map(|item| item.as_str().unwrap_or("<non-string>"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "tool argument library scope mismatch: {tool_name} requested libraries [{requested}], but this UI assistant session is scoped to `{library_ref}`"
            ));
        }
    }
    Ok(())
}

pub(super) fn tool_uses_single_library_scope(tool_name: &str) -> bool {
    matches!(
        tool_name,
        GROUNDED_ANSWER_TOOL_NAME
            | "list_documents"
            | "search_entities"
            | "get_graph_topology"
            | "list_relations"
            | "get_communities"
    )
}
