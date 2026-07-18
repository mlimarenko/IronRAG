const PG_SQLSTATE_UNDEFINED_TABLE: &str = "42P01";

/// Returns `true` when PostgreSQL reports that the target vector relation does
/// not exist. Rendered error text is deliberately ignored.
pub(super) fn is_vector_relation_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
            .and_then(sqlx::error::DatabaseError::code)
            .is_some_and(|code| is_vector_relation_not_found_sqlstate(code.as_ref()))
    })
}

fn is_vector_relation_not_found_sqlstate(code: &str) -> bool {
    code == PG_SQLSTATE_UNDEFINED_TABLE
}

#[cfg(test)]
mod tests {
    use super::{is_vector_relation_not_found, is_vector_relation_not_found_sqlstate};

    #[test]
    fn ignores_rendered_undefined_table_prose_without_a_typed_database_error() {
        assert!(is_vector_relation_not_found_sqlstate("42P01"));
        assert!(!is_vector_relation_not_found_sqlstate("23505"));

        let rendered_only = anyhow::anyhow!("42P01: relation does not exist");

        assert!(!is_vector_relation_not_found(&rendered_only));
    }
}
