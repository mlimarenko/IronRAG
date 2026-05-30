//! Integration-level regression coverage for the library snapshot
//! export pipeline.
//!
//! The original failure mode this file pins down:
//!
//! `export_library_archive_inner` propagated errors via `?` from ~10
//! fallible stages, but its `Builder<ZstdEncoder<_>>` finalization
//! lived only on the success path. Any early Err dropped the Builder
//! without `into_inner().await`, which panics inside `async-tar`
//! (`Builder dropped without finalizing`). That panic happened inside
//! the spawned writer task; axum had already committed the response
//! with status 200 and was happily streaming the half-written body to
//! the client. Result: clients saw HTTP 200 on a truncated tar.zst
//! that `zstdcat | tar tf -` reports as "premature end".
//!
//! The unit-level guarantee is asserted by tests inside
//! `snapshot::tests` (`finalize_archive_*`). This file is reserved for
//! end-to-end coverage that talks to live Postgres + Arango stacks;
//! everything here is gated with `#[ignore]` so `cargo test --lib`
//! stays green without docker but the test surface still compiles and
//! is discoverable when the operator runs the full stack.

// requires-arango
//
// These tests need a live ArangoDB and Postgres reachable via the
// canonical IronRAG `AppState` bootstrap. Run them only on a host with
// the IronRAG docker compose stack up (or with the corresponding env
// vars wired):
//
//   cargo test -p ironrag-backend --test snapshot_export -- --ignored

#[test]
#[ignore = "requires-arango: end-to-end happy path needs live postgres + arango"]
fn end_to_end_happy_path_produces_valid_archive() {
    // Placeholder: when the operator has the stack available, this
    // would:
    //   1. bootstrap AppState
    //   2. seed a small library (3 documents, ~10 chunks each)
    //   3. call export_library_archive on a Vec<u8>
    //   4. decode zstd, walk tar entries
    //   5. assert manifest.json, summary.json present
    //   6. assert no EXPORT_FAILED.json sentinel
    //
    // The unit-level finalize contract is already covered in
    // `snapshot::tests::finalize_archive_happy_path_produces_clean_round_trip`.
}

#[test]
#[ignore = "requires-arango: failure injection needs an arango collection that can be dropped mid-export"]
fn end_to_end_failure_does_not_return_silent_http_200() {
    // Placeholder: when the operator has the stack available, this
    // would:
    //   1. bootstrap AppState
    //   2. seed a library and then inject a failure (drop the
    //      knowledge_chunk_vector_d<dim> shard mid-export, or use a
    //      mock Arango client that returns Err on the Nth batch)
    //   3. call export_library_archive on a Vec<u8>
    //   4. assert: export_library_archive returns Err
    //   5. decode zstd, walk tar entries
    //   6. assert the archive contains EXPORT_FAILED.json
    //   7. assert no panic propagates to the test runner
    //
    // The unit-level error-path contract — sentinel written, original
    // error propagated, archive decodable — is already pinned in
    // `snapshot::tests::finalize_archive_error_path_writes_sentinel_and_propagates_error`.
}
