use super::*;

/// Compute the stable cache-key hash for one compile request. The hash is
/// content-addressed over the exact question/history bytes under the same
/// compiler runtime and resolved QueryCompile binding. Case, whitespace, and
/// formal literal spelling are source-significant and must never alias.
/// Compiler/runtime source files and binding fields are part of the address, so
/// semantic fixes or provider/model changes never serve stale IR rows that were
/// compiled under a different routing contract.
#[must_use]
pub(super) fn hash_compile_request(
    question: &str,
    history: &[CompileHistoryTurn],
    schema_version: u16,
    binding: &ResolvedRuntimeBinding,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"schema|");
    hasher.update(schema_version.to_be_bytes());
    hasher.update(b"|runtime|");
    hasher.update(query_ir_runtime_fingerprint().as_bytes());
    hasher.update(b"|binding|");
    hasher.update(query_compile_binding_fingerprint(binding).as_bytes());
    hash_field(&mut hasher, "question", question);
    hash_field(&mut hasher, "history_len", &history.len().to_string());
    for turn in history {
        hash_field(&mut hasher, "history_role", &turn.role);
        hash_field(&mut hasher, "history_content", &turn.content);
    }
    hex::encode(hasher.finalize())
}

#[must_use]
pub(super) fn query_compile_binding_fingerprint(binding: &ResolvedRuntimeBinding) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, "binding_id", &binding.binding_id.to_string());
    hash_field(&mut hasher, "workspace_id", &binding.workspace_id.to_string());
    hash_field(&mut hasher, "library_id", &binding.library_id.to_string());
    hash_field(&mut hasher, "binding_purpose", binding.binding_purpose.as_str());
    hash_field(&mut hasher, "provider_catalog_id", &binding.provider_catalog_id.to_string());
    hash_field(&mut hasher, "provider_kind", &binding.provider_kind);
    hash_option_field(&mut hasher, "provider_base_url", binding.provider_base_url.as_deref());
    hash_field(&mut hasher, "provider_api_style", &binding.provider_api_style);
    hash_field(&mut hasher, "account_id", &binding.account_id.to_string());
    hash_field(&mut hasher, "model_catalog_id", &binding.model_catalog_id.to_string());
    hash_field(&mut hasher, "model_name", &binding.model_name);
    hash_option_field(&mut hasher, "system_prompt", binding.system_prompt.as_deref());
    hash_optional_display_field(&mut hasher, "temperature", binding.temperature);
    hash_optional_display_field(&mut hasher, "top_p", binding.top_p);
    hash_optional_display_field(
        &mut hasher,
        "max_output_tokens_override",
        binding.max_output_tokens_override,
    );
    hash_json_value(&mut hasher, "extra_parameters_json", &binding.extra_parameters_json);
    hex::encode(hasher.finalize())
}

pub(super) fn hash_field(hasher: &mut Sha256, name: &str, value: &str) {
    hasher.update(name.as_bytes());
    hasher.update(b"=");
    hasher.update(value.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b";");
}

pub(super) fn hash_option_field(hasher: &mut Sha256, name: &str, value: Option<&str>) {
    match value {
        Some(value) => hash_field(hasher, name, value),
        None => hash_field(hasher, name, "<none>"),
    }
}

pub(super) fn hash_optional_display_field<T: ToString>(
    hasher: &mut Sha256,
    name: &str,
    value: Option<T>,
) {
    let value = value.map(|value| value.to_string());
    hash_option_field(hasher, name, value.as_deref());
}

pub(super) fn hash_json_value(hasher: &mut Sha256, name: &str, value: &Value) {
    hasher.update(name.as_bytes());
    hasher.update(b"=");
    hash_json(hasher, value);
    hasher.update(b";");
}

pub(super) fn hash_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update(b"null"),
        Value::Bool(value) => {
            if *value {
                hasher.update(b"true");
            } else {
                hasher.update(b"false");
            }
        }
        Value::Number(value) => hasher.update(value.to_string().as_bytes()),
        Value::String(value) => {
            hasher.update(b"str:");
            hasher.update(value.len().to_string().as_bytes());
            hasher.update(b":");
            hasher.update(value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"[");
            for value in values {
                hash_json(hasher, value);
                hasher.update(b",");
            }
            hasher.update(b"]");
        }
        Value::Object(map) => {
            hasher.update(b"{");
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                hasher.update(key.len().to_string().as_bytes());
                hasher.update(b":");
                hasher.update(key.as_bytes());
                hasher.update(b"=");
                if let Some(value) = map.get(key) {
                    hash_json(hasher, value);
                }
                hasher.update(b",");
            }
            hasher.update(b"}");
        }
    }
}

#[must_use]
pub fn query_ir_runtime_fingerprint() -> &'static str {
    static FINGERPRINT: LazyLock<String> = LazyLock::new(|| {
        let mut hasher = Sha256::new();
        // Every source file that participates in IR compilation must be
        // hashed, or a code change there would silently keep serving stale
        // cached IR. The compiler module is split across these files.
        hasher.update(include_str!("../compiler.rs").as_bytes());
        hasher.update(include_str!("fingerprint.rs").as_bytes());
        hasher.update(include_str!("ir_cache.rs").as_bytes());
        hasher.update(include_str!("provider_free.rs").as_bytes());
        hasher.update(include_str!("tests.rs").as_bytes());
        hasher.update(include_str!("tests/cache.rs").as_bytes());
        hasher.update(include_str!("tests/fallback_and_prompt.rs").as_bytes());
        hasher.update(include_str!("tests/provider_behavior.rs").as_bytes());
        hasher.update(include_str!("../latest_versions.rs").as_bytes());
        hasher.update(include_str!("../../../domains/query_ir.rs").as_bytes());
        hasher.update(include_str!("../../../domains/query_ir/schema.rs").as_bytes());
        hasher.update(include_str!("../../../domains/query_ir/tests.rs").as_bytes());
        hasher.update(include_str!("../../../domains/query_ir/types.rs").as_bytes());
        hasher.update(include_str!("../../../domains/query_ir/validation_tests.rs").as_bytes());
        hex::encode(hasher.finalize())
    });
    FINGERPRINT.as_str()
}
