use std::{future::Future, time::Duration};

use anyhow::Context as _;
use futures::{Stream, StreamExt as _};
use tokio::task::{AbortHandle, JoinError, JoinHandle};

use super::shutdown::ShutdownSignal;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

pub(super) struct CriticalTask {
    name: &'static str,
    handle: JoinHandle<()>,
}

impl CriticalTask {
    pub(super) const fn new(name: &'static str, handle: JoinHandle<()>) -> Self {
        Self { name, handle }
    }
}

struct TaskCompletion {
    name: &'static str,
    result: Result<(), JoinError>,
}

pub(super) async fn supervise<F>(
    role_future: F,
    critical_tasks: Vec<CriticalTask>,
    shutdown: ShutdownSignal,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    supervise_with_grace(role_future, critical_tasks, shutdown, SHUTDOWN_GRACE).await
}

async fn supervise_with_grace<F>(
    role_future: F,
    critical_tasks: Vec<CriticalTask>,
    shutdown: ShutdownSignal,
    shutdown_grace: Duration,
) -> anyhow::Result<()>
where
    F: Future<Output = anyhow::Result<()>>,
{
    if critical_tasks.is_empty() {
        return role_future.await;
    }

    let abort_handles =
        critical_tasks.iter().map(|task| task.handle.abort_handle()).collect::<Vec<_>>();
    let mut completions = critical_tasks
        .into_iter()
        .map(|task| async move { TaskCompletion { name: task.name, result: task.handle.await } })
        .collect::<futures::stream::FuturesUnordered<_>>();
    tokio::pin!(role_future);

    tokio::select! {
        role_result = &mut role_future => {
            let _ = shutdown.trigger();
            let task_result = drain_tasks(&mut completions, &abort_handles, shutdown_grace).await;
            role_result.and(task_result)
        }
        completion = completions.next() => {
            let Some(completion) = completion else {
                anyhow::bail!("critical task supervisor lost all registered tasks");
            };
            let expected_shutdown = shutdown.is_triggered();
            let first_failure = completion_failure(completion, expected_shutdown);
            let _ = shutdown.trigger();

            let role_result = match tokio::time::timeout(shutdown_grace, &mut role_future).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "service role did not stop within {} seconds after critical task termination",
                    shutdown_grace.as_secs(),
                )),
            };
            let task_result = drain_tasks(&mut completions, &abort_handles, shutdown_grace).await;

            if let Some(error) = first_failure {
                return Err(error);
            }
            role_result.and(task_result)
        }
    }
}

fn completion_failure(
    completion: TaskCompletion,
    expected_shutdown: bool,
) -> Option<anyhow::Error> {
    match completion.result {
        Ok(()) if expected_shutdown => None,
        Ok(()) => Some(anyhow::anyhow!(
            "critical background task `{}` terminated before shutdown",
            completion.name,
        )),
        Err(error) => Some(
            anyhow::Error::new(error)
                .context(format!("critical background task `{}` failed", completion.name)),
        ),
    }
}

async fn drain_tasks<S>(
    completions: &mut S,
    abort_handles: &[AbortHandle],
    shutdown_grace: Duration,
) -> anyhow::Result<()>
where
    S: Stream<Item = TaskCompletion> + Unpin,
{
    let drain = async {
        while let Some(completion) = completions.next().await {
            completion.result.with_context(|| {
                format!(
                    "critical background task `{}` failed while shutting down",
                    completion.name,
                )
            })?;
        }
        Ok(())
    };

    if let Ok(result) = tokio::time::timeout(shutdown_grace, drain).await {
        result
    } else {
        for handle in abort_handles {
            handle.abort();
        }
        anyhow::bail!(
            "critical background tasks did not stop within {} seconds",
            shutdown_grace.as_secs(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use tokio::time::{Duration, timeout};

    use super::{CriticalTask, supervise_with_grace};
    use crate::app::shutdown::ShutdownSignal;

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);
    const TEST_GRACE: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn clean_critical_task_exit_is_fail_fast() {
        let shutdown = ShutdownSignal::new();
        let role_shutdown = shutdown.clone();
        let result = timeout(
            TEST_TIMEOUT,
            supervise_with_grace(
                async move {
                    role_shutdown.wait().await;
                    Ok(())
                },
                vec![CriticalTask::new("worker", tokio::spawn(async {}))],
                shutdown.clone(),
                TEST_GRACE,
            ),
        )
        .await;

        assert!(result.is_ok(), "supervisor must not hang");
        let failure = result.ok().and_then(Result::err);
        assert!(shutdown.is_triggered());
        assert!(
            failure.as_ref().is_some_and(|error| error.to_string().contains("worker")),
            "unexpected exit must identify the failed task",
        );
    }

    #[tokio::test]
    #[allow(clippy::panic, reason = "the test must produce a panicked JoinHandle")]
    async fn critical_task_panic_is_propagated() {
        let shutdown = ShutdownSignal::new();
        let role_shutdown = shutdown.clone();
        let task = tokio::spawn(async {
            panic!("simulated critical task panic");
        });
        let result = timeout(
            TEST_TIMEOUT,
            supervise_with_grace(
                async move {
                    role_shutdown.wait().await;
                    Ok(())
                },
                vec![CriticalTask::new("relay", task)],
                shutdown,
                TEST_GRACE,
            ),
        )
        .await;

        assert!(result.is_ok(), "supervisor must not hang");
        let failure = result.ok().and_then(Result::err);
        assert!(
            failure.as_ref().is_some_and(|error| error.to_string().contains("relay")),
            "panic must identify the failed task",
        );
    }

    #[tokio::test]
    async fn normal_role_completion_drains_tasks() {
        let shutdown = ShutdownSignal::new();
        let mut task_shutdown = shutdown.subscribe();
        let task = tokio::spawn(async move {
            let _ = task_shutdown.recv().await;
        });
        let role_shutdown = shutdown.clone();
        let result = timeout(
            TEST_TIMEOUT,
            supervise_with_grace(
                async move {
                    let _ = role_shutdown.trigger();
                    Ok(())
                },
                vec![CriticalTask::new("worker", task)],
                shutdown,
                TEST_GRACE,
            ),
        )
        .await;

        assert!(result.is_ok_and(|inner| inner.is_ok()));
    }

    #[tokio::test]
    async fn hung_task_is_aborted_after_grace_period() {
        let shutdown = ShutdownSignal::new();
        let task = tokio::spawn(pending::<()>());
        let result = timeout(
            TEST_TIMEOUT,
            supervise_with_grace(
                async { Ok(()) },
                vec![CriticalTask::new("hung-worker", task)],
                shutdown,
                TEST_GRACE,
            ),
        )
        .await;

        assert!(result.is_ok(), "supervisor must enforce its own deadline");
        let failure = result.ok().and_then(Result::err);
        assert!(failure.as_ref().is_some_and(|error| error.to_string().contains("did not stop")),);
    }
}
