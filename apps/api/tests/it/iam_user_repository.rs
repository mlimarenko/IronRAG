use crate::temp_postgres_database;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use ironrag_backend::infra::repositories::iam_repository::{
    self, DeleteUserOutcome, OidcProvisionUserOutcome, SetUserRoleOutcome, SystemRole,
};
use temp_postgres_database::TempPostgresDatabase;

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn user_role_mutations_preserve_the_last_active_administrator() -> Result<()> {
    let database_url =
        std::env::var("IRONRAG_DATABASE_URL").context("IRONRAG_DATABASE_URL must be configured")?;
    let database = TempPostgresDatabase::create(&database_url, "iam_user_repository").await?;
    let postgres = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database.database_url)
        .await
        .context("failed to connect IAM user repository postgres")?;
    sqlx::migrate!("./migrations")
        .run(&postgres)
        .await
        .context("failed to apply IAM user repository migrations")?;

    let result = async {
        let first_admin = iam_repository::create_user(
            &postgres,
            "admin-alpha",
            "admin-alpha@example.invalid",
            "Admin Alpha",
            "synthetic-password-hash",
            SystemRole::Admin,
        )
        .await?;
        let last_admin = iam_repository::create_user(
            &postgres,
            "admin-beta",
            "admin-beta@example.invalid",
            "Admin Beta",
            "synthetic-password-hash",
            SystemRole::Admin,
        )
        .await?;
        assert_eq!(iam_repository::count_admin_users(&postgres).await?, 2);

        match iam_repository::set_user_role(&postgres, first_admin.principal_id, SystemRole::Viewer)
            .await?
        {
            SetUserRoleOutcome::Updated(user) => assert_eq!(user.role, "viewer"),
            SetUserRoleOutcome::NotFound | SetUserRoleOutcome::WouldDemoteLastAdmin => {
                anyhow::bail!("non-final administrator demotion was rejected")
            }
        }
        assert_eq!(iam_repository::count_admin_users(&postgres).await?, 1);

        assert!(matches!(
            iam_repository::set_user_role(&postgres, Uuid::now_v7(), SystemRole::Operator).await?,
            SetUserRoleOutcome::NotFound
        ));
        assert!(matches!(
            iam_repository::set_user_role(
                &postgres,
                last_admin.principal_id,
                SystemRole::Operator,
            )
            .await?,
            SetUserRoleOutcome::WouldDemoteLastAdmin
        ));
        assert!(matches!(
            iam_repository::delete_user(&postgres, last_admin.principal_id).await?,
            DeleteUserOutcome::WouldRemoveLastAdmin
        ));

        match iam_repository::delete_user(&postgres, first_admin.principal_id).await? {
            DeleteUserOutcome::Deleted(user) => assert_eq!(user.role, "viewer"),
            DeleteUserOutcome::NotFound | DeleteUserOutcome::WouldRemoveLastAdmin => {
                anyhow::bail!("non-administrator user deletion was rejected")
            }
        }
        assert!(matches!(
            iam_repository::delete_user(&postgres, Uuid::now_v7()).await?,
            DeleteUserOutcome::NotFound
        ));
        Ok(())
    }
    .await;

    postgres.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn deleting_an_oidc_user_disables_the_identity_instead_of_letting_it_be_reissued()
-> Result<()> {
    let database_url =
        std::env::var("IRONRAG_DATABASE_URL").context("IRONRAG_DATABASE_URL must be configured")?;
    let database =
        TempPostgresDatabase::create(&database_url, "iam_user_repository_oidc_delete").await?;
    let postgres = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database.database_url)
        .await
        .context("failed to connect IAM user repository postgres")?;
    sqlx::migrate!("./migrations")
        .run(&postgres)
        .await
        .context("failed to apply IAM user repository migrations")?;

    let result = async {
        // Second admin so deleting the first is not blocked by the
        // last-admin guard.
        iam_repository::create_user(
            &postgres,
            "admin-keeper",
            "admin-keeper@example.invalid",
            "Admin Keeper",
            "synthetic-password-hash",
            SystemRole::Admin,
        )
        .await?;

        let provisioned = match iam_repository::provision_oidc_user(&postgres, &iam_repository::NewOidcUser {
    auth_provider_kind: "oidc",
    external_subject: "subject-revoked-001",
    login: "sso-revoked",
    email: "sso-revoked@example.invalid",
    display_name: "SSO Revoked",
    password_hash: "synthetic-password-hash",
    role: SystemRole::Admin,
})
        .await?
        {
            OidcProvisionUserOutcome::Provisioned { user, created } => {
                assert!(created, "first provisioning attempt must create the identity");
                user
            }
            OidcProvisionUserOutcome::IdentityCollision => {
                anyhow::bail!("unexpected collision provisioning the first SSO user")
            }
        };
        let principal_id = provisioned.principal_id;

        match iam_repository::delete_user(&postgres, principal_id).await? {
            DeleteUserOutcome::Deleted(user) => assert_eq!(user.principal_id, principal_id),
            DeleteUserOutcome::NotFound | DeleteUserOutcome::WouldRemoveLastAdmin => {
                anyhow::bail!("deleting the SSO-provisioned user was rejected")
            }
        }

        let disabled_principal = iam_repository::get_principal_by_id(&postgres, principal_id)
            .await?
            .context("disabled principal must still resolve by id")?;
        assert_eq!(disabled_principal.status, "disabled");

        assert!(
            !iam_repository::list_users(&postgres)
                .await?
                .iter()
                .any(|user| user.principal_id == principal_id),
            "a disabled user must not reappear in the active user listing"
        );

        // The defect this guards against: re-provisioning the same external
        // subject must resolve to the same, now-disabled principal rather
        // than falling through to identity creation because the lookup in
        // `provision_oidc_user` can no longer find a hard-deleted row.
        match iam_repository::provision_oidc_user(&postgres, &iam_repository::NewOidcUser {
    auth_provider_kind: "oidc",
    external_subject: "subject-revoked-001",
    login: "sso-revoked",
    email: "sso-revoked@example.invalid",
    display_name: "SSO Revoked",
    password_hash: "synthetic-password-hash",
    role: SystemRole::Admin,
})
        .await?
        {
            OidcProvisionUserOutcome::Provisioned { user, created } => {
                assert!(
                    !created,
                    "re-provisioning the same external subject must not mint a new identity"
                );
                assert_eq!(
                    user.principal_id, principal_id,
                    "re-provisioning must resolve to the same principal that was deleted"
                );
            }
            OidcProvisionUserOutcome::IdentityCollision => {
                anyhow::bail!("re-provisioning the same external subject must not collide with itself")
            }
        }

        let principal_after_reprovision =
            iam_repository::get_principal_by_id(&postgres, principal_id)
                .await?
                .context("principal must still resolve by id after re-provisioning")?;
        assert_eq!(
            principal_after_reprovision.status, "disabled",
            "re-provisioning must not itself reactivate a disabled principal;              callers reject on status before issuing a session"
        );

        Ok(())
    }
    .await;

    postgres.close().await;
    database.drop().await?;
    result
}
