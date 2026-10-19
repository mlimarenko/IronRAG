use super::*;

#[test]
fn agent_iteration_keeps_full_mcp_surface_until_final_answer() {
    let tool_defs = [
        "list_workspaces",
        "list_libraries",
        "grounded_answer",
        "search_documents",
        "search_entities",
        "read_document",
    ]
    .into_iter()
    .map(|name| ChatToolDef {
        name: name.to_string(),
        description: String::new(),
        parameters: serde_json::json!({}),
    })
    .collect::<Vec<_>>();
    let successful_tool_names = BTreeSet::new();

    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
    let next_names = next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
    assert_eq!(
        next_names,
        vec![
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document"
        ]
    );

    let mut successful_tool_names = BTreeSet::from(["search_documents".to_string()]);

    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
    let next_names = next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>();
    assert_eq!(
        next_names,
        vec![
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document"
        ]
    );

    successful_tool_names.insert("search_entities".to_string());
    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec![
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document"
        ]
    );

    successful_tool_names.insert("read_document".to_string());
    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, false);
    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec![
            "list_workspaces",
            "list_libraries",
            "grounded_answer",
            "search_documents",
            "search_entities",
            "read_document",
        ]
    );

    assert!(tool_defs_for_agent_iteration(&tool_defs, &successful_tool_names, true).is_empty());
}

#[test]
fn first_iteration_exposes_available_answer_tools() {
    let tool_defs = ["list_workspaces", "list_libraries", "grounded_answer"]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);

    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec!["list_workspaces", "list_libraries", "grounded_answer"]
    );
}

#[test]
fn wildcard_scope_keeps_full_mcp_surface() {
    let tool_defs = ["list_documents", "grounded_answer", "search_documents", "search_entities"]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);
    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
    );

    let listing_only = BTreeSet::from(["list_documents".to_string()]);
    assert!(!should_require_tool_call_before_final(false, &tool_defs, &listing_only, false,));
    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &listing_only, false);
    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
    );

    let grounded = BTreeSet::from([GROUNDED_ANSWER_TOOL_NAME.to_string()]);
    assert!(!should_require_tool_call_before_final(false, &tool_defs, &grounded, false));
}

#[test]
fn atomic_content_question_keeps_full_mcp_surface() {
    let tool_defs = ["list_documents", "grounded_answer", "search_documents", "search_entities"]
        .into_iter()
        .map(|name| ChatToolDef {
            name: name.to_string(),
            description: String::new(),
            parameters: serde_json::json!({}),
        })
        .collect::<Vec<_>>();

    let next_tools = tool_defs_for_agent_iteration(&tool_defs, &BTreeSet::new(), false);

    assert_eq!(
        next_tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        vec!["list_documents", "grounded_answer", "search_documents", "search_entities"]
    );
}
