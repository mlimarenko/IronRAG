/// HMAC-SHA256 webhook signature scheme.
///
/// Header format:  `X-Ironrag-Signature: t=<unix_ts>,v1=<hex_hmac>`
/// HMAC input:     `<unix_ts>.<raw_body_bytes>`
/// Replay window:  configurable, default 300 seconds (5 minutes).
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

const HEADER_NAME: &str = "X-Ironrag-Signature";

/// Compute `X-Ironrag-Signature` header value for an outbound request.
///
/// # Errors
/// Returns an error string if `unix_ts` cannot be formatted or HMAC key is
/// invalid (should never happen in practice).
pub fn sign(secret: &[u8], unix_ts: u64, body: &[u8]) -> String {
    let msg = build_signed_input(unix_ts, body);
    // new_from_slice only fails when the slice length exceeds the max key size,
    // which cannot happen for HMAC (accepts any length).
    #[allow(clippy::expect_used)]
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(&msg);
    let result = mac.finalize().into_bytes();
    format!("t={unix_ts},v1={}", hex::encode(result))
}

/// Verify an `X-Ironrag-Signature` header value against the raw request body.
///
/// Returns `Ok(())` on success, or an `Err(&'static str)` describing the
/// reason for rejection (suitable for logging but intentionally generic to
/// avoid oracle attacks).
///
/// # Errors
/// Returns a description of the first failing check.
pub fn verify(
    secret: &[u8],
    header_value: &str,
    body: &[u8],
    replay_window_seconds: u64,
) -> Result<(), &'static str> {
    let (ts, v1_hex) = parse_header(header_value)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let age = now.saturating_sub(ts);
    if age > replay_window_seconds {
        return Err("signature timestamp outside replay window");
    }

    let msg = build_signed_input(ts, body);
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| "invalid HMAC key length")?;
    mac.update(&msg);
    let expected = mac.finalize().into_bytes();

    let provided = hex::decode(v1_hex).map_err(|_| "v1 is not valid hex")?;

    if expected.as_slice().ct_eq(provided.as_slice()).into() {
        Ok(())
    } else {
        Err("signature mismatch")
    }
}

/// Parse `t=<ts>,v1=<hex>` into `(ts, hex_str)`.
fn parse_header(header: &str) -> Result<(u64, &str), &'static str> {
    let mut ts: Option<u64> = None;
    let mut v1: Option<&str> = None;

    for part in header.split(',') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("t=") {
            ts = val.parse::<u64>().ok();
        } else if let Some(val) = part.strip_prefix("v1=") {
            v1 = Some(val);
        }
    }

    match (ts, v1) {
        (Some(t), Some(v)) => Ok((t, v)),
        (None, _) => Err("missing t= field in signature header"),
        (_, None) => Err("missing v1= field in signature header"),
    }
}

fn build_signed_input(unix_ts: u64, body: &[u8]) -> Vec<u8> {
    let ts_str = unix_ts.to_string();
    let mut msg = Vec::with_capacity(ts_str.len() + 1 + body.len());
    msg.extend_from_slice(ts_str.as_bytes());
    msg.push(b'.');
    msg.extend_from_slice(body);
    msg
}

/// Name of the HTTP header carrying the webhook signature.
#[must_use]
pub fn header_name() -> &'static str {
    HEADER_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret-key";
    const BODY: &[u8] = b"{\"event_type\":\"document.updated\"}";

    #[test]
    fn sign_then_verify_succeeds() {
        let ts: u64 = 1_700_000_000;
        let header = sign(SECRET, ts, BODY);
        assert!(header.starts_with("t="));
        assert!(header.contains(",v1="));
        verify(SECRET, &header, BODY, 999_999_999).expect("verify should pass");
    }

    #[test]
    fn wrong_body_fails() {
        let ts: u64 = 1_700_000_000;
        let header = sign(SECRET, ts, BODY);
        let result = verify(SECRET, &header, b"tampered", 999_999_999);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_secret_fails() {
        let ts: u64 = 1_700_000_000;
        let header = sign(SECRET, ts, BODY);
        let result = verify(b"wrong-secret", &header, BODY, 999_999_999);
        assert!(result.is_err());
    }

    #[test]
    fn expired_timestamp_rejected() {
        // Use a ts well in the past (epoch + 1) and a window of 0.
        let ts: u64 = 1;
        let header = sign(SECRET, ts, BODY);
        let result = verify(SECRET, &header, BODY, 0);
        assert!(result.is_err());
    }

    #[test]
    fn malformed_header_rejected() {
        let result = verify(SECRET, "not-a-valid-header", BODY, 999_999_999);
        assert!(result.is_err());
    }

    #[test]
    fn missing_v1_field_rejected() {
        let result = verify(SECRET, "t=1700000000", BODY, 999_999_999);
        assert!(result.is_err());
    }
}
