use super::UnifiedGateway;

#[test]
fn embedding_request_body_includes_extra_parameters_without_overriding_core_fields() {
    let body = UnifiedGateway::embedding_request_body(
        "text-embedding-3-large",
        serde_json::json!(["alpha", "beta"]),
        &serde_json::json!({
            "dimensions": 1024,
            "encoding_format": "float",
            "model": "ignored",
            "input": "ignored",
            "_providerProfile": {"runtime": {"authScheme": "bearer"}}
        }),
    );

    assert_eq!(
        body.get("model").and_then(serde_json::Value::as_str),
        Some("text-embedding-3-large")
    );
    assert_eq!(body.get("input"), Some(&serde_json::json!(["alpha", "beta"])));
    assert_eq!(body.get("dimensions").and_then(serde_json::Value::as_i64), Some(1024));
    assert_eq!(body.get("encoding_format").and_then(serde_json::Value::as_str), Some("float"));
    assert!(body.get("_providerProfile").is_none());
}

#[test]
fn embedding_vector_parser_rejects_invalid_elements_instead_of_dropping_them() {
    assert_eq!(
        UnifiedGateway::parse_embedding_vector(&serde_json::json!([0.25, -0.5])).unwrap(),
        vec![0.25, -0.5],
    );
    assert!(
        UnifiedGateway::parse_embedding_vector(&serde_json::json!([0.25, "invalid", -0.5]))
            .is_err()
    );
    assert!(UnifiedGateway::parse_embedding_vector(&serde_json::json!([0.25, 1.0e100])).is_err());
    assert!(UnifiedGateway::parse_embedding_vector(&serde_json::json!({})).is_err());
}
