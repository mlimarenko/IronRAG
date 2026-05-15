#!/usr/bin/env python3
"""End-to-end RAG pipeline smoke test parameterised per provider.

Runs the full alpha-relay scenario through ONE provider (or hybrid mode
where the chat roles use the provider and embedding falls back to
gptunnel only when the provider has no native embedding catalog row).

Usage:
    IRONRAG_ADMIN_PASSWORD=... \
    python3 multi-provider-e2e.py PROVIDER

PROVIDER ∈ {openai, deepseek, qwen, gptunnel, openrouter, routerai}.

Embed fallback: if the chosen provider exposes no embedding catalog
row, the embed_chunk + query_retrieve bindings are wired through the
gptunnel text-embedding-3-large model. Providers with native 3072-dim
embedding presets must use their own embedding lane.

Writes /tmp/multi-provider-e2e-<provider>.json with the full report.
"""

from __future__ import annotations

import json
import os
import sys
import time
import uuid
from pathlib import Path

import requests

BASE = os.environ.get("IRONRAG_BASE_URL", "http://127.0.0.1:19000/v1")
ADMIN_LOGIN = os.environ.get("IRONRAG_ADMIN_LOGIN", "admin")
ADMIN_PASSWORD = os.environ.get("IRONRAG_ADMIN_PASSWORD")

ALPHA_RELAY_TEXT = (
    "Alpha Relay runbook.\n"
    "Alpha Relay retentionWindowDays = 37.\n"
    "Incident escalation target is Delta Ops Queue.\n"
    "Canonical endpoint: https://alpha.example.test/relay.\n"
)
ALPHA_RELAY_QUESTION = (
    "What retention window does Alpha Relay use, and what is its "
    "incident escalation target?"
)
REQUIRED_TERMS = ["37", "Delta Ops Queue"]

# Per-provider chat / embedding model preferences. The harness picks the
# first available preset per role, falling back to gptunnel for embedding
# when the provider has none.
PROVIDER_PROFILES: dict[str, dict[str, list[str]]] = {
    "openai": {
        "chat":      ["gpt-5.4-mini", "gpt-5.4", "gpt-4.1-mini", "gpt-4o-mini"],
        "answer":    ["gpt-5.4", "gpt-4.1", "gpt-4o"],
        "vision":    ["gpt-4o", "gpt-4o-mini", "gpt-5.4"],
        "embedding": ["text-embedding-3-large", "text-embedding-3-small"],
    },
    "deepseek": {
        "chat":      ["deepseek-v4-flash", "deepseek-chat", "deepseek-v3.2"],
        "answer":    ["deepseek-v4-pro", "deepseek-reasoner", "deepseek-chat"],
        "vision":    [],  # deepseek has no vision model; fall back to gptunnel
        "embedding": [],
    },
    "qwen": {
        "chat":      ["qwen3.6-plus", "qwen3.5-plus", "qwen-plus"],
        "answer":    ["qwen3.6-max-preview", "qwen3.6-plus", "qwen3.5-plus"],
        "vision":    ["qwen3-vl-plus", "qwen-vl-max", "qwen-vl-plus"],
        "embedding": [],  # qwen native embedding presets are not part of this smoke profile
    },
    "gptunnel": {
        "chat":      ["gpt-4o-mini", "gpt-4.1-mini"],
        "answer":    ["gpt-4o", "gpt-5.4"],
        "vision":    ["gpt-4o", "claude-4.6-sonnet", "gemini-3.1-pro"],
        "embedding": ["text-embedding-3-large"],
    },
    "openrouter": {
        "chat":      ["openai/gpt-4o-mini", "anthropic/claude-3-haiku"],
        "answer":    ["openai/gpt-4o", "anthropic/claude-3.5-sonnet"],
        "vision":    ["openai/gpt-4o", "anthropic/claude-3.5-sonnet", "google/gemini-pro-vision"],
        "embedding": ["openai/text-embedding-3-large", "qwen/qwen3-embedding-8b"],
    },
    "routerai": {
        "chat":      ["openai/gpt-4o-mini", "anthropic/claude-3-haiku"],
        "answer":    ["openai/gpt-4o", "anthropic/claude-3.5-sonnet"],
        "vision":    ["openai/gpt-4o", "anthropic/claude-3.5-sonnet"],
        "embedding": ["openai/text-embedding-3-large"],
    },
}

