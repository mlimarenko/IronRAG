# MCP grounded_answer contract snapshots

`mcp_grounded_answer_contract.rs` snapshots the JSON shape returned by the
`grounded_answer` MCP tool wrapper.

The live tool is intentionally DB-bound: it resolves library access, creates an
ephemeral MCP conversation, and delegates to the canonical grounded-answer query
turn executor. The built-in UI assistant is a separate MCP-client-style agent
over the answer tool surface; when it chooses `grounded_answer`, it reaches this
same live tool through the in-process MCP dispatcher. The integration test
therefore calls the shared pure serializer,
`grounded_answer_contract_payload`, with deterministic synthetic assistant
execution details. That keeps the contract in `cargo test` without a database,
ArangoDB, Redis, or an external LLM key.

The snapshots cover:

- top-level MCP tool-result keys (`content`, `isError`, `structuredContent`)
- structured-content shortcut keys (`runtimeExecutionId`, `executionId`,
  `conversationId`, `libraryId`, `workspaceId`, `lifecycleState`)
- citation counts across chunk, prepared-segment, technical-fact, entity, and
  relation references
- verifier state and warning shape
- runtime stage summary item shape
- request/response turn shape

The snapshots do not execute library resolution, conversation creation,
retrieval, answer generation, or verifier semantics. Full agent-to-MCP semantic
parity depends on the UI agent and external MCP clients sharing the same answer
tool descriptors, schemas, dispatcher, and grounded-answer result contract.
Runtime probes remain the end-to-end check for that path; this suite pins the
DB-free wire shape that MCP clients consume.

All fixture questions, library refs, document titles, IDs, answers, and warning
messages are synthetic and self-contained. Do not add production questions,
customer library names, provider names, document titles, hosts, or corpus-specific
measurements to this test or its snapshots.

To update the snapshots after an intentional contract change:

```bash
INSTA_UPDATE=always cargo test --test mcp_grounded_answer_contract -p ironrag-backend
git diff -- apps/api/tests/snapshots/
```

Review the diff as a wire-contract change before committing it.
