use super::*;

fn user_row(role: &str) -> IamUserRow {
    IamUserRow {
        principal_id: Uuid::nil(),
        login: "test-user".to_string(),
        email: "test@example.invalid".to_string(),
        display_name: "Test User".to_string(),
        password_hash: "password-hash-must-not-leak".to_string(),
        auth_provider_kind: "password".to_string(),
        external_subject: None,
        role: role.to_string(),
    }
}

#[test]
fn system_role_wire_values_round_trip_and_reject_unknown_values() {
    for role in [SystemRole::Viewer, SystemRole::Operator, SystemRole::Admin] {
        assert_eq!(SystemRole::parse_wire(role.as_str()), Some(role));
    }
    assert_eq!(SystemRole::parse_wire("not-a-role"), None);
}

#[test]
fn user_row_fails_closed_for_unknown_stored_role() {
    assert_eq!(user_row("admin").system_role(), SystemRole::Admin);
    assert_eq!(user_row("unknown").system_role(), SystemRole::Viewer);
}

#[test]
fn debug_output_redacts_iam_secrets() {
    let timestamp = DateTime::<Utc>::default();
    let session = IamSessionRow {
        id: Uuid::nil(),
        principal_id: Uuid::nil(),
        session_secret_hash: "session-secret-must-not-leak".to_string(),
        issued_at: timestamp,
        expires_at: timestamp,
        revoked_at: None,
        last_seen_at: timestamp,
    };
    let token_secret = IamApiTokenSecretRow {
        token_principal_id: Uuid::nil(),
        secret_version: 1,
        secret_hash: "token-secret-must-not-leak".to_string(),
        issued_at: timestamp,
        revoked_at: None,
    };

    for output in
        [format!("{:?}", user_row("viewer")), format!("{session:?}"), format!("{token_secret:?}")]
    {
        assert!(output.contains("<redacted>"));
        assert!(!output.contains("must-not-leak"));
    }
}
