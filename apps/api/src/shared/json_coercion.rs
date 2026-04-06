//! Best-effort JSON deserialization with visibility when data is invalid.

use serde::de::DeserializeOwned;
use tracing::warn;

/// Deserializes `value` into `T`; on failure logs a warning and returns `T::default()`.
#[must_use]
pub fn from_value_or_default<T>(context: &'static str, value: &serde_json::Value) -> T
where
    T: DeserializeOwned + Default,
{
    serde_json::from_value::<T>(value.clone()).unwrap_or_else(|error| {
        warn!(context, ?error, "invalid JSON value; using default");
        T::default()
    })
}
