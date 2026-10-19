mod attempt;
mod claim;
mod embedding;
mod errors;
mod extraction;
mod failure;
mod graph;
mod heartbeat;
mod lifecycle;
mod limits;
mod maintenance;
mod pipeline;
mod preparation;
mod runtime;
mod web_jobs;

pub use attempt::WorkerAttemptAbortGuard;
pub use runtime::spawn_ingestion_worker;

#[cfg(feature = "test-support")]
pub use attempt::worker_attempt_abort_guard_for_test;

#[cfg(test)]
mod claim_tests;
#[cfg(test)]
mod errors_tests;
#[cfg(test)]
mod extraction_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod preparation_tests;
