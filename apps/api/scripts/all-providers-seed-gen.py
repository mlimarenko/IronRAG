#!/usr/bin/env python3
"""Generate idempotent ai_model_catalog + ai_price_catalog SQL for
OpenAI / DeepSeek / Qwen / GPTunnel / OpenRouter / RouterAI /
MiniMax / Ollama cloud.

Usage:
    IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=... python3 all-providers-seed-gen.py PROVIDER
where PROVIDER ∈ {openai, deepseek, qwen, gptunnel, openrouter, routerai,
minimax, ollama-cloud}.

Catalog rows are auto-discovered from /models (or fallback list if no
key / endpoint). Model capabilities are accepted only from typed provider
metadata or the operator-supplied
IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64 manifest; unknown models are skipped
fail-closed. Pricing is from a hand-curated table keyed on a
fnmatch-style glob → (input_per_1m, output_per_1m, currency, optionally
cached_input_per_1m).

Binding presets are NOT generated here. Migration 0004
(ai_config_simplification) dropped the ai_model_preset table; per-purpose
bootstrap defaults now live as the hand-curated "bootstrapPresets" array
inside ai_provider_catalog.capability_flags_json — one entry per binding
purpose with camelCase keys {purpose, modelName, temperature, topP,
maxOutputTokensOverride, extraParametersJson}, consumed by
services/ai_catalog_service/bootstrap.rs. Picking which model backs each
purpose is a curation decision; author that array by hand in the provider
seed row (see migrations/0003_minimax_provider_catalog.sql for the shape).

UUIDs: stable UUID5 of "ironrag-<provider>-<scope>:<key>".
Idempotent: ON CONFLICT on the natural-key indexes.
"""

from __future__ import annotations

import base64
import fnmatch
import json
import os
import sys
import uuid
from decimal import Decimal
from pathlib import Path

import requests

REPO_ROOT = Path(__file__).resolve().parents[3]
NS_DNS = uuid.NAMESPACE_DNS
EFFECTIVE_FROM = "2026-05-09T00:00:00Z"
EFFECTIVE_FROM_BY_PROVIDER = {
    "minimax": "2026-06-26T00:00:00Z",
}

PROVIDER_CATALOG_IDS = {
    "openai":       "00000000-0000-0000-0000-000000000101",
    "deepseek":     "00000000-0000-0000-0000-000000000102",
    "qwen":         "00000000-0000-0000-0000-000000000103",
    "ollama":       "00000000-0000-0000-0000-000000000104",
    "openrouter":   "00000000-0000-0000-0000-000000000105",
    "gptunnel":     "00000000-0000-0000-0000-000000000106",
    "routerai":     "00000000-0000-0000-0000-000000000107",
    "minimax":      "00000000-0000-0000-0000-000000000108",
}
ENDPOINTS = {
    "openai":     "https://api.openai.com/v1/models",
    "deepseek":   "https://api.deepseek.com/v1/models",
    "qwen":       "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models",
    "openrouter": "https://openrouter.ai/api/v1/models",
    "routerai":   "https://routerai.ru/api/v1/models",
    "gptunnel":   "https://gptunnel.ru/v1/models",
    "minimax":    "https://api.minimax.io/v1/models",
    "ollama":     "https://ollama.com/api/models",
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
    "minimax": [
        ("MiniMax-M2.7-highspeed", (0.60, 2.40, 0.06)),
        ("MiniMax-M2.7",           (0.30, 1.20, 0.06)),
        ("MiniMax-M2.5-highspeed", (0.60, 2.40, 0.03)),
        ("MiniMax-M2.5",           (0.30, 1.20, 0.03)),
        ("MiniMax-M2.1-highspeed", (0.60, 2.40, 0.03)),
        ("MiniMax-M2.1",           (0.30, 1.20, 0.03)),
        ("MiniMax-M2",             (0.30, 1.20, 0.03)),
    ],
    "ollama": [],       # local-only by default; cloud subset handled separately
}

