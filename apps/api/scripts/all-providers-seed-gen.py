#!/usr/bin/env python3
"""Generate idempotent ai_model_catalog + ai_model_preset + ai_price_catalog
SQL for OpenAI / DeepSeek / Qwen / OpenRouter / RouterAI / Ollama cloud.

Usage:
    IRONRAG_<PROVIDER>_API_KEY=... python3 all-providers-seed-gen.py PROVIDER
where PROVIDER ∈ {openai, deepseek, qwen, openrouter, ollama-cloud}.

Catalog + preset rows are auto-discovered from /models (or fallback list
if no key / endpoint). Pricing is from a hand-curated table keyed on a
fnmatch-style glob → (input_per_1m, output_per_1m, currency, optionally
cached_input_per_1m).

UUIDs: stable UUID5 of "ironrag-<provider>-<scope>:<key>".
Idempotent: ON CONFLICT on the natural-key indexes.
"""

from __future__ import annotations

import fnmatch
import json
import os
import re
import sys
import uuid
from decimal import Decimal
from pathlib import Path

import requests

NS_DNS = uuid.NAMESPACE_DNS
EFFECTIVE_FROM = "2026-05-09T00:00:00Z"

PROVIDER_CATALOG_IDS = {
    "openai":       "00000000-0000-0000-0000-000000000101",
    "deepseek":     "00000000-0000-0000-0000-000000000102",
    "qwen":         "00000000-0000-0000-0000-000000000103",
    "ollama":       "00000000-0000-0000-0000-000000000104",
    "openrouter":   "00000000-0000-0000-0000-000000000105",
    "routerai":     "00000000-0000-0000-0000-000000000107",
}
ENDPOINTS = {
    "openai":     "https://api.openai.com/v1/models",
    "deepseek":   "https://api.deepseek.com/v1/models",
    "qwen":       "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models",
    "openrouter": "https://openrouter.ai/api/v1/models",
    "routerai":   "https://routerai.ru/api/v1/models",
    "gptunnel":   "https://gptunnel.ru/v1/models",
    "ollama":     "https://ollama.com/api/models",
}
ROLE_TITLE = {
    "embed_chunk": "Embed Chunk",
    "query_retrieve": "Query Retrieve",
    "query_compile": "Query Compile",
    "query_answer": "Query Answer",
    "extract_graph": "Extract Graph",
    "vision": "Vision",
}

