// Async hot paths in the query and ingest runtimes produce deeply
// nested opaque future types. The default 128-step evaluation budget
// on Send/Sync resolution is not enough for them. 512 is the
// canonical escape hatch — same value the tokio ecosystem uses for
// comparable nested async builders.
#![recursion_limit = "512"]
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::panic,
        clippy::string_lit_as_bytes,
        clippy::unwrap_used,
        clippy::useless_vec,
        clippy::len_zero
    )
)]

pub mod agent_runtime;
pub mod app;
pub mod domains;
pub mod infra;
pub mod integrations;
pub mod interfaces;
pub mod mcp_types;
pub mod services;
pub mod shared;
