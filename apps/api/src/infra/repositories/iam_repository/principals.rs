use super::*;

pub async fn create_principal(
    postgres: &PgPool,
    principal_kind: &str,
    display_label: &str,
    parent_principal_id: Option<Uuid>,
) -> Result<IamPrincipalRow, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "insert into iam_principal (
            id,
            principal_kind,
            display_label,
            status,
            parent_principal_id,
            created_at,
            disabled_at
        )
        values ($1, $2::iam_principal_kind, $3, 'active', $4, now(), null)
        returning
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at",
    )
    .bind(Uuid::now_v7())
    .bind(principal_kind)
    .bind(display_label)
    .bind(parent_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn create_principal_with_transaction(
    transaction: &mut PgConnection,
    principal_kind: &str,
    display_label: &str,
    parent_principal_id: Option<Uuid>,
) -> Result<IamPrincipalRow, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "insert into iam_principal (
            id,
            principal_kind,
            display_label,
            status,
            parent_principal_id,
            created_at,
            disabled_at
        )
        values ($1, $2::iam_principal_kind, $3, 'active', $4, now(), null)
        returning
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at",
    )
    .bind(Uuid::now_v7())
    .bind(principal_kind)
    .bind(display_label)
    .bind(parent_principal_id)
    .fetch_one(&mut *transaction)
    .await
}

pub async fn get_principal_by_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamPrincipalRow>, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "select
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at
         from iam_principal
         where id = $1",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_principal_profiles_by_ids(
    postgres: &PgPool,
    principal_ids: &[Uuid],
) -> Result<Vec<IamPrincipalProfileRow>, sqlx::Error> {
    if principal_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, IamPrincipalProfileRow>(
        "select
            principal.id,
            principal.principal_kind::text as principal_kind,
            principal.status::text as status,
            principal.display_label,
            iam_user.login,
            iam_user.display_name,
            iam_user.role::text as role
         from iam_principal principal
         left join iam_user
           on iam_user.principal_id = principal.id
         where principal.id = any($1)
         order by principal.display_label asc, principal.id asc",
    )
    .bind(principal_ids)
    .fetch_all(postgres)
    .await
}

pub async fn disable_principal(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamPrincipalRow>, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "update iam_principal
         set status = 'disabled',
             disabled_at = now()
         where id = $1
         returning
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}