# ────────────────────────────────────────────────────────────────────
# Pricing tables — USD per 1M tokens (input, output, cached_input or None).
# Public-list prices as of 2026-06-04. Sources: provider documentation.
# Glob-style key matched against model_name; first match wins.
# ────────────────────────────────────────────────────────────────────
PRICES_USD: dict[str, list[tuple[str, tuple[float, float | None, float | None]]]] = {
    "openai": [
        # gpt-5.5 / gpt-5.4 / gpt-5.x (latest as of 2026-05)
        ("gpt-5.5-pro*",        (15.0, 120.0, 1.50)),
        ("gpt-5.5*",            (5.0,  30.0,  0.50)),
        ("gpt-5.4-pro*",        (15.0, 120.0, 1.50)),
        ("gpt-5.4-mini*",       (0.40, 1.60,  0.05)),
        ("gpt-5.4-nano*",       (0.10, 0.40,  None)),
        ("gpt-5.4*",            (2.50, 10.0,  0.25)),
        ("gpt-5.3-codex*",      (2.50, 10.0,  None)),
        ("gpt-5.2-pro*",        (15.0, 120.0, None)),
        ("gpt-5.2*",            (2.50, 10.0,  0.25)),
        ("gpt-5.1-codex*",      (2.50, 10.0,  None)),
        ("gpt-5.1*",            (2.50, 10.0,  0.25)),
        ("gpt-5-pro*",          (15.0, 120.0, None)),
        ("gpt-5-mini*",         (0.40, 1.60,  0.05)),
        ("gpt-5-nano*",         (0.10, 0.40,  None)),
        ("gpt-5-codex*",        (2.50, 10.0,  None)),
        ("gpt-5-search-api*",   (2.50, 10.0,  None)),
        ("gpt-5-chat-latest*",  (2.50, 10.0,  0.25)),
        ("gpt-5*",              (2.50, 10.0,  0.25)),
        # gpt-4.1 family
        ("gpt-4.1-mini*",       (0.40, 1.60,  0.10)),
        ("gpt-4.1-nano*",       (0.10, 0.40,  0.025)),
        ("gpt-4.1*",            (2.0,  8.0,   0.50)),
        # gpt-4o family
        ("gpt-4o-mini-realtime*", (0.60, 2.40, None)),
        ("gpt-4o-realtime*",      (5.0, 20.0,  None)),
        ("gpt-4o-mini-search*",   (0.15, 0.60, None)),
        ("gpt-4o-search*",        (2.50, 10.0, None)),
        ("gpt-4o-mini-audio*",    (0.15, 0.60, None)),
        ("gpt-4o-audio*",         (2.50, 10.0, None)),
        ("gpt-4o-mini-tts*",      (0.60, 12.0, None)),
        ("gpt-4o-mini-transcribe*",(1.25, 5.0, None)),
        ("gpt-4o-transcribe*",    (2.50, 10.0, None)),
        ("gpt-4o-mini*",          (0.15, 0.60, 0.075)),
        ("gpt-4o*",               (2.50, 10.0, 1.25)),
        # o-series reasoning
        ("o4-mini-deep-research*",(2.0, 8.0,   None)),
        ("o4-mini*",              (1.10, 4.40, 0.275)),
        ("o3-pro*",               (20.0, 80.0, None)),
        ("o3-mini*",              (1.10, 4.40, 0.55)),
        ("o3-deep-research*",     (10.0, 40.0, None)),
        ("o3*",                   (2.0, 8.0,   0.50)),
        ("o1-pro*",               (150.0, 600.0, None)),
        ("o1-mini*",              (1.10, 4.40, 0.55)),
        ("o1*",                   (15.0, 60.0,  7.50)),
        # Embeddings
        ("text-embedding-3-large*", (0.13, None, None)),
        ("text-embedding-3-small*", (0.02, None, None)),
        ("text-embedding-ada-002*", (0.10, None, None)),
        # Legacy chat
        ("gpt-4-turbo*", (10.0, 30.0, None)),
        ("gpt-4*",       (30.0, 60.0, None)),
        ("gpt-3.5-turbo*", (0.50, 1.50, None)),
        # Image / audio generation models — billed per-image; skip token rows
    ],
    "deepseek": [
        ("deepseek-v4-pro*",    (0.55, 2.19, 0.14)),
        ("deepseek-v4-flash*",  (0.27, 1.10, 0.07)),
        ("deepseek-reasoner*",  (0.55, 2.19, 0.14)),    # DeepSeek-R1 / V3-R
        ("deepseek-chat*",      (0.27, 1.10, 0.07)),
        ("deepseek-v3*",        (0.27, 1.10, 0.07)),
        ("deepseek-coder*",     (0.27, 1.10, 0.07)),
    ],
    "qwen": [
        # Qwen3 / Qwen3.5 / Qwen3.6 / Qwen3.7 series
        ("qwen3.7-max*",        (1.25, 3.75, None)),
        ("qwen3.7-plus*",       (0.40, 1.60, None)),
        ("qwen3.6-max*",        (1.60, 6.40, None)),
        ("qwen3.6-plus*",       (0.80, 3.20, None)),
        ("qwen3.6-27b*",        (0.30, 1.20, None)),
        ("glm-5.1*",            (0.98, 3.08, None)),
        ("kimi-k2.6*",          (0.684, 3.42, None)),
        ("minimax-m2.5*",       (0.15, 1.15, None)),
        ("mimo-v2.5-pro*",      (0.15, 1.15, None)),
        ("deepseek-v4-pro*",    (0.435, 0.87, None)),
        ("deepseek-v4-flash*",  (0.0983, 0.1966, None)),
        ("qwen3.5-max*",        (1.60, 6.40, None)),
        ("qwen3.5-plus*",       (0.80, 3.20, None)),
        ("qwen3.5-flash*",      (0.075, 0.30, None)),
        ("qwen3-coder*",        (0.50, 2.0,  None)),
        ("qwen3-vl-plus*",      (0.80, 3.20, None)),
        ("qwen3-vl-max*",       (1.60, 6.40, None)),
        ("qwen3-max*",          (1.60, 6.40, None)),
        ("qwen3-plus*",         (0.80, 3.20, None)),
        ("qwen3-turbo*",        (0.30, 1.20, None)),
        ("qwen3-flash*",        (0.075, 0.30, None)),
        ("qwen3*",              (0.30, 1.20, None)),
        ("qwen-vl-max*",        (1.60, 6.40, None)),
        ("qwen-vl-plus*",       (0.80, 3.20, None)),
        ("qwen-max*",           (1.60, 6.40, None)),
        ("qwen-plus*",          (0.80, 3.20, None)),
        ("qwen-turbo*",         (0.30, 1.20, None)),
        ("qwen-coder*",         (0.50, 2.0,  None)),
        ("text-embedding-v*",   (0.05, None, None)),
        ("text-embedding-async-v*", (0.05, None, None)),
    ],
    "openrouter": [],   # no API key, no public price feed reachable here
    "ollama": [],       # local-only by default; cloud subset handled separately
}


