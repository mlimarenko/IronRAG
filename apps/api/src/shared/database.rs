/// Well-known PostgreSQL database error classes used at service boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseErrorClass {
    UniqueViolation,
    ForeignKeyViolation,
    Contention,
    Other,
}

/// Classifies the SQLSTATE carried by an SQLx database error.
#[must_use]
pub fn classify_error(error: &sqlx::Error) -> DatabaseErrorClass {
    let Some(code) = error.as_database_error().and_then(sqlx::error::DatabaseError::code) else {
        return DatabaseErrorClass::Other;
    };

    match code.as_ref() {
        "23505" => DatabaseErrorClass::UniqueViolation,
        "23503" => DatabaseErrorClass::ForeignKeyViolation,
        "40001" | "40P01" | "55P03" => DatabaseErrorClass::Contention,
        _ => DatabaseErrorClass::Other,
    }
}

/// Returns whether SQLx classified an error as a database unique-key violation.
#[must_use]
pub fn error_is_unique_violation(error: &sqlx::Error) -> bool {
    classify_error(error) == DatabaseErrorClass::UniqueViolation
}

#[cfg(test)]
pub(crate) fn test_database_error(
    code: &'static str,
    constraint: Option<&'static str>,
) -> sqlx::Error {
    use std::{borrow::Cow, error::Error as StdError, fmt};

    use sqlx::error::{DatabaseError, ErrorKind};

    #[derive(Debug)]
    struct TestDatabaseError {
        code: &'static str,
        constraint: Option<&'static str>,
    }

    impl fmt::Display for TestDatabaseError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "synthetic database error ({})", self.code)
        }
    }

    impl StdError for TestDatabaseError {}

    impl DatabaseError for TestDatabaseError {
        fn constraint(&self) -> Option<&str> {
            self.constraint
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(self.code.into())
        }

        fn message(&self) -> &str {
            "synthetic database error"
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }
    }

    sqlx::Error::Database(Box::new(TestDatabaseError { code, constraint }))
}

#[cfg(test)]
mod tests {
    use super::{
        DatabaseErrorClass, classify_error, error_is_unique_violation, test_database_error,
    };

    #[test]
    fn classifies_well_known_sqlstates() {
        for (code, expected) in [
            ("23505", DatabaseErrorClass::UniqueViolation),
            ("23503", DatabaseErrorClass::ForeignKeyViolation),
            ("40001", DatabaseErrorClass::Contention),
            ("40P01", DatabaseErrorClass::Contention),
            ("55P03", DatabaseErrorClass::Contention),
            ("22000", DatabaseErrorClass::Other),
        ] {
            assert_eq!(
                classify_error(&test_database_error(code, None)),
                expected,
                "SQLSTATE {code}"
            );
        }
        assert_eq!(classify_error(&sqlx::Error::RowNotFound), DatabaseErrorClass::Other);
    }

    #[test]
    fn unique_predicate_uses_the_shared_classification() {
        assert!(error_is_unique_violation(&test_database_error("23505", None)));
        assert!(!error_is_unique_violation(&test_database_error("23503", None)));
    }
}
