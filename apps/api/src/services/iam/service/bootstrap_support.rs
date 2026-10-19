use super::*;

pub(super) async fn rollback_failed_bootstrap_principal(state: &AppState, principal_id: Uuid) {
    let rollback = async {
        sqlx::query("delete from iam_workspace_membership where principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_grant where principal_id = $1 or granted_by_principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_session where principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query(
            "delete from iam_api_token_secret where token_principal_id in (
                select principal_id from iam_api_token where principal_id = $1 or issued_by_principal_id = $1
            )",
        )
        .bind(principal_id)
        .execute(&state.persistence.postgres)
        .await?;
        sqlx::query("delete from iam_api_token where principal_id = $1 or issued_by_principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_user where principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_principal where id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;

    if let Err(error) = rollback {
        warn!(
            ?error,
            principal_id = %principal_id,
            "failed to roll back incomplete bootstrap principal",
        );
    }
}

pub(super) fn normalize_bootstrap_login(value: &str) -> Result<String, ApiError> {
    let login = value.trim().to_ascii_lowercase();
    if login.is_empty() {
        return Err(ApiError::BadRequest("login must not be empty".into()));
    }
    if login.bytes().any(|byte| !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')) {
        return Err(ApiError::BadRequest(
            "login must contain only lowercase letters, digits, '.', '_' or '-'".into(),
        ));
    }
    Ok(login)
}

pub(super) fn validate_email(value: &str) -> Result<String, ApiError> {
    let email = value.trim();
    if email.len() > 254 || email.contains(char::is_whitespace) || !email.contains('@') {
        return Err(ApiError::BadRequest("email must be a valid address".into()));
    }
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err(ApiError::BadRequest("email must be a valid address".into()));
    };
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(ApiError::BadRequest("email must be a valid address".into()));
    }
    Ok(email.to_string())
}