RANGE_PRICES_USD: dict[str, dict[str, list[tuple[str, int | None, int | None, float]]]] = {
    "minimax": {
        "MiniMax-M3": [
            ("per_1m_input_tokens", None, 512_000, 0.30),
            ("per_1m_input_tokens", 512_001, None, 0.60),
            ("per_1m_output_tokens", None, 512_000, 1.20),
            ("per_1m_output_tokens", 512_001, None, 2.40),
            ("per_1m_cached_input_tokens", None, 512_000, 0.06),
            ("per_1m_cached_input_tokens", 512_001, None, 0.12),
        ],
    },
}

MODEL_CAPABILITY_ENV = "IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64"
PROVIDER_API_KEYS_ENV = "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"
MAX_CAPABILITY_MANIFEST_BYTES = 4_194_304
MAX_CAPABILITY_MODELS = 100_000
MAX_PROVIDER_KEY_MAP_BYTES = 1_048_576
MAX_PROVIDER_KEYS = 256
MAX_PROVIDER_KEY_BYTES = 65_536
MODEL_CAPABILITY_KINDS = frozenset({"chat", "embedding"})
MODEL_MODALITY_KINDS = frozenset({"text", "multimodal"})
TEXT_CHAT_ROLES = (
    "extract_graph",
    "query_compile",
    "query_answer",
    "agent",
)
EMBEDDING_ROLES = ("embed_chunk",)


def stable_uuid(*parts: str) -> str:
    return str(uuid.uuid5(NS_DNS, ":".join(parts)))


def sql_escape(value: str) -> str:
    return value.replace("'", "''")


def typed_metadata_candidates(
    model: dict,
    manifest_entry: object | None,
) -> list[object]:
    if manifest_entry is not None:
        return [manifest_entry]
    return [model.get("ironragCapabilities"), model.get("metadata"), model]


def parse_typed_signature(typed: dict) -> tuple[str, str, list[str]] | None:
    capability = typed.get("capabilityKind", typed.get("capability_kind"))
    modality = typed.get("modalityKind", typed.get("modality_kind"))
    if capability not in MODEL_CAPABILITY_KINDS or modality not in MODEL_MODALITY_KINDS:
        return None
    if capability == "embedding":
        if modality != "text":
            return None
        return capability, modality, list(EMBEDDING_ROLES)

    roles = list(TEXT_CHAT_ROLES)
    if modality == "multimodal":
        roles.append("extract_text")
    return capability, modality, roles


def typed_model_signature(
    model: dict,
    manifest_entry: object | None = None,
) -> tuple[str, str, list[str]] | None:
    """Return a validated typed model signature without inspecting its name."""
    typed_fields = {
        "capabilityKind",
        "capability_kind",
        "modalityKind",
        "modality_kind",
    }
    for typed in typed_metadata_candidates(model, manifest_entry):
        if isinstance(typed, dict) and typed_fields.intersection(typed):
            return parse_typed_signature(typed)
    return None


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


def find_range_prices(
    provider: str,
    model_name: str,
) -> list[tuple[str, int | None, int | None, Decimal]]:
    rows = RANGE_PRICES_USD.get(provider, {}).get(model_name, [])
    return [
        (unit, min_tokens, max_tokens, Decimal(str(unit_price)))
        for unit, min_tokens, max_tokens, unit_price in rows
    ]


def model_catalog_row(
    catalog_id: str,
    provider: str,
    model_id: str,
    signature: tuple[str, str, list[str]],
) -> str:
    capability, modality, roles = signature
    model_catalog_id = stable_uuid("ironrag", provider, "model", model_id, capability)
    metadata = json.dumps(
        {"defaultRoles": roles, "seedSource": "provider_catalog"},
        separators=(",", ":"),
    )
    return (
        f"    ('{model_catalog_id}'::uuid, '{catalog_id}'::uuid, "
        f"'{sql_escape(model_id)}', '{capability}'::ai_model_capability_kind, "
        f"'{modality}'::ai_model_modality_kind, "
        f"'active'::ai_model_lifecycle_state, "
        f"'{sql_escape(metadata)}'::jsonb)"
    )


