//! Knowledge-plane storage ports (trait surfaces).
//!
//! These traits are the boundary between query/ingest services and the concrete
//! `PostgreSQL` knowledge-plane adapter. The traits stay as domain ports so the
//! services do not depend on table layout details, but runtime backend selection
//! has been removed: `PostgreSQL` is the single implementation.
//!
//! ## Surface split
//!
//! Storage responsibilities are split by domain: [`DocumentStore`],
//! [`SearchStore`], [`GraphStore`], and [`ContextStore`]. Infrastructure
//! construction details stay outside these traits; callers receive trait objects
//! and use only domain-level operations.
//!
//! ## Leaky contracts (design §14.4)
//!
//! Three observable behaviors are part of the contract and are pinned in the
//! relevant method doc-comments below:
//!
//! 1. **Input-rank ordering.** Membership reads that take an ordered id slice and
//!    rank their output by the *input* position (`unnest(...) WITH ORDINALITY`)
//!    must return rows ordered by the rank of
//!    the input id, not by storage order. See
//!    [`DocumentStore::list_source_profile_chunks_by_revisions`].
//! 2. **Write-count returns.** Methods returning a mutation count return the
//!    number of rows actually written/removed (`cmd_tuples`/`RETURNING`), not a request count. See the `delete_*`/`u64`
//!    methods on [`SearchStore`], [`GraphStore`], and [`ContextStore`].
//! 3. **Vector write-routing is hidden.** Callers never name a physical
//!    dimension relation. [`SearchStore::upsert_chunk_vectors_bulk`] (and its
//!    singular sibling) own routing into a shared typed-by-dimension table;
//!    library, profile, and vector kind identify logical lanes inside it.
//!
//! ## Required edge direction
//!
//! `evidence_source_edge` is written EVIDENCE→REVISION by
//! [`GraphStore::upsert_evidence_source_edge`]. The required direction is
//! **EVIDENCE→REVISION** and the `PostgreSQL` adapter enforces it with an FK.

mod context_store;
mod document_store;
mod graph_store;
mod search_store;
mod write_authority;

pub use context_store::*;
pub use document_store::*;
pub use graph_store::*;
pub use search_store::*;
pub use write_authority::*;
