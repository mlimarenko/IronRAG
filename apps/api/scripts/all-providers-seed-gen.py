#!/usr/bin/env python3
"""Generate runtime-discovery catalog updates for credentialed providers.

The authoritative clean-v1 provider, curated model, and static-price catalog is
owned by ``app/catalog_seed.rs`` and seeded by startup authority. This script
is retained only for operator-initiated discovery of additional provider models
and OpenRouter's authoritative USD prices.

Usage:
    IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=... python3 all-providers-seed-gen.py PROVIDER

Models are accepted only from typed provider metadata or the operator-supplied
IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64 manifest; unknown models are skipped
fail-closed. Router and local-provider prices are emitted only when the runtime
response supplies authoritative USD pricing. The generated SQL is never part of
the schema baseline.

UUIDs: stable UUID5 of "ironrag-<provider>-<scope>:<key>".
Idempotent: ON CONFLICT on the natural-key indexes.
"""

from __future__ import annotations

import base64
import json
import os
import sys
import uuid
from decimal import Decimal
from pathlib import Path

import requests

REPO_ROOT = Path(__file__).resolve().parents[3]
NS_DNS = uuid.NAMESPACE_DNS
EFFECTIVE_FROM = "2026-07-22T00:00:00Z"
EFFECTIVE_FROM_BY_PROVIDER: dict[str, str] = {}

PROVIDER_CATALOG_IDS = {
    "openai":       "00000000-0000-0000-0000-000000000101",
    "deepseek":     "00000000-0000-0000-0000-000000000102",
    "qwen":         "00000000-0000-0000-0000-000000000103",
    "ollama":       "00000000-0000-0000-0000-000000000104",
    "openrouter":   "00000000-0000-0000-0000-000000000105",
    "gptunnel":     "00000000-0000-0000-0000-000000000106",
    "routerai":     "00000000-0000-0000-0000-000000000107",
    "minimax":      "00000000-0000-0000-0000-000000000108",
    "anthropic":    "00000000-0000-0000-0000-000000000109",
    "gemini":       "00000000-0000-0000-0000-00000000010a",
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
    "anthropic":  "https://api.anthropic.com/v1/models",
    "gemini":     "https://generativelanguage.googleapis.com/v1beta/models",
}

MODEL_CAPABILITY_ENV = "IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64"
PROVIDER_API_KEYS_ENV = "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"
MAX_CAPABILITY_MANIFEST_BYTES = 4_194_304
MAX_CAPABILITY_MODELS = 100_000
MAX_PROVIDER_KEY_MAP_BYTES = 1_048_576
MAX_PROVIDER_KEYS = 256
MAX_PROVIDER_KEY_BYTES = 65_536
AI_BINDING_PURPOSES = frozenset({
    "extract_text",
    "extract_graph",
    "graph_entity_type",
    "embed_chunk",
    "query_compile",
    "query_rerank",
    "query_answer",
    "agent",
})


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


def parse_typed_signature(typed: dict) -> tuple[list[str], list[str]] | None:
    observed_input_modalities = typed.get("observedInputModalities")
    binding_purposes = typed.get("allowedBindingPurposes")
    if not isinstance(observed_input_modalities, list) or not observed_input_modalities:
        return None
    if any(not isinstance(modality, str) or not modality for modality in observed_input_modalities):
        return None
    if not isinstance(binding_purposes, list) or not binding_purposes:
        return None
    if any(
        not isinstance(purpose, str) or purpose not in AI_BINDING_PURPOSES
        for purpose in binding_purposes
    ):
        return None

    return sorted(set(observed_input_modalities)), sorted(set(binding_purposes))


def typed_model_signature(
    model: dict,
    manifest_entry: object | None = None,
) -> tuple[list[str], list[str]] | None:
    """Return observed facts and typed binding eligibility without name inference."""
    for typed in typed_metadata_candidates(model, manifest_entry):
        if isinstance(typed, dict) and (
            "observedInputModalities" in typed or "allowedBindingPurposes" in typed
        ):
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