def range_price_rows(
    provider: str,
    model_id: str,
) -> list[str]:
    rows: list[str] = []
    for unit, min_tokens, max_tokens, value in find_range_prices(provider, model_id):
        min_sql = "NULL" if min_tokens is None else str(min_tokens)
        max_sql = "NULL" if max_tokens is None else str(max_tokens)
        price_id = stable_uuid(
            "ironrag", provider, "price", model_id, unit, min_sql, max_sql
        )
        rows.append(
            f"        ('{price_id}'::uuid, '{sql_escape(model_id)}', "
            f"'{unit}'::billing_unit, {min_sql}, {max_sql}, {value:f}, 'USD')"
        )
    return rows


def model_prices(
    provider: str,
    model: dict,
) -> tuple[Decimal | None, Decimal | None, Decimal | None] | None:
    live_prices = live_pricing_per_million(model)
    if live_prices is not None:
        input_price, output_price = live_prices
        return input_price, output_price, None

    static_prices = find_price(provider, model["id"])
    if static_prices is None:
        return None
    return tuple(
        Decimal(str(value)) if value is not None else None
        for value in static_prices
    )


def flat_price_rows(
    provider: str,
    model_id: str,
    prices: tuple[Decimal | None, Decimal | None, Decimal | None],
) -> list[str]:
    rows: list[str] = []
    units = (
        "per_1m_input_tokens",
        "per_1m_output_tokens",
        "per_1m_cached_input_tokens",
    )
    for unit, value in zip(units, prices, strict=True):
        if value is None:
            continue
        price_id = stable_uuid("ironrag", provider, "price", model_id, unit)
        rows.append(
            f"        ('{price_id}'::uuid, '{sql_escape(model_id)}', "
            f"'{unit}'::billing_unit, NULL, NULL, {value:f}, 'USD')"
        )
    return rows


def model_price_rows(provider: str, model: dict) -> list[str]:
    model_id = model["id"]
    ranged_rows = range_price_rows(provider, model_id)
    if ranged_rows:
        return ranged_rows
    prices = model_prices(provider, model)
    return [] if prices is None else flat_price_rows(provider, model_id, prices)


def append_catalog_insert(lines: list[str], catalog_rows: list[str]) -> None:
    if not catalog_rows:
        return
    lines.extend([
        "INSERT INTO ai_model_catalog (",
        "    id, provider_catalog_id, model_name, capability_kind,",
        "    modality_kind, lifecycle_state, metadata_json",
        ") VALUES",
        ",\n".join(catalog_rows),
        "ON CONFLICT (provider_catalog_id, model_name, capability_kind) DO NOTHING;",
        "",
    ])


def append_price_insert(
    lines: list[str],
    provider: str,
    catalog_id: str,
    price_rows: list[str],
) -> None:
    if not price_rows:
        return
    effective_from = EFFECTIVE_FROM_BY_PROVIDER.get(provider, EFFECTIVE_FROM)
    lines.extend([
        "INSERT INTO ai_price_catalog (",
        "    id, model_catalog_id, billing_unit, price_variant_key,",
        "    request_input_tokens_min, request_input_tokens_max,",
        "    unit_price, currency_code, effective_from, catalog_scope, workspace_id",
        ")",
        "SELECT",
        "    p.price_id, m.id, p.billing_unit, 'default',",
        "    p.request_input_tokens_min, p.request_input_tokens_max,",
        "    p.unit_price, p.currency_code,",
        f"    '{effective_from}'::timestamptz, 'system'::ai_price_catalog_scope, NULL",
        "FROM ai_model_catalog m",
        "JOIN (VALUES",
        ",\n".join(price_rows),
        (
            ") AS p(price_id, model_name, billing_unit, request_input_tokens_min, "
            "request_input_tokens_max, unit_price, currency_code)"
        ),
        "    ON p.model_name = m.model_name",
        f"WHERE m.provider_catalog_id = '{catalog_id}'::uuid",
        "ON CONFLICT DO NOTHING;",
        "",
    ])


