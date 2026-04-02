use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TechnicalFactKind {
    Url,
    EndpointPath,
    HttpMethod,
    Port,
    ParameterName,
    StatusCode,
    Protocol,
    AuthRule,
    Identifier,
}

impl TechnicalFactKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::EndpointPath => "endpoint_path",
            Self::HttpMethod => "http_method",
            Self::Port => "port",
            Self::ParameterName => "parameter_name",
            Self::StatusCode => "status_code",
            Self::Protocol => "protocol",
            Self::AuthRule => "auth_rule",
            Self::Identifier => "identifier",
        }
    }
}

impl std::str::FromStr for TechnicalFactKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "url" => Ok(Self::Url),
            "endpoint_path" => Ok(Self::EndpointPath),
            "http_method" => Ok(Self::HttpMethod),
            "port" => Ok(Self::Port),
            "parameter_name" => Ok(Self::ParameterName),
            "status_code" => Ok(Self::StatusCode),
            "protocol" => Ok(Self::Protocol),
            "auth_rule" => Ok(Self::AuthRule),
            "identifier" => Ok(Self::Identifier),
            other => Err(format!("unsupported technical fact kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "value_type", content = "value")]
pub enum TechnicalFactValue {
    Text(String),
    Integer(i64),
}

impl TechnicalFactValue {
    #[must_use]
    pub fn canonical_string(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Integer(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactQualifier {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactConflict {
    pub conflict_group_id: String,
    pub fact_kind: TechnicalFactKind,
    pub canonical_values: Vec<String>,
    pub fact_ids: Vec<Uuid>,
}

pub fn normalize_technical_fact_value(
    fact_kind: TechnicalFactKind,
    raw_value: &str,
) -> Option<TechnicalFactValue> {
    let raw_value = raw_value.trim();
    if raw_value.is_empty() {
        return None;
    }

    match fact_kind {
        TechnicalFactKind::Url | TechnicalFactKind::EndpointPath => {
            let compact = compact_technical_literal(raw_value);
            (!compact.is_empty()).then_some(TechnicalFactValue::Text(compact))
        }
        TechnicalFactKind::HttpMethod => {
            let normalized = compact_technical_literal(raw_value).to_ascii_uppercase();
            if matches!(
                normalized.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
            ) {
                Some(TechnicalFactValue::Text(normalized))
            } else {
                None
            }
        }
        TechnicalFactKind::Port | TechnicalFactKind::StatusCode => {
            let digits = raw_value.chars().filter(char::is_ascii_digit).collect::<String>();
            digits.parse::<i64>().ok().map(TechnicalFactValue::Integer)
        }
        TechnicalFactKind::Protocol => {
            let normalized = compact_technical_literal(raw_value).to_ascii_lowercase();
            matches!(
                normalized.as_str(),
                "http" | "https" | "tcp" | "udp" | "ws" | "wss" | "grpc" | "soap"
            )
            .then_some(TechnicalFactValue::Text(normalized))
        }
        TechnicalFactKind::ParameterName
        | TechnicalFactKind::AuthRule
        | TechnicalFactKind::Identifier => {
            let normalized = compact_technical_literal(raw_value);
            (!normalized.is_empty()).then_some(TechnicalFactValue::Text(normalized))
        }
    }
}

#[must_use]
pub fn compact_technical_literal(raw_value: &str) -> String {
    raw_value.chars().filter(|ch| !ch.is_whitespace()).collect()
}

#[must_use]
pub fn collapse_literal_whitespace(raw_value: &str) -> String {
    raw_value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{TechnicalFactKind, TechnicalFactValue, normalize_technical_fact_value};

    #[test]
    fn normalizes_http_method_to_uppercase() {
        assert_eq!(
            normalize_technical_fact_value(TechnicalFactKind::HttpMethod, " get "),
            Some(TechnicalFactValue::Text("GET".to_string()))
        );
    }

    #[test]
    fn removes_wrapped_whitespace_from_paths() {
        assert_eq!(
            normalize_technical_fact_value(TechnicalFactKind::EndpointPath, "/api/\n v1/ status "),
            Some(TechnicalFactValue::Text("/api/v1/status".to_string()))
        );
    }
}
