RustRAG Backend — Rust API + worker runtime.

Owns: auth, ingestion, revision lineage, chunking/embedding, graph extraction, Neo4j projection, grounded query, provider profiles, model pricing catalog.

Stack: PostgreSQL, Redis, Neo4j (wired by repo-root docker-compose.yml).

  cargo check && cargo test -q

Docs: ../rustrag.spec-kit/.specify/memory/ (operations/, architecture/, api/, security/).