def emit(
    provider: str,
    models: list[dict],
    capability_manifest: dict[str, object] | None = None,
) -> str:
    catalog_id = PROVIDER_CATALOG_IDS[provider]
    catalog_rows: list[str] = []
    price_rows: list[str] = []
    skipped_without_typed_capability = 0
    manifest = capability_manifest or {}

    for model in models:
        model_id = model["id"]
        signature = typed_model_signature(model, manifest.get(model_id))
        if signature is None:
            skipped_without_typed_capability += 1
            continue
        catalog_rows.append(model_catalog_row(catalog_id, provider, model_id, signature))
        price_rows.extend(model_price_rows(provider, model))

    lines: list[str] = [
        f"-- Auto-generated {provider} catalog + price seed.",
        f"-- Source: scripts/all-providers-seed-gen.py {provider}",
        "-- Idempotent: ON CONFLICT on natural-key indexes.",
        "",
    ]
    append_catalog_insert(lines, catalog_rows)
    append_price_insert(lines, provider, catalog_id, price_rows)

    if skipped_without_typed_capability:
        print(
            f"WARN: skipped {skipped_without_typed_capability} models "
            "without typed capability metadata",
            file=sys.stderr,
        )
    return "\n".join(lines)


def unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON object key")
        result[key] = value
    return result


def decode_json_object(encoded: str, max_decoded_bytes: int) -> dict[str, object]:
    max_encoded_bytes = ((max_decoded_bytes + 2) // 3) * 4
    if len(encoded.encode("utf-8")) > max_encoded_bytes:
        raise ValueError("encoded JSON payload is too large")
    decoded = base64.b64decode(encoded, validate=True)
    if len(decoded) > max_decoded_bytes:
        raise ValueError("decoded JSON payload is too large")
    if base64.b64encode(decoded).decode("ascii") != encoded:
        raise ValueError("non-canonical base64")
    payload = json.loads(decoded.decode("utf-8"), object_pairs_hook=unique_object)
    if not isinstance(payload, dict):
        raise ValueError("JSON payload is not an object")
    return payload


def load_model_capability_manifest(provider: str) -> dict[str, object]:
    encoded = os.environ.get(MODEL_CAPABILITY_ENV, "")
    if not encoded:
        return {}

    try:
        payload = decode_json_object(encoded, MAX_CAPABILITY_MANIFEST_BYTES)
        provider_entries = payload.get(provider, {})
        if not isinstance(provider_entries, dict):
            raise ValueError("provider capability manifest is not an object")
        if len(provider_entries) > MAX_CAPABILITY_MODELS:
            raise ValueError("provider capability manifest has too many models")
        return provider_entries
    except ValueError as error:
        raise SystemExit(f"{MODEL_CAPABILITY_ENV} is invalid") from error


def read_env_value(env_name: str, env_path: Path) -> str:
    value = os.environ.get(env_name, "")
    if value or not env_path.exists():
        return value
    for line in env_path.read_text().splitlines():
        if line.startswith(f"{env_name}="):
            return line.split("=", 1)[1]
    return ""


def load_api_key(provider: str) -> str | None:
    encoded_map = read_env_value(PROVIDER_API_KEYS_ENV, REPO_ROOT / ".env")
    if not encoded_map:
        return None

    try:
        provider_keys = decode_json_object(encoded_map, MAX_PROVIDER_KEY_MAP_BYTES)
        if len(provider_keys) > MAX_PROVIDER_KEYS:
            raise ValueError("provider key payload has too many entries")
        value = provider_keys.get(provider)
        if value is not None and not isinstance(value, str):
            raise ValueError("provider key is not a string")
        if value is not None and len(value.encode("utf-8")) > MAX_PROVIDER_KEY_BYTES:
            raise ValueError("provider key is too large")
    except ValueError as error:
        raise SystemExit(f"{PROVIDER_API_KEYS_ENV} is invalid") from error
    return value if value else None


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
    capability_manifest = load_model_capability_manifest(provider)
    print(f"-- {provider}: discovered {len(models)} models", file=sys.stderr)
    sys.stdout.write(emit(provider, models, capability_manifest))
    return 0


if __name__ == "__main__":
    sys.exit(main())