def stable_uuid(*parts: str) -> str:
    return str(uuid.uuid5(NS_DNS, ":".join(parts)))


def sql_escape(value: str) -> str:
    return value.replace("'", "''")


def classify(model_id: str) -> tuple[str, str, list[str]]:
    """Return (capability_kind, modality_kind, default_roles).

    Strict bucket split (per UX feedback — no audio/image/legacy
    completion models in chat-stage dropdowns):

    embedding (capability_kind=embedding, modality=text)
      → roles: embed_chunk, query_retrieve
      Match: model name contains 'embedding' or ends with '-embed' /
      ':embed' / '-emb'.

    chat-multimodal (capability_kind=chat, modality=multimodal)
      → roles: query_answer + extract_graph + query_compile + vision
      Match: vision-capable chat models (claude/gemini/gpt-4o/4.1/5*,
      qwen-vl, pixtral, molmo, kimi-vl, internvl, llama-vision, etc.).

    chat-text (capability_kind=chat, modality=text)
      → roles: query_answer + extract_graph + query_compile
      Match: pure text chat models that follow JSON-schema-ish format.

    Excluded entirely (capability='', skipped from catalog):
      - audio: whisper, tts, transcribe, audio-preview, realtime, audio-
      - image: dall-e, gpt-image, *image-1, image-edit, stable-diffusion,
        flux, midjourney
      - video: video-, sora, runway-
      - legacy completion-only: davinci, babbage, ada, curie, gpt-3,
        text-completion (these aren't chat-format).
      - specialized agent: computer-use, search-api, deep-research
        (purpose-bound, not free-form chat).
      - moderation: moderation, omni-moderation, safety
      - reranker: -rerank, /rerank-

    Strictness rationale: every preset visible under a chat-stage
    role (extract_graph / query_compile / query_answer) must accept
    arbitrary system+user messages and return free-form chat with
    structured-output capability. Audio/image/legacy/agent models
    cannot.
    """
    low = model_id.lower()

    # Embeddings.
    if (
        "embedding" in low
        or low.endswith("-embed")
        or low.endswith(":embed")
        or low.endswith("-emb")
        or "/embeddings" in low
    ):
        return ("embedding", "text", ["embed_chunk", "query_retrieve"])

    # Hard exclusions: families that are NOT free-form chat.
    audio_markers = (
        "whisper", "/tts", "-tts", "transcribe", "audio-preview",
        "realtime", "-audio", "audio-",
    )
    image_markers = (
        "dall-e", "dalle", "gpt-image", "image-1", "image-2",
        "image-3", "image-edit", "image-gen", "stable-diffusion",
        "/flux", "flux-", "midjourney", "playground-",
        "mj-", "ideogram", "recraft", "faceswap", "face-swap",
    )
    video_markers = (
        "video-", "sora", "runway-", "veo-", "kling-",
        "mj-video", "haiper", "luma-",
    )
    legacy_completion_markers = (
        "davinci", "babbage", "ada-", "curie", "gpt-3-",
        "text-completion", "text-davinci",
    )
    specialized_agent_markers = (
        "computer-use", "search-api", "deep-research",
        "browser-use", "operator",
    )
    moderation_markers = ("moderation", "omni-moderation", "safety-")
    reranker_markers = ("rerank", "-reranker")
    skip_groups = (
        audio_markers,
        image_markers,
        video_markers,
        legacy_completion_markers,
        specialized_agent_markers,
        moderation_markers,
        reranker_markers,
    )
    for group in skip_groups:
        for marker in group:
            if marker in low:
                return ("", "", [])

    # Vision-capable chat models. Reasonably exhaustive provider matrix
    # as of 2026-05; treat anything matching as multimodal.
    vision_markers = (
        "gpt-4o", "gpt-4.1", "gpt-5",
        "claude-3", "claude-4", "claude-5", "claude-opus", "claude-sonnet",
        "claude-haiku",
        "gemini", "imagen-vision",
        "grok-2-vision", "grok-3-vision", "grok-4", "grok-vision",
        "qwen-vl", "qwen2-vl", "qwen2.5-vl", "qwen3-vl", "qwen3.5-vl",
        "qwen3.6-vl", "qwen3.7-vl",
        "vl-", "-vl-", "-vl:",
        "pixtral", "mistral-medium", "mistral-large",
        "molmo", "kimi-vl", "internvl", "minimax-m", "yi-vision",
        "llama-3.2-vision", "llama-3.3-vision", "llama-vision",
        "llama4", "step-3", "phi-vision", "phi-4-vision", "lfm-vision",
    )
    is_multimodal = any(m in low for m in vision_markers)
    chat_roles = ["query_answer", "extract_graph", "query_compile"]
    if is_multimodal:
        return ("chat", "multimodal", chat_roles + ["vision"])

    return ("chat", "text", chat_roles)


