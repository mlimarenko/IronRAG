use anyhow::Context;
use serde::de::DeserializeOwned;

pub(super) fn decode_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let mut values = decode_many_results::<T>(cursor)?;
    values.pop().context("expected at least one result row from arangodb query")
}

pub(super) fn decode_optional_single_result<T>(
    cursor: serde_json::Value,
) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let mut values = decode_many_results::<T>(cursor)?;
    Ok(values.pop())
}

pub(super) fn decode_many_results<T>(cursor: serde_json::Value) -> anyhow::Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let result =
        cursor.get("result").cloned().context("arangodb cursor payload missing result field")?;
    serde_json::from_value(result).context("deserialize arangodb cursor rows")
}