EMBED_FALLBACK_PROVIDER = "gptunnel"
EMBED_FALLBACK_MODELS = ["text-embedding-3-large"]

VISION_FALLBACK_PROVIDER = "gptunnel"
VISION_FALLBACK_MODELS = ["gpt-4o", "claude-4.6-sonnet", "gemini-3.1-pro"]

# Purposes that may fall back to gptunnel when the chosen provider has
# no native preset for that role. Embedding falls back only when the provider
# profile has no compatible native embedding preset. Vision falls back ONLY when the
# provider profile explicitly opts in by leaving its `vision` list
# empty (e.g. deepseek, which has no vision API). Providers that DO
# expose vision must use their own model — silent cross-provider
# routing is forbidden.
FALLBACK_PURPOSES = {"embed_chunk", "query_retrieve", "vision"}


def fail(msg: str, ctx=None) -> "no return":
    sys.stderr.write(f"FAIL: {msg}\n")
    if ctx is not None:
        sys.stderr.write(f"      {json.dumps(ctx, default=str, indent=2)[:1500]}\n")
    sys.exit(1)


def step(msg: str) -> None:
    print(f"[step] {msg}", flush=True)


def login(s: requests.Session) -> None:
    if not ADMIN_PASSWORD:
        fail("IRONRAG_ADMIN_PASSWORD is required")
    step("login admin")
    r = s.post(f"{BASE}/iam/session/login",
               json={"login": ADMIN_LOGIN, "password": ADMIN_PASSWORD})
    if r.status_code != 200:
        fail(f"login failed status={r.status_code}", r.text[:500])


def load_api_key(provider: str) -> str | None:
    env_name = f"IRONRAG_{provider.upper()}_API_KEY"
    val = os.environ.get(env_name)
    if val:
        return val
    p = Path("/home/leader/sources/IronRAG/ironrag/.env")
    if p.exists():
        for line in p.read_text().splitlines():
            if line.startswith(f"{env_name}="):
                return line.split("=", 1)[1].strip()
    return None


def find_provider_catalog_id(s: requests.Session, provider: str) -> str:
    pr = s.get(f"{BASE}/ai/providers")
    items = pr.json() if isinstance(pr.json(), list) else pr.json().get("items", [])
    for p in items:
        if p.get("providerKind") == provider:
            return p["id"]
    fail(f"provider not in catalog: {provider}")


def create_workspace(s: requests.Session, suffix: str) -> str:
    step("create workspace")
    name = f"e2e-{suffix}-{uuid.uuid4().hex[:8]}"
    r = s.post(f"{BASE}/catalog/workspaces", json={"displayName": name})
    if r.status_code not in (200, 201):
        fail(f"create workspace status={r.status_code}", r.text[:500])
    return r.json()["id"]


def create_library(s: requests.Session, workspace_id: str) -> str:
    step("create library")
    name = f"alpha-relay-{uuid.uuid4().hex[:8]}"
    r = s.post(f"{BASE}/catalog/workspaces/{workspace_id}/libraries",
               json={"displayName": name})
    if r.status_code not in (200, 201):
        fail(f"create library status={r.status_code}", r.text[:500])
    return r.json()["id"]


def create_credential(s: requests.Session, provider: str, workspace_id: str,
                      provider_catalog_id: str) -> str:
    step(f"create {provider} credential")
    api_key = load_api_key(provider)
    if not api_key:
        fail(f"missing IRONRAG_{provider.upper()}_API_KEY")
    r = s.post(f"{BASE}/ai/credentials", json={
        "providerCatalogId": provider_catalog_id,
        "label": f"{provider}-e2e",
        "scopeKind": "workspace",
        "workspaceId": workspace_id,
        "apiKey": api_key,
    })
    if r.status_code not in (200, 201):
        fail(f"create credential status={r.status_code}", r.text[:500])
    return r.json()["id"]


def list_presets(s: requests.Session) -> list[dict]:
    r = s.get(f"{BASE}/ai/model-presets")
    if r.status_code != 200:
        fail(f"list presets status={r.status_code}", r.text[:500])
    return r.json() if isinstance(r.json(), list) else r.json().get("items", [])


