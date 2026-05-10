-- 0006: vanilla MCP agent under the UI assistant
--
-- The UI assistant handler used to call `query.execute_turn` directly,
-- which made UI a degraded shortcut path next to external MCP agents
-- (openclaw/Telegram). To enforce constitution §16 (MCP–UI parity)
-- structurally, the UI handler now drives an in-process MCP agent —
-- a default LLM with the same library-scoped tool catalogue an external
-- token would see — and lets the agent decide which tool to invoke.
--
-- Schema deltas:
--   1. New `ai_binding_purpose = 'agent'` value. Tool-loop prompt is a
--      different contract (ReAct-ish, tool-use directives) than the
--      grounded-answer prompt that `query_answer` continues to drive.
--      Reusing `query_answer` would make one binding mean two things.
--   2. `runtime_execution.parent_execution_id` so the outer agent run
--      and each MCP tool invocation form a parent/child tree. Without
--      that the assistant debug panel and §16 parity-trace
--      (`grep run_id`) cannot relate the agent's iteration record to
--      the `grounded_answer`-spawned execute_turn underneath.

alter type ai_binding_purpose add value if not exists 'agent';

alter table runtime_execution
    add column if not exists parent_execution_id uuid
        references runtime_execution(id) on delete set null;

create index if not exists runtime_execution_parent_idx
    on runtime_execution (parent_execution_id)
    where parent_execution_id is not null;
