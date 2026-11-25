#[derive(Debug, thiserror::Error)]
#[error("graph evidence store unavailable")]
pub(super) struct GraphStoreUnavailable;

pub(super) fn graph_evidence_read_error(error: sqlx::Error) -> anyhow::Error {
    let operational = match &error {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        sqlx::Error::Database(database) => {
            database.code().is_some_and(|code| code.starts_with("08") || code == "57P03")
        }
        _ => false,
    };
    if operational { GraphStoreUnavailable.into() } else { error.into() }
}

#[cfg(test)]
mod tests {
    use super::{GraphStoreUnavailable, graph_evidence_read_error};

    #[test]
    fn graph_store_only_marks_explicit_operational_database_failures() {
        assert!(
            graph_evidence_read_error(sqlx::Error::PoolTimedOut)
                .downcast_ref::<GraphStoreUnavailable>()
                .is_some()
        );
        assert!(
            graph_evidence_read_error(sqlx::Error::RowNotFound)
                .downcast_ref::<GraphStoreUnavailable>()
                .is_none()
        );
    }
}