def list_catalog(s: requests.Session) -> list[dict]:
    r = s.get(f"{BASE}/ai/models")
    return r.json() if isinstance(r.json(), list) else r.json().get("items", [])


def pick_preset(presets: list[dict], catalog: list[dict],
                provider_catalog_id: str, candidates: list[str]) -> dict | None:
    """Find a preset where modelCatalog.providerCatalogId matches AND
    model_name is in candidates (in priority order)."""
    valid_model_ids = {m["id"] for m in catalog
                       if m.get("providerCatalogId") == provider_catalog_id
                       and m.get("modelName") in candidates}
    if not valid_model_ids:
        return None
    # Sort by candidate priority
    name_priority = {n: i for i, n in enumerate(candidates)}
    catalog_by_id = {m["id"]: m for m in catalog}
    matching = [p for p in presets if p["modelCatalogId"] in valid_model_ids]
    if not matching:
        return None
    matching.sort(key=lambda p: name_priority.get(catalog_by_id[p["modelCatalogId"]]["modelName"], 999))
    return matching[0]


def create_binding(s: requests.Session, workspace_id: str, library_id: str,
                   credential_id: str, preset_id: str, purpose: str) -> str:
    step(f"create binding {purpose} → preset {preset_id}")
    r = s.post(f"{BASE}/ai/bindings", json={
        "bindingPurpose": purpose,
        "scopeKind": "library",
        "workspaceId": workspace_id,
        "libraryId": library_id,
        "providerCredentialId": credential_id,
        "modelPresetId": preset_id,
    })
    if r.status_code in (200, 201):
        return r.json()["id"]
    # `embed_chunk` and `query_retrieve` are auto-paired by the canonical
    # vector-counterpart sync (see ai_catalog_service::sync_vector_counterpart_binding):
    # creating either implicitly upserts the partner with the same model.
    # When that happens the explicit second POST returns 409. Treat that as
    # success and resolve the existing binding id.
    if r.status_code == 409 and purpose in {"embed_chunk", "query_retrieve"}:
        listing = s.get(f"{BASE}/ai/bindings",
                        params={"libraryId": library_id, "scopeKind": "library"})
        if listing.status_code == 200:
            items = listing.json() if isinstance(listing.json(), list) else listing.json().get("items", [])
            for b in items:
                if b.get("libraryId") == library_id and b.get("bindingPurpose") == purpose:
                    print(f"  ↳ {purpose} reused auto-paired binding {b['id']}", flush=True)
                    return b["id"]
    fail(f"create binding {purpose} status={r.status_code}", r.text[:500])


def upload_doc(s: requests.Session, library_id: str) -> str:
    step("upload alpha-relay.md")
    files = {"file": ("alpha-relay.md", ALPHA_RELAY_TEXT.encode("utf-8"), "text/markdown")}
    saved_ct = s.headers.pop("Content-Type", None)
    try:
        r = s.post(f"{BASE}/content/documents/upload",
                   data={"library_id": library_id, "external_key": "alpha-relay.md"},
                   files=files)
    finally:
        if saved_ct is not None:
            s.headers["Content-Type"] = saved_ct
    if r.status_code not in (200, 201, 202):
        fail(f"upload doc status={r.status_code}", r.text[:500])
    return r.json().get("document", {}).get("document", {}).get("id") or r.json().get("id")


def wait_ingest(s: requests.Session, library_id: str, timeout_s: int = 240) -> dict:
    step("wait ingest readable")
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        r = s.get(f"{BASE}/ops/libraries/{library_id}/dashboard")
        if r.status_code == 200:
            d = r.json()
            metrics = d.get("documentMetrics", {})
            ready = metrics.get("ready", 0)
            queue = metrics.get("queued", 0)
            running = metrics.get("processing", 0)
            failed = metrics.get("failed", 0)
            print(f"  ingest: ready={ready} queued={queue} processing={running} failed={failed}", flush=True)
            if ready >= 1 and queue == 0 and running == 0:
                return d
            if failed > 0:
                fail(f"ingest failed (failed={failed})", d)
        time.sleep(5)
    fail("ingest timeout")


