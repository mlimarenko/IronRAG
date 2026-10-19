use anyhow::{Context, Result};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use super::database_url::database_url_with_name;

pub(crate) struct TempPostgresDatabase {
    name: String,
    admin_url: String,
    pub(crate) database_url: String,
}

impl TempPostgresDatabase {
    pub(crate) async fn create(
        base_database_url: &str,
        database_name_prefix: &str,
    ) -> Result<Self> {
        let admin_url = database_url_with_name(base_database_url, "postgres")?;
        let name = format!("{database_name_prefix}_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("failed to connect to postgres admin database")?;

        terminate_database_connections(&admin_pool, &name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop stale test database {name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to create test database {name}"))?;
        admin_pool.close().await;

        Ok(Self {
            database_url: database_url_with_name(base_database_url, &name)?,
            admin_url,
            name,
        })
    }

    pub(crate) async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("failed to reconnect postgres admin database for cleanup")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("failed to drop test database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

async fn terminate_database_connections(admin_pool: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(admin_pool)
    .await
    .context("failed to terminate postgres database connections")?;
    Ok(())
}
