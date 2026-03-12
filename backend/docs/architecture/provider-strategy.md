# Provider Strategy

## Goals

- support OpenAI and DeepSeek-like providers early
- allow multiple credentials per workspace and provider
- support future OpenAI-compatible endpoints
- avoid provider SDK lock-in

## Recommended Model

1. `provider_account` stores credential metadata + secret material through a secure storage boundary.
2. `model_profile` defines a callable model configuration bound to a provider account.
3. Runtime services request a `chat`, `embedding`, or `extract` capability from an internal gateway.
4. The gateway selects the provider adapter and credential according to explicit policy.

## `genai` Usage

Use `genai` as the default implementation for standard chat/completion flows.

Do **not** let `genai` leak into business-domain types. Keep internal request/response contracts platform-owned.

## Why This Matters

If later we need provider-specific controls for embeddings, JSON mode, reasoning, or tool semantics, we can add direct adapters without rewriting the whole application layer.
