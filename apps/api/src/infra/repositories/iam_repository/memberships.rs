use super::*;

pub async fn upsert_workspace_membership(
    postgres: &PgPool,
    workspace_id: Uuid,
    principal_id: Uuid,
    membership_state: &str,
) -> Result<IamWorkspaceMembershipRow, sqlx::Error> {
    sqlx::query_as::<_, IamWorkspaceMembershipRow>(
        "insert into iam_workspace_membership (
            workspace_id,
            principal_id,
            membership_state,
            joined_at,
            ended_at
        )
        values ($1, $2, $3::iam_membership_state, now(), null)
        on conflict (workspace_id, principal_id)
        do update set membership_state = excluded.membership_state,
                      ended_at = case when excluded.membership_state = 'ended' then now() else null end
        returning
            workspace_id,
            principal_id,
            membership_state::text as membership_state,
            joined_at,
            ended_at",
    )
    .bind(workspace_id)
    .bind(principal_id)
    .bind(membership_state)
    .fetch_one(postgres)
    .await
}

pub async fn list_workspace_memberships_by_principal(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Vec<IamWorkspaceMembershipRow>, sqlx::Error> {
    sqlx::query_as::<_, IamWorkspaceMembershipRow>(
        "select
            workspace_id,
            principal_id,
            membership_state::text as membership_state,
            joined_at,
            ended_at
         from iam_workspace_membership
         where principal_id = $1
         order by joined_at asc",
    )
    .bind(principal_id)
    .fetch_all(postgres)
    .await
}
