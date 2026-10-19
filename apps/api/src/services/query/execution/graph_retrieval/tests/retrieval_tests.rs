use super::*;

#[test]
fn entity_retrieval_lane_merge_keeps_lexical_needle_under_vector_score_pressure() {
    let vector_one = node("Noisy Vector One", "concept", None);
    let vector_two = node("Noisy Vector Two", "concept", None);
    let needle = node("Needle Endpoint", "artifact", None);
    let vector_hits = vec![
        RuntimeMatchedEntity {
            node_id: vector_one.id,
            label: vector_one.label,
            node_type: vector_one.node_type,
            summary: None,
            score: Some(9_000.0),
        },
        RuntimeMatchedEntity {
            node_id: vector_two.id,
            label: vector_two.label,
            node_type: vector_two.node_type,
            summary: None,
            score: Some(8_000.0),
        },
    ];
    let lexical_hits = vec![RuntimeMatchedEntity {
        node_id: needle.id,
        label: needle.label,
        node_type: needle.node_type,
        summary: None,
        score: Some(0.24),
    }];

    let merged = merge_entity_retrieval_lanes(vector_hits, lexical_hits, 2);

    assert!(merged.iter().any(|entity| entity.node_id == needle.id));
}

#[test]
fn entity_lane_slice_merge_preserves_narrow_local_ranking() {
    let cross_lane = matched_entity("Cross Lane Anchor");
    let vector_only = matched_entity("Vector Only Anchor");
    let lexical_only = matched_entity("Lexical Only Anchor");
    let vector_hits = vec![cross_lane.clone(), vector_only];
    let lexical_hits = vec![lexical_only.clone(), cross_lane.clone()];

    let sliced = merge_entity_retrieval_lane_slices(&vector_hits, &lexical_hits, 1);
    let broad = merge_entity_retrieval_lanes(vector_hits, lexical_hits, 2);

    assert_eq!(sliced[0].node_id, lexical_only.node_id);
    assert_eq!(broad[0].node_id, cross_lane.node_id);
}

#[test]
fn entity_merge_keeps_query_anchors_before_expansion_scores() {
    let mut first_anchor = matched_entity("Prefix Alpha");
    first_anchor.score = Some(0.01);
    let mut second_anchor = matched_entity("Prefix Beta");
    second_anchor.score = Some(0.01);
    let mut expansion = matched_entity("Broad Expansion");
    expansion.score = Some(100.0);

    let merged =
        merge_primary_then_expanded_entities(vec![first_anchor, second_anchor], vec![expansion], 2);

    let labels = merged.iter().map(|entity| entity.label.as_str()).collect::<Vec<_>>();
    assert_eq!(labels, vec!["Prefix Alpha", "Prefix Beta"]);
}

#[test]
fn relationship_entities_skip_document_nodes() {
    let document = node("Reference Document", "document", None);
    let artifact = node("Primary Artifact", "artifact", None);
    let relationship = RuntimeMatchedRelationship {
        edge_id: Uuid::now_v7(),
        relation_type: "mentions".to_string(),
        from_node_id: document.id,
        from_label: document.label.clone(),
        to_node_id: artifact.id,
        to_label: artifact.label.clone(),
        summary: None,
        support_count: 1,
        score: Some(10.0),
    };
    let graph_index = graph_index_with_projection(vec![document, artifact], Vec::new());

    let entities = entities_from_relationships(&[relationship], &graph_index, 4);

    let labels = entities.iter().map(|entity| entity.label.as_str()).collect::<Vec<_>>();
    assert_eq!(labels, vec!["Primary Artifact"]);
}

#[test]
fn graph_index_iterators_follow_projection_order() {
    let first = node("first node", "process", None);
    let second = node("second node", "artifact", None);
    let first_edge = edge(first.id, second.id, "uses", Some("first edge"));
    let second_edge = edge(second.id, first.id, "mentions", Some("second edge"));
    let graph_index = graph_index_with_projection(
        vec![first.clone(), second.clone()],
        vec![first_edge.clone(), second_edge.clone()],
    );

    let node_labels = graph_index.nodes().map(|node| node.label.as_str()).collect::<Vec<_>>();
    let edge_summaries =
        graph_index.edges().filter_map(|edge| edge.summary.as_deref()).collect::<Vec<_>>();

    assert_eq!(node_labels, vec!["first node", "second node"]);
    assert_eq!(edge_summaries, vec!["first edge", "second edge"]);
}

