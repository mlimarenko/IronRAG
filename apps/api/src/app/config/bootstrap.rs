use super::*;

pub(super) fn read_bootstrap_provider_api_keys_from_env()
-> Result<Vec<UiBootstrapAiProviderSecret>, String> {
    let mut removed_names = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| is_removed_provider_api_key_env_name(name))
        .collect::<Vec<_>>();
    removed_names.sort_unstable();
    if let Some(removed_name) = removed_names.first() {
        return Err(format!(
            "{removed_name} uses the removed provider-specific credential convention; move the exact provider kind and credential into {BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV}"
        ));
    }

    let Some(raw_encoded_map) = std::env::var_os(BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV) else {
        return Ok(Vec::new());
    };
    let mut raw_encoded_map = raw_encoded_map.into_string().map_err(|_| {
        format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must contain valid UTF-8 base64")
    })?;
    let result = parse_bootstrap_provider_api_keys_b64(&raw_encoded_map);
    raw_encoded_map.zeroize();
    result
}

pub(super) fn is_removed_provider_api_key_env_name(env_name: &str) -> bool {
    let Some(provider_fragment) =
        env_name.strip_prefix("IRONRAG_").and_then(|value| value.strip_suffix("_API_KEY"))
    else {
        return false;
    };
    !provider_fragment.is_empty()
        && provider_fragment
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(super) fn parse_bootstrap_provider_api_keys_b64(
    encoded_map: &str,
) -> Result<Vec<UiBootstrapAiProviderSecret>, String> {
    if encoded_map.is_empty() {
        return Ok(Vec::new());
    }
    if encoded_map != encoded_map.trim()
        || encoded_map.len() > MAX_BOOTSTRAP_PROVIDER_MAP_BASE64_BYTES
    {
        return Err(format!(
            "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must be strict standard-base64 JSON within the configured size limit"
        ));
    }
    let mut decoded = BASE64_STANDARD.decode(encoded_map).map_err(|_| {
        format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must be strict standard-base64 JSON")
    })?;
    if decoded.len() > MAX_BOOTSTRAP_PROVIDER_MAP_JSON_BYTES {
        decoded.zeroize();
        return Err(format!(
            "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} decoded JSON exceeds the configured size limit"
        ));
    }
    let mut normalized_encoding = BASE64_STANDARD.encode(&decoded);
    let is_normalized = normalized_encoding == encoded_map;
    normalized_encoding.zeroize();
    if !is_normalized {
        decoded.zeroize();
        return Err(format!(
            "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must be strict standard-base64 JSON"
        ));
    }
    let mut raw_json = match String::from_utf8(decoded) {
        Ok(raw_json) => raw_json,
        Err(error) => {
            drop(zeroize::Zeroizing::new(error.into_bytes()));
            return Err(format!(
                "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must decode to valid UTF-8 JSON"
            ));
        }
    };
    let result = parse_bootstrap_provider_api_keys_json(&raw_json);
    raw_json.zeroize();
    result
}

struct BootstrapProviderSecrets(Vec<UiBootstrapAiProviderSecret>);

impl<'de> Deserialize<'de> for BootstrapProviderSecrets {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(BootstrapProviderSecretsVisitor)
    }
}

struct BootstrapProviderSecretsVisitor;

impl<'de> Visitor<'de> for BootstrapProviderSecretsVisitor {
    type Value = BootstrapProviderSecrets;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object mapping exact provider kinds to API-key strings")
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut secrets = std::collections::BTreeMap::new();
        let mut entry_count = 0_usize;
        while let Some((provider_kind, mut raw_api_key)) = entries.next_entry::<String, String>()? {
            entry_count += 1;
            if entry_count > MAX_BOOTSTRAP_PROVIDER_SECRETS {
                raw_api_key.zeroize();
                return Err(A::Error::custom("provider credential map has too many entries"));
            }
            if let Err(reason) = validate_bootstrap_provider_kind(&provider_kind) {
                raw_api_key.zeroize();
                return Err(A::Error::custom(format!("provider kind {reason}")));
            }
            if raw_api_key.len() > MAX_BOOTSTRAP_PROVIDER_API_KEY_BYTES {
                raw_api_key.zeroize();
                return Err(A::Error::custom("provider credential exceeds the size limit"));
            }
            if raw_api_key.is_empty() {
                continue;
            }
            // JSON and base64 already provide an unambiguous transport. Keep
            // credential bytes exact: whitespace may be significant to an
            // operator-defined provider and must never be normalized here.
            let api_key = std::mem::take(&mut raw_api_key);
            let candidate =
                UiBootstrapAiProviderSecret { provider_kind: provider_kind.clone(), api_key };
            match secrets.entry(provider_kind) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(candidate);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    return Err(A::Error::custom(format!(
                        "duplicate provider kind {}",
                        entry.key()
                    )));
                }
            }
        }
        Ok(BootstrapProviderSecrets(secrets.into_values().collect()))
    }
}

pub(super) fn parse_bootstrap_provider_api_keys_json(
    raw_json: &str,
) -> Result<Vec<UiBootstrapAiProviderSecret>, String> {
    let mut deserializer = serde_json::Deserializer::from_str(raw_json);
    let BootstrapProviderSecrets(secrets) =
        BootstrapProviderSecrets::deserialize(&mut deserializer).map_err(|_| {
            // serde diagnostics can include the unexpected scalar value. Do
            // not propagate them because this document consists of secrets.
            format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} decoded JSON is invalid")
        })?;
    deserializer.end().map_err(|_| {
        format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} decoded JSON is invalid")
    })?;
    Ok(secrets)
}

fn validate_bootstrap_provider_kind(provider_kind: &str) -> Result<(), &'static str> {
    if provider_kind.is_empty() || provider_kind.len() > MAX_BOOTSTRAP_PROVIDER_KIND_BYTES {
        return Err("must contain between 1 and 128 UTF-8 bytes");
    }
    if provider_kind.chars().any(char::is_whitespace) {
        return Err("must not contain whitespace");
    }
    if provider_kind.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    Ok(())
}

pub(super) fn resolved_ui_bootstrap_ai_binding_default(
    binding_purpose: AiBindingPurpose,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
) -> Option<UiBootstrapAiBindingDefault> {
    let provider_kind =
        provider_kind.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase);
    let model_name = model_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    if provider_kind.is_none() && model_name.is_none() {
        return None;
    }
    Some(UiBootstrapAiBindingDefault { binding_purpose, provider_kind, model_name })
}
