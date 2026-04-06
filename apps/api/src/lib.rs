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
