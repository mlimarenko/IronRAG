use super::*;
use std::collections::HashMap;

// -----------------------------------------------------------------
// Two-level cache tests — mirror `StubGateway` with a `StubCache`
// keyed by `(library_id, question_hash)` so we can assert both the
// read-through path (binding-scoped hit skips the LLM) and the
// write-through path (successful compile populates the cache) without
// any real Redis or Postgres.
// -----------------------------------------------------------------

#[derive(Default)]
struct StubCache {
    store: Mutex<HashMap<(Uuid, String), CachedIrEntry>>,
    get_calls: Mutex<u32>,
    put_calls: Mutex<u32>,
}

impl StubCache {
    fn seeded(library_id: Uuid, question_hash: String, entry: CachedIrEntry) -> Self {
        let cache = Self::default();
        cache.store.lock().unwrap().insert((library_id, question_hash), entry);
        cache
    }

    fn len(&self) -> usize {
        self.store.lock().unwrap().len()
    }

    fn put_calls(&self) -> u32 {
        *self.put_calls.lock().unwrap()
    }
}

#[async_trait]
impl QueryIrCache for StubCache {
    async fn get(&self, library_id: Uuid, question_hash: &str) -> Option<CachedIrEntry> {
        *self.get_calls.lock().unwrap() += 1;
        self.store.lock().unwrap().get(&(library_id, question_hash.to_string())).cloned()
    }

    async fn put(&self, library_id: Uuid, question_hash: &str, entry: &CachedIrEntry) {
        *self.put_calls.lock().unwrap() += 1;
        self.store.lock().unwrap().insert((library_id, question_hash.to_string()), entry.clone());
    }
}

#[tokio::test]
async fn cache_hit_short_circuits_llm() {
    let library_id = Uuid::now_v7();
    let question = "how do I configure the payment module?";
    let history: Vec<CompileHistoryTurn> = Vec::new();
    let service = QueryCompilerService;
    let binding = sample_binding();
    let hash = hash_compile_request(question, &history, QUERY_IR_SCHEMA_VERSION, &binding);
    let cached = CachedIrEntry {
        ir: compiled_ir(),
        provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
        model_name: String::new(),
        usage_json: json!({"source": "redis"}),
    };
    let cache = StubCache::seeded(library_id, hash, cached);
    let gateway = StubGateway::new(Err(anyhow::anyhow!("gateway must not be called on cache hit")));

    let outcome = service
        .compile_with_cache_and_gateway(
            &cache,
            &gateway,
            CompileInput { binding: &binding, library_id, question, history: &history },
        )
        .await
        .expect("cache hit is a success path");

    assert_eq!(outcome.provider_kind, CACHE_HIT_REDIS_PROVIDER_KIND);
    assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
    assert!(
        gateway.last_request.lock().unwrap().is_none(),
        "gateway.generate must not be called on cache hit"
    );
    assert_eq!(cache.put_calls(), 0, "cache must not be rewritten on hit");
}

#[tokio::test]
async fn invalid_cached_ir_is_rejected_and_recompiled() {
    let library_id = Uuid::now_v7();
    let question = "current question";
    let history: Vec<CompileHistoryTurn> = Vec::new();
    let binding = sample_binding();
    let hash = hash_compile_request(question, &history, QUERY_IR_SCHEMA_VERSION, &binding);
    let mut invalid_ir = compiled_ir();
    invalid_ir.act = QueryAct::Compare;
    invalid_ir.comparison = None;
    let cache = StubCache::seeded(
        library_id,
        hash,
        CachedIrEntry {
            ir: invalid_ir,
            provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
            model_name: String::new(),
            usage_json: json!({"source": "redis"}),
        },
    );
    let live_ir = compiled_ir();
    let gateway = StubGateway::new(Ok(chat_response_with(
        &serde_json::to_string(&live_ir).expect("serialize live fixture"),
    )));

    let outcome = QueryCompilerService
        .compile_with_cache_and_gateway(
            &cache,
            &gateway,
            CompileInput { binding: &binding, library_id, question, history: &history },
        )
        .await
        .expect("invalid cache entry must fall through to a live compile");

    assert_eq!(outcome.ir, live_ir);
    assert!(!outcome.served_from_cache);
    assert!(gateway.last_request.lock().unwrap().is_some());
    assert_eq!(cache.put_calls(), 1);
}

