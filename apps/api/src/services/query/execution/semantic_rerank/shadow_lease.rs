use super::{
    Arc, DISTRIBUTED_SHADOW_LEASE_KEY, DISTRIBUTED_SHADOW_LEASE_TTL_SECONDS,
    MAX_CONCURRENT_SHADOW_TASKS, OnceLock, Result, Uuid, anyhow,
};

pub(crate) fn try_acquire_shadow_task_permit() -> Option<tokio::sync::OwnedSemaphorePermit> {
    static SHADOW_TASK_BUDGET: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SHADOW_TASK_BUDGET
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SHADOW_TASKS)))
        .clone()
        .try_acquire_owned()
        .ok()
}

/// Deployment-wide shadow-call lease.
///
/// The process-local semaphore protects one replica. This Redis lease protects
/// the deployment when several API replicas all observe the same query burst.
/// Shadow work is observational, so coordination errors fail closed and never
/// add paid provider load while Redis is degraded.
pub(crate) async fn try_acquire_distributed_shadow_lease(
    client: &redis::Client,
) -> Result<Option<DistributedShadowLease>> {
    let owner = Uuid::now_v7();
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|error| anyhow!("connect to redis for semantic shadow lease: {error}"))?;
    let response: Option<String> = redis::cmd("SET")
        .arg(DISTRIBUTED_SHADOW_LEASE_KEY)
        .arg(owner.to_string())
        .arg("NX")
        .arg("EX")
        .arg(DISTRIBUTED_SHADOW_LEASE_TTL_SECONDS)
        .query_async(&mut connection)
        .await
        .map_err(|error| anyhow!("acquire semantic shadow lease: {error}"))?;
    Ok(response.is_some().then(|| DistributedShadowLease { client: client.clone(), owner }))
}

pub(crate) struct DistributedShadowLease {
    client: redis::Client,
    owner: Uuid,
}

impl Drop for DistributedShadowLease {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                owner = %self.owner,
                "semantic rerank shadow lease dropped outside an async runtime"
            );
            return;
        };
        let client = self.client.clone();
        let owner = self.owner;
        runtime.spawn(async move {
            if let Err(error) = release_distributed_shadow_lease(&client, owner).await {
                tracing::warn!(
                    owner = %owner,
                    %error,
                    "failed to release semantic rerank shadow lease; TTL recovery remains active"
                );
            }
        });
    }
}

async fn release_distributed_shadow_lease(client: &redis::Client, owner: Uuid) -> Result<bool> {
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|error| anyhow!("connect to redis for semantic shadow lease release: {error}"))?;
    let released: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('DEL', KEYS[1]) \
             else return 0 end",
        )
        .arg(1)
        .arg(DISTRIBUTED_SHADOW_LEASE_KEY)
        .arg(owner.to_string())
        .query_async(&mut connection)
        .await
        .map_err(|error| anyhow!("release semantic shadow lease: {error}"))?;
    Ok(released > 0)
}