def run_query(s: requests.Session, workspace_id: str, library_id: str) -> dict:
    step("create query session")
    r = s.post(f"{BASE}/query/sessions",
               json={"workspaceId": workspace_id, "libraryId": library_id})
    if r.status_code not in (200, 201):
        fail(f"create session status={r.status_code}", r.text[:500])
    session_id = r.json()["id"]

    step("submit turn")
    r = s.post(f"{BASE}/query/sessions/{session_id}/turns",
               json={"contentText": ALPHA_RELAY_QUESTION},
               timeout=180)
    if r.status_code not in (200, 201):
        fail(f"submit turn status={r.status_code}", r.text[:1000])
    return r.json()


def verify(answer: dict) -> bool:
    text = json.dumps(answer)
    missing = [t for t in REQUIRED_TERMS if t not in text]
    if missing:
        return False
    return True


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: multi-provider-e2e.py PROVIDER", file=sys.stderr)
        return 2
    provider = sys.argv[1]
    if provider not in PROVIDER_PROFILES:
        fail(f"unknown provider: {provider}")
    profile = PROVIDER_PROFILES[provider]

    s = requests.Session()
    s.headers.update({"Content-Type": "application/json"})

    login(s)
    pcid = find_provider_catalog_id(s, provider)
    workspace_id = create_workspace(s, provider)
    library_id = create_library(s, workspace_id)
    credential_id = create_credential(s, provider, workspace_id, pcid)
    presets = list_presets(s)
    catalog = list_catalog(s)

    bindings = []
    role_to_candidates = {
        "extract_graph": profile["chat"],
        "query_compile": profile["chat"],
        "query_answer":  profile["answer"],
        "embed_chunk":     profile["embedding"],
        "query_retrieve":  profile["embedding"],
        "vision":          profile["vision"],
    }
    fallback_credential_id = None
    fallback_provider_catalog_id = None
    for purpose, candidates in role_to_candidates.items():
        preset = pick_preset(presets, catalog, pcid, candidates) if candidates else None
        cred = credential_id
        if preset is None:
            # Fall back to gptunnel when the chosen provider has no native
            # preset for embedding or vision. Embedding fallback keeps the
            # smoke runnable for providers without a native embedding lane;
            # vision binding is required by the extract_graph stage.
            if purpose not in FALLBACK_PURPOSES:
                fail(f"no preset for {provider}/{purpose}", candidates)
            if fallback_credential_id is None:
                fallback_provider_catalog_id = find_provider_catalog_id(s, EMBED_FALLBACK_PROVIDER)
                fallback_credential_id = create_credential(
                    s, EMBED_FALLBACK_PROVIDER, workspace_id, fallback_provider_catalog_id,
                )
            fallback_models = (
                VISION_FALLBACK_MODELS if purpose == "vision" else EMBED_FALLBACK_MODELS
            )
            preset = pick_preset(presets, catalog, fallback_provider_catalog_id,
                                 fallback_models)
            if preset is None:
                fail(f"fallback preset missing for {purpose}", fallback_models)
            cred = fallback_credential_id
            print(f"  ↳ {purpose} falls back to {EMBED_FALLBACK_PROVIDER}", flush=True)
        bid = create_binding(s, workspace_id, library_id, cred, preset["id"], purpose)
        bindings.append({"purpose": purpose, "presetId": preset["id"], "bindingId": bid})

    document_id = upload_doc(s, library_id)
    print(f"  documentId={document_id}", flush=True)
    wait_ingest(s, library_id)
    answer = run_query(s, workspace_id, library_id)
    passed = verify(answer)
    out = {
        "provider": provider,
        "workspaceId": workspace_id,
        "libraryId": library_id,
        "credentialId": credential_id,
        "documentId": document_id,
        "bindings": bindings,
        "answerExcerpt": (answer.get("responseTurn") or {}).get("contentText"),
        "verificationState": answer.get("verificationState"),
        "executionId": (answer.get("responseTurn") or {}).get("executionId"),
        "passed": passed,
    }
    out_path = Path(f"/tmp/multi-provider-e2e-{provider}.json")
    out_path.write_text(json.dumps(out, indent=2, default=str))
    if passed:
        print(f"\nPASS [{provider}] — {out_path}", flush=True)
        return 0
    else:
        print(f"\nFAIL-VERIFY [{provider}] — required terms missing", flush=True)
        print(f"  excerpt: {out['answerExcerpt']}", flush=True)
        return 1


if __name__ == "__main__":
    sys.exit(main())
