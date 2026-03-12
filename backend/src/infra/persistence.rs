use redis::Client as RedisClient;
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::app::config::Settings;

#[derive(Clone)]
pub struct Persistence {
    pub postgres: PgPool,
    pub redis: RedisClient,
}

impl Persistence {
    /// Connects to Postgres and Redis, verifies Redis responsiveness, and runs migrations.
    ///
    /// # Errors
    /// Returns any database, migration, Redis client, or Redis ping initialization error.
    pub async fn connect(settings: &Settings) -> anyhow::Result<Self> {
        let postgres = PgPoolOptions::new()
            .max_connections(settings.database_max_connections)
            .connect(&settings.database_url)
            .await?;

        sqlx::migrate!("./migrations").run(&postgres).await?;

        let redis = RedisClient::open(settings.redis_url.clone())?;
        let mut conn = redis.get_multiplexed_tokio_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;

        Ok(Self { postgres, redis })
    }
}
