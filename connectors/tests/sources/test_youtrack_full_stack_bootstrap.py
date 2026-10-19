from copy import deepcopy
from pathlib import Path
from typing import Any

import bootstrap_full_stack
import pytest
from bootstrap_full_stack import Config, _has_exact_runtime_scope
from prepare_connector_runtime import RuntimeContract, _routing_yaml

LIBRARY_ID = "00000000-0000-0000-0000-000000000123"
WORKSPACE_ID = "00000000-0000-0000-0000-000000000456"


def _identity() -> dict[str, Any]:
    return {
        "principal": {"principalKind": "api_token"},
        "user": None,
        "workspaceMemberships": [],
        "effectiveGrants": [
            {
                "resourceKind": "library",
                "resourceId": LIBRARY_ID,
                "permissionKind": "library_read",
            },
            {
                "resourceKind": "library",
                "resourceId": LIBRARY_ID,
                "permissionKind": "library_write",
            },
        ],
    }


def test_exact_runtime_scope_accepts_only_required_library_grants() -> None:
    assert _has_exact_runtime_scope(_identity(), LIBRARY_ID)


@pytest.mark.parametrize(
    "extra_grant",
    [
        {
            "resourceKind": "library",
            "resourceId": LIBRARY_ID,
            "permissionKind": "library_admin",
        },
        {
            "resourceKind": "library",
            "resourceId": "00000000-0000-0000-0000-000000000999",
            "permissionKind": "library_read",
        },
        {
            "resourceKind": "system",
            "resourceId": "00000000-0000-0000-0000-000000000000",
            "permissionKind": "system_admin",
        },
    ],
)
def test_exact_runtime_scope_rejects_any_extra_grant(extra_grant: dict[str, str]) -> None:
    identity = _identity()
    identity["effectiveGrants"].append(extra_grant)

    assert not _has_exact_runtime_scope(identity, LIBRARY_ID)


def test_exact_runtime_scope_rejects_duplicate_grant() -> None:
    identity = _identity()
    identity["effectiveGrants"].append(deepcopy(identity["effectiveGrants"][0]))

    assert not _has_exact_runtime_scope(identity, LIBRARY_ID)


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("principal", {"principalKind": "user"}),
        ("user", {"id": "00000000-0000-0000-0000-000000000456"}),
        ("workspaceMemberships", [{"workspaceId": "00000000-0000-0000-0000-000000000789"}]),
    ],
)
def test_exact_runtime_scope_rejects_non_token_identity_context(field: str, value: object) -> None:
    identity = _identity()
    identity[field] = value

    assert not _has_exact_runtime_scope(identity, LIBRARY_ID)


def test_bootstrap_runtime_contains_slugs_for_routing_and_ids_for_scope_assertions(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    config = Config(
        base_url="http://127.0.0.1:18080",
        admin_login="fixture-admin",
        admin_password="synthetic-password",
        admin_token="inrg_synthetic-bootstrap-token",
        mock_provider_url="http://mock-provider:8080/v1",
        mock_provider_api_key=None,
        workspace_slug="default",
        library_slug="youtrack-e2e",
        runtime_file=tmp_path / "runtime.json",
        ready_timeout_seconds=1,
        request_timeout_seconds=1,
    )
    captured: dict[str, str] = {}

    monkeypatch.setattr(bootstrap_full_stack, "_read_existing_runtime", lambda _: {})
    monkeypatch.setattr(bootstrap_full_stack, "_wait_for_bootstrap_status", lambda *_: {})
    monkeypatch.setattr(bootstrap_full_stack, "_authenticate_admin", lambda *_: None)
    monkeypatch.setattr(bootstrap_full_stack, "_get_workspace", lambda *_: {"id": WORKSPACE_ID})
    monkeypatch.setattr(
        bootstrap_full_stack,
        "_get_or_create_library",
        lambda *_: {"id": LIBRARY_ID},
    )
    monkeypatch.setattr(
        bootstrap_full_stack,
        "_get_or_create_account",
        lambda *_: {"id": "00000000-0000-0000-0000-000000000789"},
    )
    monkeypatch.setattr(bootstrap_full_stack, "_verify_models", lambda *_: None)
    monkeypatch.setattr(bootstrap_full_stack, "_ensure_bindings", lambda *_: None)
    monkeypatch.setattr(bootstrap_full_stack, "_verify_ingestion_ready", lambda *_: None)
    monkeypatch.setattr(
        bootstrap_full_stack,
        "_runtime_token_is_reusable",
        lambda values, *_: (
            values.get("IRONRAG_API_TOKEN")
            if values.get("IRONRAG_WORKSPACE_SLUG") == "default"
            and values.get("IRONRAG_LIBRARY_SLUG") == "youtrack-e2e"
            else None
        ),
    )
    monkeypatch.setattr(
        bootstrap_full_stack,
        "_mint_runtime_token",
        lambda *_: "irt_synthetic-runtime-token",
    )
    monkeypatch.setattr(
        bootstrap_full_stack,
        "_atomic_write_runtime",
        lambda _, values: captured.update(values),
    )

    bootstrap_full_stack.bootstrap(config)

    assert captured == {
        "IRONRAG_BASE_URL": config.base_url,
        "IRONRAG_API_TOKEN": "irt_synthetic-runtime-token",
        "IRONRAG_WORKSPACE_SLUG": "default",
        "IRONRAG_LIBRARY_SLUG": "youtrack-e2e",
        "IRONRAG_WORKSPACE_ID": WORKSPACE_ID,
        "IRONRAG_LIBRARY_ID": LIBRARY_ID,
    }


def test_generated_routing_uses_friendly_catalog_ref_only() -> None:
    runtime = RuntimeContract(
        base_url="http://127.0.0.1:18080",
        api_token="irt_synthetic-runtime-token",
        workspace_slug="default",
        library_slug="youtrack-e2e",
        workspace_id=WORKSPACE_ID,
        library_id=LIBRARY_ID,
    )

    routing = _routing_yaml(runtime).decode()

    assert "library: 'default/youtrack-e2e'" in routing
    assert "workspace:" not in routing
    assert WORKSPACE_ID not in routing
    assert LIBRARY_ID not in routing