def fetch_models(provider: str, api_key: str | None) -> list[dict]:
    """Return raw model dicts (preserves pricing/architecture fields)."""
    url = ENDPOINTS[provider]
    headers = {"Accept": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    try:
        r = requests.get(url, headers=headers, timeout=30)
        r.raise_for_status()
    except Exception as exc:
        print(f"WARN: could not fetch {url}: {exc}", file=sys.stderr)
        return []
    body = r.json()
    items = body.get("data", body if isinstance(body, list) else [])
    items = [m for m in items if m.get("id")]
    items.sort(key=lambda m: m["id"])
    return items


def live_pricing_per_million(model: dict) -> tuple[Decimal, Decimal | None] | None:
    """Convert per-token live pricing into USD per 1M tokens."""
    pricing = model.get("pricing")
    if not isinstance(pricing, dict):
        return None
    try:
        prompt = Decimal(str(pricing.get("prompt", "0")))
        completion = Decimal(str(pricing.get("completion", "0")))
    except Exception:
        return None
    if prompt <= 0 and completion <= 0:
        return None
    inp = (prompt * 1_000_000) if prompt > 0 else None
    out = (completion * 1_000_000) if completion > 0 else None
    if inp is None and out is None:
        return None
    return (inp, out)


def find_price(provider: str, model_name: str) -> tuple[float, float | None, float | None] | None:
    for pattern, prices in PRICES_USD.get(provider, []):
        if fnmatch.fnmatchcase(model_name, pattern):
            return prices
    return None


def emit(provider: str, models: list[dict]) -> str:
    catalog_id = PROVIDER_CATALOG_IDS[provider]
    catalog_rows: list[str] = []
    preset_rows: list[str] = []
    price_rows: list[str] = []
    for model in models:
        model_id = model["id"]
        capability, modality, roles = classify(model_id)
        if not capability:
            continue
        cid = stable_uuid("ironrag", provider, "model", model_id, capability)
        metadata = json.dumps(
            {"defaultRoles": roles, "seedSource": "provider_catalog"},
            separators=(",", ":"),
        )
        catalog_rows.append(
            f"    ('{cid}'::uuid, '{catalog_id}'::uuid, '{sql_escape(model_id)}', "
            f"'{capability}'::ai_model_capability_kind, "
            f"'{modality}'::ai_model_modality_kind, "
            f"'active'::ai_model_lifecycle_state, "
            f"'{sql_escape(metadata)}'::jsonb)"
        )
        for role in roles:
            preset_name = f"{provider.title()} {ROLE_TITLE.get(role, role.title())} · {model_id}"
            if capability == "embedding":
                temperature = "NULL"
                top_p = "NULL"
            else:
                temperature = "0.3"
                top_p = "0.9"
            preset_rows.append(
                f"        ('{sql_escape(model_id)}', '{sql_escape(preset_name)}', "
                f"{temperature}, {top_p})"
            )
        # Live per-token pricing (openrouter / routerai expose pricing.prompt+completion)
        # takes precedence over the static USD glob table; fall back to the static
        # table only when the live feed is silent.
        live = live_pricing_per_million(model)
        if live is not None:
            input_p, output_p = live
            cached_p = None
        else:
            static = find_price(provider, model_id)
            if static is None:
                continue
            input_p_f, output_p_f, cached_p_f = static
            input_p = Decimal(str(input_p_f)) if input_p_f is not None else None
            output_p = Decimal(str(output_p_f)) if output_p_f is not None else None
            cached_p = Decimal(str(cached_p_f)) if cached_p_f is not None else None
        for unit, val in (
            ("per_1m_input_tokens", input_p),
            ("per_1m_output_tokens", output_p),
            ("per_1m_cached_input_tokens", cached_p),
        ):
            if val is None:
                continue
            pid = stable_uuid("ironrag", provider, "price", model_id, unit)
            price_rows.append(
                f"        ('{pid}'::uuid, '{sql_escape(model_id)}', "
                f"'{unit}'::billing_unit, {val:f}, 'USD')"
            )

    lines: list[str] = [
        f"-- Auto-generated {provider} catalog + preset + price seed.",
        f"-- Source: scripts/all-providers-seed-gen.py {provider}",
        f"-- Idempotent: ON CONFLICT on natural-key indexes.",
        "",
    ]
    if catalog_rows:
        lines.extend([
            "INSERT INTO ai_model_catalog (",
            "    id, provider_catalog_id, model_name, capability_kind,",
            "    modality_kind, lifecycle_state, metadata_json",
            ") VALUES",
            ",\n".join(catalog_rows),
            "ON CONFLICT (provider_catalog_id, model_name, capability_kind) DO NOTHING;",
            "",
        ])
    if preset_rows:
        lines.extend([
            "INSERT INTO ai_model_preset (",
            "    scope_kind, workspace_id, library_id,",
            "    model_catalog_id, preset_name,",
            "    temperature, top_p, extra_parameters_json",
            ")",
            "SELECT",
            "    'instance'::ai_scope_kind, NULL, NULL,",
            "    m.id, p.preset_name, p.temperature, p.top_p, '{}'::jsonb",
            "FROM ai_model_catalog m",
            "JOIN (VALUES",
            ",\n".join(preset_rows),
            ") AS p(model_name, preset_name, temperature, top_p)",
            "    ON p.model_name = m.model_name",
            f"WHERE m.provider_catalog_id = '{catalog_id}'::uuid",
            "ON CONFLICT DO NOTHING;",
            "",
        ])
    if price_rows:
        lines.extend([
            "INSERT INTO ai_price_catalog (",
            "    id, model_catalog_id, billing_unit, price_variant_key,",
            "    request_input_tokens_min, request_input_tokens_max,",
            "    unit_price, currency_code, effective_from, catalog_scope, workspace_id",
            ")",
            "SELECT",
            "    p.price_id, m.id, p.billing_unit, 'default',",
            "    NULL, NULL, p.unit_price, p.currency_code,",
            f"    '{EFFECTIVE_FROM}'::timestamptz, 'system'::ai_price_catalog_scope, NULL",
            "FROM ai_model_catalog m",
            "JOIN (VALUES",
            ",\n".join(price_rows),
            ") AS p(price_id, model_name, billing_unit, unit_price, currency_code)",
            "    ON p.model_name = m.model_name",
            f"WHERE m.provider_catalog_id = '{catalog_id}'::uuid",
            "ON CONFLICT DO NOTHING;",
            "",
        ])

    return "\n".join(lines)


def load_api_key(provider: str) -> str | None:
    env_name = f"IRONRAG_{provider.upper()}_API_KEY"
    val = os.environ.get(env_name)
    if val:
        return val
    env_path = Path("/home/leader/sources/gitlab.piping.space/general/tools/ironrag/.env")
    if env_path.exists():
        for line in env_path.read_text().splitlines():
            if line.startswith(f"{env_name}="):
                return line.split("=", 1)[1].strip()
    return None


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: all-providers-seed-gen.py PROVIDER", file=sys.stderr)
        return 1
    provider = sys.argv[1]
    if provider not in PROVIDER_CATALOG_IDS:
        print(f"unknown provider: {provider}", file=sys.stderr)
        return 1
    api_key = load_api_key(provider)
    models = fetch_models(provider, api_key)
    print(f"-- {provider}: discovered {len(models)} models", file=sys.stderr)
    sys.stdout.write(emit(provider, models))
    return 0


if __name__ == "__main__":
    sys.exit(main())