def model_catalog_row(
    catalog_id: str,
    provider: str,
    model_id: str,
    signature: tuple[list[str], list[str]],
) -> str:
    observed_input_modalities, _binding_purposes = signature
    model_catalog_id = stable_uuid("ironrag", provider, "model", model_id)
    metadata = json.dumps(
        {
            "observedInputModalities": observed_input_modalities,
            "seedSource": "provider_catalog",
        },
        separators=(",", ":"),
    )
    return (
        f"    ('{model_catalog_id}'::uuid, '{catalog_id}'::uuid, "
        f"'{sql_escape(model_id)}', 'active'::ai_model_lifecycle_state, "
        f"'{sql_escape(metadata)}'::jsonb)"
    )


def model_binding_purpose_rows(
    provider: str,
    model_id: str,
    binding_purposes: list[str],
) -> list[str]:
    model_catalog_id = stable_uuid("ironrag", provider, "model", model_id)
    return [
        f"    ('{model_catalog_id}'::uuid, '{purpose}'::text)"
        for purpose in binding_purposes
    ]


def model_prices(
    _provider: str,
    model: dict,
) -> tuple[Decimal | None, Decimal | None, Decimal | None] | None:
    """The provider's live /models pricing is the only price source.

    A static fallback table used to live here and drifted away from the
    curated seed; discovered models without a live price now stay unpriced.
    """
    live_prices = live_pricing_per_million(model)
    if live_prices is None:
        return None
    input_price, output_price = live_prices
    return input_price, output_price, None


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
    prices = model_prices(provider, model)
    return [] if prices is None else flat_price_rows(provider, model_id, prices)


def append_catalog_insert(
    lines: list[str],
    catalog_rows: list[str],
    binding_purpose_rows: list[str],
) -> None:
    if not catalog_rows:
        return
    lines.extend([
        "WITH inserted_models AS (",
        "    INSERT INTO ai_model_catalog (",
        "        id, provider_catalog_id, model_name, lifecycle_state, metadata_json",
        "    ) VALUES",
        ",\n".join(catalog_rows),
        "    ON CONFLICT (provider_catalog_id, model_name) DO NOTHING",
        "    RETURNING id",
        ")",
        "INSERT INTO ai_model_binding_purpose (model_catalog_id, binding_purpose)",
        "SELECT purpose.model_catalog_id, purpose.binding_purpose::ai_binding_purpose",
        "FROM (VALUES",
        ",\n".join(binding_purpose_rows),
        ") AS purpose(model_catalog_id, binding_purpose)",
        "JOIN inserted_models ON inserted_models.id = purpose.model_catalog_id",
        "ON CONFLICT DO NOTHING;",
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
    binding_purpose_rows: list[str] = []
    price_rows: list[str] = []
    skipped_without_typed_eligibility = 0
    manifest = capability_manifest or {}

    for model in models:
        model_id = model["id"]
        signature = typed_model_signature(model, manifest.get(model_id))
        if signature is None:
            skipped_without_typed_eligibility += 1
            continue
        catalog_rows.append(model_catalog_row(catalog_id, provider, model_id, signature))
        binding_purpose_rows.extend(
            model_binding_purpose_rows(provider, model_id, signature[1])
        )
        price_rows.extend(model_price_rows(provider, model))

    lines: list[str] = [
        f"-- Auto-generated {provider} catalog + price seed.",
        f"-- Source: scripts/all-providers-seed-gen.py {provider}",
        "-- Idempotent: ON CONFLICT on natural-key indexes.",
        "",
    ]
    append_catalog_insert(lines, catalog_rows, binding_purpose_rows)
    append_price_insert(lines, provider, catalog_id, price_rows)

    if skipped_without_typed_eligibility:
        print(
            f"WARN: skipped {skipped_without_typed_eligibility} models "
            "without typed binding eligibility",
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
        raise ValueError("non-normalized base64")
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
