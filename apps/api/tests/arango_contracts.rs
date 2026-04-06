#[test]
fn openapi_mentions_knowledge_surface_without_legacy_projection_words() {
    let contract = include_str!("../contracts/rustrag.openapi.yaml");
    assert!(contract.contains("knowledge"));
    assert!(!contract.contains(concat!("Neo", "4j")));
}
