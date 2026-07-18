#!/usr/bin/env python3
"""End-to-end RAG pipeline smoke test parameterised per provider.

Runs the full alpha-relay scenario through ONE provider (or hybrid mode
where the chat roles use the provider and embedding falls back to
gptunnel only when the provider has no native embedding catalog row).

Usage:
    IRONRAG_ADMIN_PASSWORD=... \
    python3 multi-provider-e2e.py PROVIDER

The admin login/password may also be supplied through the
IRONRAG_UI_BOOTSTRAP_ADMIN_* variables written by the local install flow.

PROVIDER ∈ {openai, deepseek, qwen, gptunnel, openrouter, routerai, minimax}.

Embed fallback: if the chosen provider exposes no embedding catalog
row, the canonical embed_chunk binding is wired through the gptunnel
text-embedding-3-large model. Providers with native 3072-dim embedding
presets must use their own embedding lane.

Writes the full report to a private, per-run temporary directory. Set
IRONRAG_E2E_REPORT_DIR to retain it in an operator-controlled directory.
The smoke is provider-strict for query answering: fallback text can keep
the product usable, but it is not counted as a successful provider e2e.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import time
import uuid
from pathlib import Path

import requests

REPO_ROOT = Path(__file__).resolve().parents[3]
BASE = os.environ.get("IRONRAG_BASE_URL", "http://127.0.0.1:19000/v1")
ADMIN_LOGIN = (
    os.environ.get("IRONRAG_ADMIN_LOGIN")
    or os.environ.get("IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN")
    or "admin"
)
ADMIN_PASSWORD = (
    os.environ.get("IRONRAG_ADMIN_PASSWORD")
    or os.environ.get("IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD")
)

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

OPENAI_SMALL_CHAT_MODEL = "gpt-5.4-mini"
OPENAI_PRIMARY_CHAT_MODEL = "gpt-5.4"
OPENAI_LEGACY_SMALL_MODEL = "gpt-4.1-mini"
OPENAI_MULTIMODAL_SMALL_MODEL = "gpt-4o-mini"
OPENAI_MULTIMODAL_MODEL = "gpt-4o"
QWEN_PLUS_MODEL = "qwen3.6-plus"
QWEN_LEGACY_PLUS_MODEL = "qwen3.5-plus"
ROUTER_SMALL_OPENAI_MODEL = "openai/gpt-4o-mini"
ROUTER_SMALL_ANTHROPIC_MODEL = "anthropic/claude-3-haiku"
ROUTER_OPENAI_MODEL = "openai/gpt-4o"
ROUTER_ANTHROPIC_MODEL = "anthropic/claude-3.5-sonnet"

# Per-provider model preferences keyed by the public binding contract. The
# harness picks the first available preset for each canonical purpose.
PROVIDER_PROFILES: dict[str, dict[str, list[str]]] = {
    "openai": {
        "extract_graph": [
            OPENAI_SMALL_CHAT_MODEL,
            OPENAI_PRIMARY_CHAT_MODEL,
            OPENAI_LEGACY_SMALL_MODEL,
            OPENAI_MULTIMODAL_SMALL_MODEL,
        ],
        "embed_chunk": ["text-embedding-3-large", "text-embedding-3-small"],
        "query_compile": [
            OPENAI_SMALL_CHAT_MODEL,
            OPENAI_PRIMARY_CHAT_MODEL,
            OPENAI_LEGACY_SMALL_MODEL,
            OPENAI_MULTIMODAL_SMALL_MODEL,
        ],
        "query_answer": [OPENAI_PRIMARY_CHAT_MODEL, "gpt-4.1", OPENAI_MULTIMODAL_MODEL],
        "agent": [OPENAI_PRIMARY_CHAT_MODEL, "gpt-4.1", OPENAI_MULTIMODAL_MODEL],
        "extract_text": [
            OPENAI_MULTIMODAL_MODEL,
            OPENAI_MULTIMODAL_SMALL_MODEL,
            OPENAI_PRIMARY_CHAT_MODEL,
        ],
    },
    "deepseek": {
        "extract_graph": ["deepseek-v4-flash", "deepseek-chat", "deepseek-v3.2"],
        "embed_chunk": [],
        "query_compile": ["deepseek-v4-flash", "deepseek-chat", "deepseek-v3.2"],
        "query_answer": ["deepseek-v4-pro", "deepseek-reasoner", "deepseek-chat"],
        "agent": ["deepseek-v4-pro", "deepseek-reasoner", "deepseek-chat"],
        "extract_text": [],
    },
    "qwen": {
        "extract_graph": [QWEN_PLUS_MODEL, QWEN_LEGACY_PLUS_MODEL, "qwen-plus"],
        "embed_chunk": [],
        "query_compile": [QWEN_PLUS_MODEL, QWEN_LEGACY_PLUS_MODEL, "qwen-plus"],
        "query_answer": ["qwen3.6-max-preview", QWEN_PLUS_MODEL, QWEN_LEGACY_PLUS_MODEL],
        "agent": ["qwen3.6-max-preview", QWEN_PLUS_MODEL, QWEN_LEGACY_PLUS_MODEL],
        "extract_text": ["qwen3-vl-plus", "qwen-vl-max", "qwen-vl-plus"],
    },
    "gptunnel": {
        "extract_graph": [OPENAI_MULTIMODAL_SMALL_MODEL, OPENAI_LEGACY_SMALL_MODEL],
        "embed_chunk": ["text-embedding-3-large"],
        "query_compile": [OPENAI_MULTIMODAL_SMALL_MODEL, OPENAI_LEGACY_SMALL_MODEL],
        "query_answer": [OPENAI_MULTIMODAL_MODEL, OPENAI_PRIMARY_CHAT_MODEL],
        "agent": [OPENAI_MULTIMODAL_MODEL, OPENAI_PRIMARY_CHAT_MODEL],
        "extract_text": [OPENAI_MULTIMODAL_MODEL, "claude-4.6-sonnet", "gemini-3.1-pro"],
    },
    "openrouter": {
        "extract_graph": [ROUTER_SMALL_OPENAI_MODEL, ROUTER_SMALL_ANTHROPIC_MODEL],
        "embed_chunk": ["openai/text-embedding-3-large", "qwen/qwen3-embedding-8b"],
        "query_compile": [ROUTER_SMALL_OPENAI_MODEL, ROUTER_SMALL_ANTHROPIC_MODEL],
        "query_answer": [ROUTER_OPENAI_MODEL, ROUTER_ANTHROPIC_MODEL],
        "agent": [ROUTER_OPENAI_MODEL, ROUTER_ANTHROPIC_MODEL],
        "extract_text": [
            ROUTER_OPENAI_MODEL,
            ROUTER_ANTHROPIC_MODEL,
            "google/gemini-pro-vision",
        ],
    },
    "routerai": {
        "extract_graph": [ROUTER_SMALL_OPENAI_MODEL, ROUTER_SMALL_ANTHROPIC_MODEL],
        "embed_chunk": ["openai/text-embedding-3-large"],
        "query_compile": [ROUTER_SMALL_OPENAI_MODEL, ROUTER_SMALL_ANTHROPIC_MODEL],
        "query_answer": [ROUTER_OPENAI_MODEL, ROUTER_ANTHROPIC_MODEL],
        "agent": [ROUTER_OPENAI_MODEL, ROUTER_ANTHROPIC_MODEL],
        "extract_text": [ROUTER_OPENAI_MODEL, ROUTER_ANTHROPIC_MODEL],
    },
    "minimax": {
        "extract_graph": ["MiniMax-M3"],
        "embed_chunk": [],
        "query_compile": ["MiniMax-M3"],
        "query_answer": [
            "MiniMax-M3",
            "MiniMax-M2.7",
            "MiniMax-M2.7-highspeed",
            "MiniMax-M2.5",
            "MiniMax-M2.5-highspeed",
            "MiniMax-M2.1",
            "MiniMax-M2.1-highspeed",
            "MiniMax-M2",
        ],
        "agent": [
            "MiniMax-M3",
            "MiniMax-M2.7",
            "MiniMax-M2.7-highspeed",
            "MiniMax-M2.5",
            "MiniMax-M2.5-highspeed",
            "MiniMax-M2.1",
            "MiniMax-M2.1-highspeed",
            "MiniMax-M2",
        ],
        "extract_text": ["MiniMax-M3"],
    },
}

FALLBACK_PROVIDER = "gptunnel"
EMBED_FALLBACK_MODELS = ["text-embedding-3-large"]

EXTRACT_TEXT_FALLBACK_MODELS = [
    "gpt-4o",
    "claude-4.6-sonnet",
    "gemini-3.1-pro",
]

# Purposes that may fall back to gptunnel when the chosen provider has
# no native preset for that role. Embedding falls back only when the provider
# profile has no compatible native embedding preset. Document understanding
# falls back only when the profile explicitly declares no multimodal candidate.
# Providers that do declare one must use their own model.
FALLBACK_PURPOSES = {"embed_chunk", "extract_text"}


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
    p = REPO_ROOT / ".env"
    if p.exists():
        for line in p.read_text().splitlines():
            if line.startswith(f"{env_name}="):
                return line.split("=", 1)[1].strip()
    return None


def list_providers(s: requests.Session) -> list[dict]:
    pr = s.get(f"{BASE}/ai/providers")
    return pr.json() if isinstance(pr.json(), list) else pr.json().get("items", [])


def find_provider_catalog_id(providers: list[dict], provider: str) -> str:
    for p in providers:
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
    matching.sort(
        key=lambda preset: name_priority.get(
            catalog_by_id[preset["modelCatalogId"]]["modelName"], 999
        )
    )
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
            print(
                f"  ingest: ready={ready} queued={queue} "
                f"processing={running} failed={failed}",
                flush=True,
            )
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


def list_query_provider_calls(
    s: requests.Session,
    execution_id: str,
    providers: list[dict],
    catalog: list[dict],
) -> list[dict]:
    r = s.get(f"{BASE}/billing/executions/query_execution/{execution_id}/provider-calls")
    if r.status_code != 200:
        fail(f"list query provider calls status={r.status_code}", r.text[:500])
    provider_by_id = {item["id"]: item for item in providers}
    model_by_id = {item["id"]: item for item in catalog}
    calls = r.json()
    for call in calls:
        provider = provider_by_id.get(call.get("providerCatalogId"), {})
        model = model_by_id.get(call.get("modelCatalogId"), {})
        call["providerKind"] = provider.get("providerKind")
        call["modelName"] = model.get("modelName")
    return calls


ANSWER_PRODUCING_CALL_KINDS = {"query_answer", "query_agent"}


def provider_answer_call_present(
    provider_catalog_id: str,
    provider_calls: list[dict],
) -> bool:
    return any(
        call.get("providerCatalogId") == provider_catalog_id
        and call.get("callKind") in ANSWER_PRODUCING_CALL_KINDS
        and call.get("callState") == "completed"
        for call in provider_calls
    )


def verify(answer: dict) -> bool:
    text = json.dumps(answer)
    missing = [t for t in REQUIRED_TERMS if t not in text]
    if missing:
        return False
    return True


def write_report(provider: str, report: dict) -> Path:
    configured_directory = os.environ.get("IRONRAG_E2E_REPORT_DIR")
    if configured_directory:
        report_directory = Path(configured_directory).expanduser().resolve()
        report_directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    else:
        report_directory = Path(tempfile.mkdtemp(prefix="ironrag-provider-e2e-"))
    report_path = report_directory / f"multi-provider-e2e-{provider}.json"
    report_path.write_text(json.dumps(report, indent=2, default=str), encoding="utf-8")
    report_path.chmod(0o600)
    return report_path


def fallback_models_for_purpose(purpose: str) -> list[str]:
    if purpose == "extract_text":
        return EXTRACT_TEXT_FALLBACK_MODELS
    return EMBED_FALLBACK_MODELS


def ensure_fallback_credential(
    session: requests.Session,
    providers: list[dict],
    workspace_id: str,
    credential_id: str | None,
    provider_catalog_id: str | None,
) -> tuple[str, str]:
    if credential_id is not None and provider_catalog_id is not None:
        return credential_id, provider_catalog_id
    provider_catalog_id = find_provider_catalog_id(providers, FALLBACK_PROVIDER)
    credential_id = create_credential(
        session,
        FALLBACK_PROVIDER,
        workspace_id,
        provider_catalog_id,
    )
    return credential_id, provider_catalog_id


def resolve_binding_preset(
    session: requests.Session,
    provider: str,
    purpose: str,
    candidates: list[str],
    providers: list[dict],
    presets: list[dict],
    catalog: list[dict],
    provider_catalog_id: str,
    workspace_id: str,
    credential_id: str,
    fallback_credential_id: str | None,
    fallback_provider_catalog_id: str | None,
) -> tuple[dict, str, str | None, str | None]:
    preset = (
        pick_preset(presets, catalog, provider_catalog_id, candidates)
        if candidates
        else None
    )
    if preset is not None:
        return preset, credential_id, fallback_credential_id, fallback_provider_catalog_id
    if purpose not in FALLBACK_PURPOSES:
        fail(f"no preset for {provider}/{purpose}", candidates)

    fallback_credential_id, fallback_provider_catalog_id = ensure_fallback_credential(
        session,
        providers,
        workspace_id,
        fallback_credential_id,
        fallback_provider_catalog_id,
    )
    fallback_models = fallback_models_for_purpose(purpose)
    preset = pick_preset(
        presets,
        catalog,
        fallback_provider_catalog_id,
        fallback_models,
    )
    if preset is None:
        fail(f"fallback preset missing for {purpose}", fallback_models)
    print(f"  ↳ {purpose} falls back to {FALLBACK_PROVIDER}", flush=True)
    return (
        preset,
        fallback_credential_id,
        fallback_credential_id,
        fallback_provider_catalog_id,
    )


def create_bindings(
    session: requests.Session,
    provider: str,
    profile: dict[str, list[str]],
    providers: list[dict],
    presets: list[dict],
    catalog: list[dict],
    provider_catalog_id: str,
    workspace_id: str,
    library_id: str,
    credential_id: str,
) -> list[dict]:
    bindings: list[dict] = []
    fallback_credential_id = None
    fallback_provider_catalog_id = None
    for purpose, candidates in profile.items():
        preset, binding_credential_id, fallback_credential_id, fallback_provider_catalog_id = (
            resolve_binding_preset(
                session,
                provider,
                purpose,
                candidates,
                providers,
                presets,
                catalog,
                provider_catalog_id,
                workspace_id,
                credential_id,
                fallback_credential_id,
                fallback_provider_catalog_id,
            )
        )
        binding_id = create_binding(
            session,
            workspace_id,
            library_id,
            binding_credential_id,
            preset["id"],
            purpose,
        )
        bindings.append(
            {"purpose": purpose, "presetId": preset["id"], "bindingId": binding_id}
        )
    return bindings


def summarized_provider_calls(provider_calls: list[dict]) -> list[dict]:
    fields = ("providerKind", "modelName", "callKind", "callState")
    return [
        {field: call.get(field) for field in fields}
        for call in provider_calls
    ]


def build_report(
    provider: str,
    provider_catalog_id: str,
    workspace_id: str,
    library_id: str,
    credential_id: str,
    document_id: str,
    bindings: list[dict],
    answer: dict,
    provider_calls: list[dict],
) -> dict:
    response_turn = answer.get("responseTurn") or {}
    provider_answer_ok = provider_answer_call_present(
        provider_catalog_id,
        provider_calls,
    )
    return {
        "provider": provider,
        "workspaceId": workspace_id,
        "libraryId": library_id,
        "credentialId": credential_id,
        "documentId": document_id,
        "bindings": bindings,
        "answerExcerpt": response_turn.get("contentText"),
        "verificationState": answer.get("verificationState"),
        "executionId": response_turn.get("executionId"),
        "providerCalls": summarized_provider_calls(provider_calls),
        "providerAnswerCallPresent": provider_answer_ok,
        "passed": verify(answer) and provider_answer_ok,
    }


def report_result(provider: str, report: dict, report_path: Path) -> int:
    if report["passed"]:
        print(f"\nPASS [{provider}] — {report_path}", flush=True)
        return 0
    if not report["providerAnswerCallPresent"]:
        print(
            f"\nFAIL-DEGRADED [{provider}] — query answer used fallback provider/runtime",
            flush=True,
        )
        print(f"  provider calls: {report['providerCalls']}", flush=True)
        print(f"  report: {report_path}", flush=True)
        return 1

    print(f"\nFAIL-VERIFY [{provider}] — required terms missing", flush=True)
    print(f"  excerpt: {report['answerExcerpt']}", flush=True)
    return 1


def run_provider_smoke(provider: str) -> tuple[dict, Path]:
    profile = PROVIDER_PROFILES[provider]
    session = requests.Session()
    session.headers.update({"Content-Type": "application/json"})

    login(session)
    providers = list_providers(session)
    provider_catalog_id = find_provider_catalog_id(providers, provider)
    workspace_id = create_workspace(session, provider)
    library_id = create_library(session, workspace_id)
    credential_id = create_credential(
        session,
        provider,
        workspace_id,
        provider_catalog_id,
    )
    presets = list_presets(session)
    catalog = list_catalog(session)
    bindings = create_bindings(
        session,
        provider,
        profile,
        providers,
        presets,
        catalog,
        provider_catalog_id,
        workspace_id,
        library_id,
        credential_id,
    )

    document_id = upload_doc(session, library_id)
    print(f"  documentId={document_id}", flush=True)
    wait_ingest(session, library_id)
    answer = run_query(session, workspace_id, library_id)
    execution_id = (answer.get("responseTurn") or {}).get("executionId")
    provider_calls = (
        list_query_provider_calls(session, execution_id, providers, catalog)
        if execution_id
        else []
    )
    report = build_report(
        provider,
        provider_catalog_id,
        workspace_id,
        library_id,
        credential_id,
        document_id,
        bindings,
        answer,
        provider_calls,
    )
    return report, write_report(provider, report)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: multi-provider-e2e.py PROVIDER", file=sys.stderr)
        return 2
    provider = sys.argv[1]
    if provider not in PROVIDER_PROFILES:
        fail(f"unknown provider: {provider}")
    report, report_path = run_provider_smoke(provider)
    return report_result(provider, report, report_path)


if __name__ == "__main__":
    sys.exit(main())
