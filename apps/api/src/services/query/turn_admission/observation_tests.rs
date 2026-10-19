use super::*;
use std::future::{Future, poll_fn};
use std::io::Write;
use std::sync::Mutex;
use std::task::Poll;
use tracing::instrument::WithSubscriber;

#[derive(Clone, Default)]
struct ObservationLog(Arc<Mutex<Vec<u8>>>);

impl Write for ObservationLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("observation log").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ObservationLog {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

async fn capture(future: impl Future<Output = ()>) -> String {
    let log = ObservationLog::default();
    let subscriber =
        tracing_subscriber::fmt().with_ansi(false).without_time().with_writer(log.clone()).finish();
    future.with_subscriber(subscriber).await;
    String::from_utf8(log.0.lock().expect("observation log").clone()).expect("UTF-8 log")
}

fn admission(max_queue: usize) -> QueryTurnAdmission {
    QueryTurnAdmission::new(QueryTurnAdmissionConfig {
        max_concurrent: 1,
        max_queue,
        acquire_timeout: Duration::from_millis(250),
    })
}

fn completed(log: &str) -> Vec<&str> {
    log.lines().filter(|line| line.contains("stage=\"query.admission.completed\"")).collect()
}

async fn poll_waiter_once(waiter: std::pin::Pin<&mut impl Future>) {
    let mut waiter = waiter;
    poll_fn(|cx| {
        assert!(waiter.as_mut().poll(cx).is_pending(), "permit must remain held");
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn admission_duration_excludes_the_admitted_pipeline_and_nested_reuse() {
    let log = capture(async {
        let admission = admission(1);
        with_query_turn_admission(admission.clone(), async {
            tokio::time::advance(Duration::from_secs(30)).await;
            with_query_turn_admission(admission, async { Ok::<_, ApiError>(()) }).await
        })
        .await
        .expect("nested turn");
    })
    .await;
    let events = completed(&log);
    assert_eq!(events.len(), 1, "nested reuse must not count another acquisition");
    assert!(events[0].contains("outcome=\"admitted\""));
    assert!(events[0].contains("queued=false"));
    assert!(events[0].contains("elapsed_ms=0.0"), "pipeline time leaked into wait: {log}");
    assert!(events[0].contains("admission.wait_ms=0.0"), "trace field missing: {log}");
}

#[tokio::test(start_paused = true)]
async fn queued_admission_records_only_the_wait_until_a_permit_is_released() {
    let log = capture(async {
        let admission = admission(1);
        let held = admission.acquire().await.expect("initial permit");
        let mut waiter = Box::pin(admission.acquire());
        poll_waiter_once(waiter.as_mut()).await;
        tokio::time::advance(Duration::from_millis(75)).await;
        drop(held);
        let _permit = waiter.await.expect("queued permit");
        tokio::time::advance(Duration::from_secs(30)).await;
    })
    .await;
    let events = completed(&log);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("outcome=\"admitted\""));
    assert!(events[1].contains("queued=true"));
    assert!(events[1].contains("elapsed_ms=75.0"), "wrong wait: {log}");
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_and_a_closed_semaphore_have_distinct_terminal_outcomes() {
    let log = capture(async {
        let admission = admission(0);
        let _held = admission.acquire().await.expect("initial permit");
        assert_eq!(admission.acquire().await.expect_err("full").kind(), "query_admission_rejected");
        admission.semaphore.close();
        assert_eq!(
            admission.acquire().await.expect_err("closed").kind(),
            "query_admission_rejected"
        );
    })
    .await;
    let events = completed(&log);
    assert_eq!(events.len(), 3);
    assert!(events[1].contains("outcome=\"queue_full\""));
    assert!(events[2].contains("outcome=\"closed\""));
    assert!(events[1..].iter().all(|event| event.contains("queued=false")));
}

#[tokio::test(start_paused = true)]
async fn queued_timeouts_record_duration_and_release_the_waiting_slot() {
    let log = capture(async {
        let admission = admission(1);
        let _held = admission.acquire().await.expect("initial permit");
        assert_eq!(
            admission.acquire().await.expect_err("timeout").kind(),
            "query_admission_rejected"
        );
        assert_eq!(admission.queued_waiters(), 0);
    })
    .await;
    let events = completed(&log);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("outcome=\"timeout\""));
    assert!(events[1].contains("queued=true"));
    assert!(events[1].contains("elapsed_ms=250.0"), "wrong timeout wait: {log}");
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_queued_future_records_once_and_releases_the_waiting_slot() {
    let log = capture(async {
        let admission = admission(1);
        let _held = admission.acquire().await.expect("initial permit");
        let mut waiter = Box::pin(admission.acquire());
        poll_waiter_once(waiter.as_mut()).await;
        tokio::time::advance(Duration::from_millis(60)).await;
        drop(waiter);
        assert_eq!(admission.queued_waiters(), 0);
    })
    .await;
    let events = completed(&log);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("outcome=\"cancelled\""));
    assert!(events[1].contains("queued=true"));
    assert!(events[1].contains("elapsed_ms=60.0"), "wrong cancelled wait: {log}");
}

#[tokio::test(start_paused = true)]
async fn closing_the_semaphore_while_queued_records_the_wait_and_releases_the_slot() {
    let log = capture(async {
        let admission = admission(1);
        let _held = admission.acquire().await.expect("initial permit");
        let mut waiter = Box::pin(admission.acquire());
        poll_waiter_once(waiter.as_mut()).await;
        tokio::time::advance(Duration::from_millis(40)).await;
        admission.semaphore.close();
        assert_eq!(waiter.await.expect_err("closed").kind(), "query_admission_rejected");
        assert_eq!(admission.queued_waiters(), 0);
    })
    .await;
    let events = completed(&log);
    assert_eq!(events.len(), 2);
    assert!(events[1].contains("outcome=\"closed\""));
    assert!(events[1].contains("queued=true"));
    assert!(events[1].contains("elapsed_ms=40.0"), "wrong closed wait: {log}");
}

#[tokio::test(start_paused = true)]
async fn a_never_polled_acquisition_does_not_fabricate_an_observation() {
    let log = capture(async {
        let admission = admission(1);
        let acquisition = admission.acquire();
        drop(acquisition);
        assert_eq!(admission.queued_waiters(), 0);
        assert_eq!(admission.semaphore.available_permits(), 1);
    })
    .await;
    assert!(completed(&log).is_empty(), "an acquisition that never started was counted: {log}");
}