#[test]
fn lexical_relationship_hits_use_plan_keywords_only() {
    let source = node("source process", "process", None);
    let target = node("target artifact", "artifact", None);
    let keyword_edge = edge(source.id, target.id, "connects", Some("keyword edge"));
    let entity_keyword_edge = edge(source.id, target.id, "routes_to", Some("entity-keyword edge"));
    let graph_index =
        graph_index_with_projection(vec![source, target], vec![keyword_edge, entity_keyword_edge]);
    let plan = RuntimeQueryPlan {
        keywords: vec!["connects".to_string()],
        entity_keywords: vec!["routes".to_string()],
        ..build_query_plan("connect source", None, Some(8), None)
    };
    let ir = describe_ir("routes");
    let relevance_profile = graph_relevance_profile(&plan, Some(&ir));

    let hits = lexical_relationship_hits(&relevance_profile, &graph_index);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].relation_type, "connects");
}

#[test]
fn associative_edges_rank_edge_text_relevance_before_stable_ties() {
    let source = node("source process", "process", None);
    let ordinary_target = node("ordinary artifact", "artifact", None);
    let needle_target = node("needle artifact", "artifact", None);
    let ordinary_edge = edge(source.id, ordinary_target.id, "produces", Some("ordinary output"));
    let needle_edge = edge(source.id, needle_target.id, "produces", Some("needle output"));
    let graph_index = graph_index_with_projection(
        vec![source.clone(), ordinary_target, needle_target],
        vec![ordinary_edge, needle_edge],
    );
    let plan = RuntimeQueryPlan {
        keywords: vec!["needle".to_string()],
        entity_keywords: Vec::new(),
        ..build_query_plan("needle", None, Some(8), None)
    };
    let entities = vec![RuntimeMatchedEntity {
        node_id: source.id,
        label: source.label,
        node_type: source.node_type,
        summary: None,
        score: Some(0.3),
    }];

    let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 2);

    assert_eq!(hits[0].to_label, "needle artifact");
    assert!(score_value(hits[0].score) > score_value(hits[1].score));
}

#[test]
fn associative_edges_preserve_fused_entity_rank_as_seed_strength() {
    let high_seed = node("high ranked seed", "process", None);
    let low_seed = node("low ranked seed", "process", None);
    let high_target = node("high target", "artifact", None);
    let low_target = node("low target", "artifact", None);
    let high_edge = edge(high_seed.id, high_target.id, "links", Some("neutral support"));
    let mut low_edge = edge(low_seed.id, low_target.id, "links", Some("neutral support"));
    low_edge.support_count = 20;
    let graph_index = graph_index_with_projection(
        vec![high_seed.clone(), low_seed.clone(), high_target, low_target],
        vec![high_edge, low_edge],
    );
    let plan = RuntimeQueryPlan {
        keywords: Vec::new(),
        entity_keywords: Vec::new(),
        high_level_keywords: Vec::new(),
        low_level_keywords: Vec::new(),
        concept_keywords: Vec::new(),
        ..build_query_plan("ranked seed expansion", None, Some(8), None)
    };
    let entities = vec![
        RuntimeMatchedEntity {
            node_id: high_seed.id,
            label: high_seed.label,
            node_type: high_seed.node_type,
            summary: None,
            score: Some(0.016_393),
        },
        RuntimeMatchedEntity {
            node_id: low_seed.id,
            label: low_seed.label,
            node_type: low_seed.node_type,
            summary: None,
            score: Some(0.016_129),
        },
    ];

    let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 2);

    assert_eq!(hits[0].to_label, "high target");
}

#[test]
fn associative_seed_score_is_rank_monotonic_and_bounded() {
    let first = associative_seed_score_for_rank(0);
    let second = associative_seed_score_for_rank(1);
    let distant = associative_seed_score_for_rank(99);

    assert!(first <= 2.0);
    assert!(first > second);
    assert!(second > distant);
    assert!(distant > 1.0);
}