#[tokio::test]
async fn cache_miss_writes_through() {
    let library_id = Uuid::now_v7();
    let question = "what port does the broker listen on?";
    let history: Vec<CompileHistoryTurn> = Vec::new();
    let ir_json = json!({
        "act": "retrieve_value",
        "scope": "single_document",
        "language": "en",
        "target_types": ["port"],
        "target_entities": [],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.85
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let cache = StubCache::default();
    let service = QueryCompilerService;
    let binding = sample_binding();

    let outcome = service
        .compile_with_cache_and_gateway(
            &cache,
            &gateway,
            CompileInput { binding: &binding, library_id, question, history: &history },
        )
        .await
        .expect("compile ok");

    assert_eq!(outcome.ir.act, QueryAct::RetrieveValue);
    assert_eq!(cache.put_calls(), 1, "successful compile must write through to cache");
    assert_eq!(cache.len(), 1);

    // A second call with the same inputs must now be served from the cache
    // without touching the gateway (the stub gateway is one-shot and
    // would panic on a second invocation).
    let outcome_two = service
        .compile_with_cache_and_gateway(
            &cache,
            &gateway,
            CompileInput { binding: &binding, library_id, question, history: &history },
        )
        .await
        .expect("cache hit");
    assert_eq!(outcome_two.ir.act, QueryAct::RetrieveValue);
}

#[tokio::test]
async fn cache_key_is_scoped_to_resolved_binding() {
    let library_id = Uuid::now_v7();
    let question = "how do I configure the payment module?";
    let history: Vec<CompileHistoryTurn> = Vec::new();
    let binding = sample_binding();
    let mut other_binding = binding.clone();
    other_binding.binding_id = Uuid::now_v7();
    other_binding.model_catalog_id = Uuid::now_v7();
    other_binding.model_name = "gpt-5.4-mini".to_string();

    let hash = hash_compile_request(question, &history, QUERY_IR_SCHEMA_VERSION, &binding);
    let cache = StubCache::seeded(
        library_id,
        hash,
        CachedIrEntry {
            ir: compiled_ir(),
            provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
            model_name: String::new(),
            usage_json: json!({"source": "redis"}),
        },
    );
    let ir_json = json!({
        "act": "retrieve_value",
        "scope": "single_document",
        "language": "en",
        "target_types": ["port"],
        "target_entities": [],
        "literal_constraints": [],
        "temporal_constraints": [],
        "comparison": null,
        "document_focus": null,
        "conversation_refs": [],
        "needs_clarification": null,
        "source_slice": null,
        "confidence": 0.85
    })
    .to_string();
    let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
    let service = QueryCompilerService;

    let outcome = service
        .compile_with_cache_and_gateway(
            &cache,
            &gateway,
            CompileInput { binding: &other_binding, library_id, question, history: &history },
        )
        .await
        .expect("binding-scoped cache miss should compile live");

    assert_eq!(outcome.ir.act, QueryAct::RetrieveValue);
    assert_eq!(
        gateway.last_request.lock().unwrap().as_ref().map(|request| request.model_name.as_str()),
        Some("gpt-5.4-mini")
    );
    assert_eq!(cache.put_calls(), 1);
}

#[tokio::test]
async fn provider_failure_is_not_cached() {
    let library_id = Uuid::now_v7();
    let question = "anything";
    let history: Vec<CompileHistoryTurn> = Vec::new();
    let gateway = StubGateway::new(Err(anyhow::anyhow!("upstream 503")));
    let cache = StubCache::default();
    let service = QueryCompilerService;
    let binding = sample_binding();

    let error = service
        .compile_with_cache_and_gateway(
            &cache,
            &gateway,
            CompileInput { binding: &binding, library_id, question, history: &history },
        )
        .await
        .expect_err("provider failure must fail the compile");

    assert!(matches!(error, ApiError::ProviderFailure(_)));
    assert_eq!(cache.put_calls(), 0, "failed compiles must not be cached");
    assert_eq!(cache.len(), 0);
}

#[tokio::test]
async fn hash_compile_request_preserves_exact_question_history_and_binding_bytes() {
    let binding = sample_binding();
    let base = hash_compile_request("Hello World", &[], QUERY_IR_SCHEMA_VERSION, &binding);
    let case_variant = hash_compile_request("hello world", &[], QUERY_IR_SCHEMA_VERSION, &binding);
    let whitespace_variant =
        hash_compile_request(" Hello World ", &[], QUERY_IR_SCHEMA_VERSION, &binding);
    let formal_literal_variant =
        hash_compile_request("Hello World `A_B`", &[], QUERY_IR_SCHEMA_VERSION, &binding);
    assert_ne!(base, case_variant, "question case is source-significant");
    assert_ne!(base, whitespace_variant, "question whitespace is source-significant");
    assert_ne!(base, formal_literal_variant, "formal literals are source-significant");

    let with_history = hash_compile_request(
        "Hello World",
        &[CompileHistoryTurn { role: "user".to_string(), content: "prior context".to_string() }],
        QUERY_IR_SCHEMA_VERSION,
        &binding,
    );
    assert_ne!(base, with_history, "history must contribute to the hash");

    let history_case_variant = hash_compile_request(
        "Hello World",
        &[CompileHistoryTurn { role: "user".to_string(), content: "Prior context".to_string() }],
        QUERY_IR_SCHEMA_VERSION,
        &binding,
    );
    let history_role_variant = hash_compile_request(
        "Hello World",
        &[CompileHistoryTurn {
            role: "assistant".to_string(),
            content: "prior context".to_string(),
        }],
        QUERY_IR_SCHEMA_VERSION,
        &binding,
    );
    assert_ne!(with_history, history_case_variant, "history content bytes are exact");
    assert_ne!(with_history, history_role_variant, "history role bytes are exact");

    let bumped =
        hash_compile_request("Hello World", &[], QUERY_IR_SCHEMA_VERSION.wrapping_add(1), &binding);
    assert_ne!(base, bumped, "schema_version must contribute to the hash");

    let mut other_binding = binding;
    other_binding.model_name = "gpt-5.4-mini".to_string();
    let other_binding_hash =
        hash_compile_request("Hello World", &[], QUERY_IR_SCHEMA_VERSION, &other_binding);
    assert_ne!(base, other_binding_hash, "binding fingerprint must contribute to the hash");

    assert_eq!(
        query_ir_runtime_fingerprint().len(),
        64,
        "runtime fingerprint is a SHA-256 hex digest"
    );
}