#[test]
fn associative_edges_promote_two_hop_bridge_over_one_hop_noise() {
    let source = node("Alpha Relay", "process", None);
    let bridge = node("Bridge Junction", "artifact", None);
    let endpoint = node("Gamma Endpoint", "artifact", None);
    let noise = node("Ordinary Artifact", "artifact", None);
    let noise_edge = edge(source.id, noise.id, "mentions", Some("ordinary output"));
    let bridge_edge = edge(source.id, bridge.id, "connects", Some("Alpha Relay bridge"));
    let endpoint_edge =
        edge(bridge.id, endpoint.id, "routes_to", Some("Bridge reaches Gamma Endpoint"));
    let graph_index = graph_index_with_projection(
        vec![source.clone(), bridge, endpoint, noise],
        vec![noise_edge, bridge_edge, endpoint_edge],
    );
    let plan = RuntimeQueryPlan {
        keywords: vec!["gamma".to_string(), "endpoint".to_string()],
        entity_keywords: Vec::new(),
        ..build_query_plan("which route reaches Gamma Endpoint?", None, Some(8), None)
    };
    let entities = vec![RuntimeMatchedEntity {
        node_id: source.id,
        label: source.label,
        node_type: source.node_type,
        summary: None,
        score: Some(0.3),
    }];

    let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 3);

    assert_eq!(hits[0].to_label, "Gamma Endpoint");
    assert!(hits.iter().any(|hit| hit.to_label == "Ordinary Artifact"));
}

#[test]
fn associative_edges_ignore_document_seed_noise() {
    let router = node("Router Hub", "artifact", None);
    let needle_artifact = node("Needle Artifact", "artifact", None);
    let ordinary_artifact = node("Ordinary Artifact", "artifact", None);
    let noise_artifact = node("Noise Artifact", "artifact", None);
    let source_document = node("random topology snapshot", "document", None);
    let noisy_document = node("reference guide", "document", None);
    let noise_edge = edge(
        source_document.id,
        noise_artifact.id,
        "mentions",
        Some("Document mentions extracted entity"),
    );
    let noisy_edge = edge(
        noisy_document.id,
        needle_artifact.id,
        "mentions",
        Some("Document mentions extracted entity"),
    );
    let guarded_route_edge = edge(
        router.id,
        needle_artifact.id,
        "selects",
        Some("Router Hub selects Needle Artifact through the guarded needle route"),
    );
    let ordinary_edge = edge(
        router.id,
        ordinary_artifact.id,
        "mentions",
        Some("Router Hub mentions Ordinary Artifact through the ordinary noise route"),
    );
    let graph_index = graph_index_with_projection(
        vec![
            router.clone(),
            needle_artifact.clone(),
            ordinary_artifact.clone(),
            noise_artifact.clone(),
            source_document.clone(),
            noisy_document.clone(),
        ],
        vec![noise_edge, noisy_edge, guarded_route_edge, ordinary_edge],
    );
    let plan = RuntimeQueryPlan {
        keywords: vec![
            "Router".into(),
            "Hub".into(),
            "guarded".into(),
            "needle".into(),
            "route".into(),
            "Ordinary".into(),
            "Artifact".into(),
        ],
        entity_keywords: Vec::new(),
        ..build_query_plan("Which route does Router Hub select?", None, Some(8), None)
    };
    let entities = vec![
        RuntimeMatchedEntity {
            node_id: router.id,
            label: router.label,
            node_type: router.node_type,
            summary: None,
            score: Some(0.8),
        },
        RuntimeMatchedEntity {
            node_id: source_document.id,
            label: source_document.label,
            node_type: source_document.node_type,
            summary: None,
            score: Some(12.0),
        },
        RuntimeMatchedEntity {
            node_id: noisy_document.id,
            label: noisy_document.label,
            node_type: noisy_document.node_type,
            summary: None,
            score: Some(11.0),
        },
    ];

    let hits = associative_edges_for_entities(&entities, &graph_index, &plan, None, 3);

    assert!(hits.iter().any(|hit| hit.to_label == "Needle Artifact"));
    assert!(hits.iter().any(|hit| hit.to_label == "Ordinary Artifact"));
    assert!(hits.iter().any(|hit| {
        hit.summary.as_deref().is_some_and(|summary| summary.contains("guarded needle route"))
    }));
}

#[test]
fn associative_edges_return_empty_for_empty_seed_or_limit() {
    let source = node("Alpha Relay", "process", None);
    let target = node("Gamma Endpoint", "artifact", None);
    let edge = edge(source.id, target.id, "routes_to", Some("route"));
    let graph_index = graph_index_with_projection(vec![source.clone(), target], vec![edge]);
    let plan = build_query_plan("Gamma Endpoint", None, Some(8), None);
    let entities = vec![RuntimeMatchedEntity {
        node_id: source.id,
        label: source.label,
        node_type: source.node_type,
        summary: None,
        score: Some(0.3),
    }];

    assert!(associative_edges_for_entities(&[], &graph_index, &plan, None, 3).is_empty());
    assert!(associative_edges_for_entities(&entities, &graph_index, &plan, None, 0).is_empty());
}
