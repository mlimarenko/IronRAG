#!/usr/bin/env python3
"""Provision the real IronRAG side of the isolated full-stack E2E fixture.

Required environment variables:

* ``IRONRAG_E2E_URL`` -- host-visible IronRAG service root.
* ``IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN`` and
  ``IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`` -- fixture admin credentials.
* ``IRONRAG_E2E_MOCK_PROVIDER_URL`` -- Docker-internal mock provider URL.  A
  trailing ``/v1`` is added when absent.
* ``IRONRAG_E2E_RUNTIME_FILE`` -- absolute output path.  ``.json`` writes a
  JSON environment map; every other suffix writes a dotenv file.

``IRONRAG_E2E_TOKEN`` is optional.  When the server was bootstrapped through
``IRONRAG_UI_BOOTSTRAP_ADMIN_*``, it must be that environment bootstrap's
system API token.  On an unclaimed server the helper instead performs the
interactive bootstrap with the mock Ollama provider and authenticates with the
resulting session cookie.

The helper deliberately never prints response bodies, credentials, cookies,
or tokens.  Its runtime artifact contains a least-privilege, library-scoped
token, human-readable catalog slugs, and the internal IDs needed by the E2E
assertions.  It is atomically written with mode 0600.
"""

from __future__ import annotations

import contextlib
import http.cookiejar
import ipaddress
import json
import os
import re
import secrets
import shlex
import stat
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

OLLAMA_PROVIDER_ID = "00000000-0000-0000-0000-000000000104"
OLLAMA_CHAT_MODEL_ID = "00000000-0000-0000-0000-000000000241"
OLLAMA_EMBEDDING_MODEL_ID = "00000000-0000-0000-0000-000000000242"
ACCOUNT_LABEL = "youtrack-e2e-ollama-mock"
TOKEN_LABEL = "youtrack-e2e-connector"
MAX_RESPONSE_BYTES = 4 * 1024 * 1024
MAX_RUNTIME_BYTES = 64 * 1024
SLUG_PATTERN = re.compile(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$")

BINDING_MODELS = {
    "extract_graph": OLLAMA_CHAT_MODEL_ID,
    "embed_chunk": OLLAMA_EMBEDDING_MODEL_ID,
    "query_retrieve": OLLAMA_EMBEDDING_MODEL_ID,
    "query_compile": OLLAMA_CHAT_MODEL_ID,
    "query_answer": OLLAMA_CHAT_MODEL_ID,
}
BOOTSTRAP_ONLY_KEYS = {
    "IRONRAG_UI_BOOTSTRAP_ADMIN_API_TOKEN",
    "IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN",
    "IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD",
    "IRONRAG_CREDENTIAL_MASTER_KEY",
}


class BootstrapError(RuntimeError):
    """Expected, already-redacted fixture bootstrap failure."""


class ApiStatusError(BootstrapError):
    """An IronRAG API response with an unexpected HTTP status."""

    def __init__(self, method: str, path: str, status_code: int) -> None:
        self.status_code = status_code
        super().__init__(f"IronRAG {method} {path} returned HTTP {status_code}")


class ApiTransportError(BootstrapError):
    """A redacted IronRAG transport failure."""


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: Any,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> urllib.request.Request | None:
        del req, fp, code, msg, headers, newurl
        return None


@dataclass(frozen=True)
class Config:
    base_url: str
    admin_login: str
    admin_password: str
    admin_token: str | None
    mock_provider_url: str
    mock_provider_api_key: str | None
    workspace_slug: str
    library_slug: str
    runtime_file: Path
    ready_timeout_seconds: float
    request_timeout_seconds: float

    @classmethod
    def from_environment(cls) -> Config:
        base_url = _normalise_service_url(_required_env("IRONRAG_E2E_URL"))
        _require_safe_target(base_url)

        admin_login = _required_env("IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN")
        admin_password = _required_env("IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD", strip=False)
        if any(character in admin_login for character in "\r\n\0"):
            raise BootstrapError(
                "IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN contains a forbidden control character"
            )
        if len(admin_password.strip()) < 8 or any(
            character in admin_password for character in "\r\n\0"
        ):
            raise BootstrapError(
                "IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD must be at least eight characters "
                "and contain no line breaks"
            )

        workspace_slug = os.environ.get("IRONRAG_E2E_WORKSPACE_SLUG", "default").strip()
        library_slug = os.environ.get("IRONRAG_E2E_LIBRARY_SLUG", "youtrack-e2e").strip()
        for variable, value in (
            ("IRONRAG_E2E_WORKSPACE_SLUG", workspace_slug),
            ("IRONRAG_E2E_LIBRARY_SLUG", library_slug),
        ):
            if not SLUG_PATTERN.fullmatch(value):
                raise BootstrapError(f"{variable} must be a lowercase DNS-style slug")

        runtime_raw = _required_env("IRONRAG_E2E_RUNTIME_FILE")
        if any(character in runtime_raw for character in "\r\n\0"):
            raise BootstrapError("IRONRAG_E2E_RUNTIME_FILE contains a forbidden control character")
        runtime_file = Path(runtime_raw).expanduser()
        if not runtime_file.is_absolute():
            raise BootstrapError("IRONRAG_E2E_RUNTIME_FILE must be an absolute tempfile path")
        if not runtime_file.parent.is_dir():
            raise BootstrapError("IRONRAG_E2E_RUNTIME_FILE parent directory does not exist")

        e2e_token = _optional_secret_env("IRONRAG_E2E_TOKEN")
        bootstrap_token = _optional_secret_env("IRONRAG_UI_BOOTSTRAP_ADMIN_API_TOKEN")
        if e2e_token is not None and bootstrap_token is not None and e2e_token != bootstrap_token:
            raise BootstrapError(
                "IRONRAG_E2E_TOKEN and IRONRAG_UI_BOOTSTRAP_ADMIN_API_TOKEN must match"
            )

        return cls(
            base_url=base_url,
            admin_login=admin_login,
            admin_password=admin_password,
            admin_token=e2e_token or bootstrap_token,
            mock_provider_url=_normalise_mock_provider_url(
                _required_env("IRONRAG_E2E_MOCK_PROVIDER_URL")
            ),
            mock_provider_api_key=_optional_secret_env("IRONRAG_E2E_MOCK_PROVIDER_API_KEY"),
            workspace_slug=workspace_slug,
            library_slug=library_slug,
            runtime_file=runtime_file,
            ready_timeout_seconds=_bounded_float_env(
                "IRONRAG_E2E_READY_TIMEOUT_SECONDS", default=240.0, minimum=1.0, maximum=900.0
            ),
            request_timeout_seconds=_bounded_float_env(
                "IRONRAG_E2E_REQUEST_TIMEOUT_SECONDS", default=30.0, minimum=1.0, maximum=120.0
            ),
        )


class IronRagClient:
    """Small redirect-free JSON client that never includes bodies in errors."""

    def __init__(self, base_url: str, timeout_seconds: float) -> None:
        self._base_url = base_url
        self._timeout_seconds = timeout_seconds
        self._cookies = http.cookiejar.CookieJar()
        self._opener = urllib.request.build_opener(
            _NoRedirect(), urllib.request.HTTPCookieProcessor(self._cookies)
        )
        self.bearer_token: str | None = None

    def request(
        self,
        method: str,
        path: str,
        payload: dict[str, Any] | None = None,
        *,
        accepted_statuses: frozenset[int] = frozenset({200}),
    ) -> Any:
        data = None
        headers = {
            "Accept": "application/json",
            "User-Agent": "ironrag-youtrack-full-e2e-bootstrap/0.1.0",
        }
        if payload is not None:
            data = json.dumps(payload, separators=(",", ":")).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if self.bearer_token is not None:
            headers["Authorization"] = f"Bearer {self.bearer_token}"

        request = urllib.request.Request(
            f"{self._base_url}{path}", data=data, headers=headers, method=method
        )
        try:
            with self._opener.open(request, timeout=self._timeout_seconds) as response:
                status_code = response.status
                body = response.read(MAX_RESPONSE_BYTES + 1)
        except urllib.error.HTTPError as error:
            # Drain only a bounded amount and intentionally discard it: API error
            # bodies can reflect submitted credentials.
            try:
                error.read(MAX_RESPONSE_BYTES + 1)
            finally:
                error.close()
            raise ApiStatusError(method, path, error.code) from None
        except OSError:
            raise ApiTransportError(f"IronRAG {method} {path} transport request failed") from None

        if status_code not in accepted_statuses:
            raise ApiStatusError(method, path, status_code)
        if len(body) > MAX_RESPONSE_BYTES:
            raise BootstrapError(f"IronRAG {method} {path} response exceeded the safety limit")
        if not body:
            return None
        try:
            return json.loads(body)
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise BootstrapError(f"IronRAG {method} {path} returned invalid JSON") from None


def _required_env(name: str, *, strip: bool = True) -> str:
    raw = os.environ.get(name)
    if raw is None:
        raise BootstrapError(f"required environment variable {name} is missing")
    value = raw.strip() if strip else raw
    if not value:
        raise BootstrapError(f"required environment variable {name} is empty")
    return value


def _optional_secret_env(name: str) -> str | None:
    value = os.environ.get(name, "").strip()
    if not value:
        return None
    if any(character.isspace() for character in value) or "\0" in value:
        raise BootstrapError(f"{name} contains a forbidden whitespace or control character")
    return value


def _bounded_float_env(name: str, *, default: float, minimum: float, maximum: float) -> float:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        value = float(raw)
    except ValueError:
        raise BootstrapError(f"{name} must be numeric") from None
    if not minimum <= value <= maximum:
        raise BootstrapError(f"{name} must be between {minimum:g} and {maximum:g}")
    return value


def _normalise_service_url(raw: str) -> str:
    parsed = _parse_http_url(raw, "IRONRAG_E2E_URL")
    _validate_http_url(parsed, "IRONRAG_E2E_URL")
    path = parsed.path.rstrip("/")
    if path.endswith("/v1"):
        path = path[:-3]
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))


def _normalise_mock_provider_url(raw: str) -> str:
    parsed = _parse_http_url(raw, "IRONRAG_E2E_MOCK_PROVIDER_URL")
    _validate_http_url(parsed, "IRONRAG_E2E_MOCK_PROVIDER_URL")
    path = parsed.path.rstrip("/")
    if not path.endswith("/v1"):
        path = f"{path}/v1"
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))


def _parse_http_url(raw: str, variable: str) -> urllib.parse.SplitResult:
    try:
        return urllib.parse.urlsplit(raw)
    except ValueError:
        raise BootstrapError(f"{variable} must be a valid absolute HTTP(S) URL") from None


def _validate_http_url(parsed: urllib.parse.SplitResult, variable: str) -> None:
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise BootstrapError(f"{variable} must be an absolute HTTP(S) URL")
    if parsed.username is not None or parsed.password is not None:
        raise BootstrapError(f"{variable} must not contain credentials")
    if parsed.query or parsed.fragment:
        raise BootstrapError(f"{variable} must not contain a query or fragment")
    if any(character.isspace() for character in parsed.geturl()):
        raise BootstrapError(f"{variable} must not contain whitespace")
    try:
        _ = parsed.port
    except ValueError:
        raise BootstrapError(f"{variable} contains an invalid TCP port") from None


def _require_safe_target(base_url: str) -> None:
    hostname = urllib.parse.urlsplit(base_url).hostname
    is_loopback = hostname == "localhost"
    if hostname and not is_loopback:
        try:
            is_loopback = ipaddress.ip_address(hostname).is_loopback
        except ValueError:
            is_loopback = False
    if not is_loopback and os.environ.get("IRONRAG_E2E_ALLOW_REMOTE_DESTRUCTIVE") != "1":
        raise BootstrapError(
            "IRONRAG_E2E_URL is not loopback; set "
            "IRONRAG_E2E_ALLOW_REMOTE_DESTRUCTIVE=1 only for an isolated fixture"
        )


def _expect_object(value: Any, operation: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BootstrapError(f"IronRAG {operation} response must be a JSON object")
    return value


def _expect_list(value: Any, operation: str) -> list[dict[str, Any]]:
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise BootstrapError(f"IronRAG {operation} response must be a JSON object array")
    return value


def _expect_string(value: dict[str, Any], key: str, operation: str) -> str:
    item = value.get(key)
    if not isinstance(item, str) or not item:
        raise BootstrapError(f"IronRAG {operation} response is missing {key}")
    return item


def _wait_for_bootstrap_status(client: IronRagClient, timeout_seconds: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last_error: BootstrapError | None = None
    while time.monotonic() < deadline:
        try:
            status = _expect_object(
                client.request("GET", "/v1/iam/bootstrap/status"), "bootstrap status"
            )
            if isinstance(status.get("setupRequired"), bool):
                return status
            raise BootstrapError("IronRAG bootstrap status omitted setupRequired")
        except ApiStatusError as error:
            if error.status_code < 500 and error.status_code != 404:
                raise
            last_error = error
        except ApiTransportError as error:
            last_error = error
        time.sleep(min(2.0, max(0.1, deadline - time.monotonic())))
    del last_error
    raise BootstrapError("IronRAG did not become bootstrap-ready before the timeout")


def _authenticate_admin(
    client: IronRagClient, config: Config, bootstrap_status: dict[str, Any]
) -> None:
    if bootstrap_status["setupRequired"]:
        payload = {
            "login": config.admin_login,
            "displayName": "E2E Admin",
            "password": config.admin_password,
            "aiSetup": {
                "providerKind": "ollama",
                "apiKey": config.mock_provider_api_key,
                "baseUrl": config.mock_provider_url,
            },
        }
        try:
            client.request("POST", "/v1/iam/bootstrap/setup", payload)
            return
        except ApiStatusError as error:
            if error.status_code != 409:
                raise
            # A concurrent idempotent helper may have claimed the fixture.

    if config.admin_token is not None:
        client.bearer_token = config.admin_token
        try:
            client.request("GET", "/v1/iam/me")
            return
        except ApiStatusError as error:
            client.bearer_token = None
            if error.status_code not in {401, 403}:
                raise

    client.request(
        "POST",
        "/v1/iam/session/login",
        {"login": config.admin_login, "password": config.admin_password, "rememberMe": False},
    )


def _get_workspace(client: IronRagClient, slug: str) -> dict[str, Any]:
    workspaces = _expect_list(client.request("GET", "/v1/catalog/workspaces"), "workspace list")
    matches = [workspace for workspace in workspaces if workspace.get("slug") == slug]
    if len(matches) != 1:
        raise BootstrapError(
            "IronRAG fixture must contain exactly one workspace with IRONRAG_E2E_WORKSPACE_SLUG"
        )
    _expect_string(matches[0], "id", "workspace list")
    return matches[0]


def _library_path(workspace_id: str) -> str:
    return f"/v1/catalog/workspaces/{urllib.parse.quote(workspace_id, safe='')}/libraries"


def _get_or_create_library(
    client: IronRagClient, workspace_id: str, library_slug: str
) -> dict[str, Any]:
    path = _library_path(workspace_id)

    def find() -> dict[str, Any] | None:
        libraries = _expect_list(client.request("GET", path), "library list")
        matches = [library for library in libraries if library.get("slug") == library_slug]
        if len(matches) > 1:
            raise BootstrapError("IronRAG returned duplicate fixture library slugs")
        return matches[0] if matches else None

    existing = find()
    if existing is not None:
        _expect_string(existing, "id", "library list")
        return existing
    try:
        created = _expect_object(
            client.request(
                "POST",
                path,
                {
                    "slug": library_slug,
                    "displayName": "YouTrack E2E",
                    "description": "Isolated synthetic connector lifecycle fixture.",
                },
            ),
            "library create",
        )
    except ApiStatusError as error:
        if error.status_code != 409:
            raise
        conflicted = find()
        if conflicted is None:
            raise BootstrapError(
                "fixture library create conflicted but no matching library exists"
            ) from None
        created = conflicted
    _expect_string(created, "id", "library create")
    return created


def _scope_query(workspace_id: str, library_id: str) -> str:
    return urllib.parse.urlencode(
        {"scopeKind": "library", "workspaceId": workspace_id, "libraryId": library_id}
    )


def _list_accounts(
    client: IronRagClient, workspace_id: str, library_id: str
) -> list[dict[str, Any]]:
    return _expect_list(
        client.request("GET", f"/v1/ai/accounts?{_scope_query(workspace_id, library_id)}"),
        "AI account list",
    )


def _get_or_create_account(
    client: IronRagClient, config: Config, workspace_id: str, library_id: str
) -> dict[str, Any]:
    def find() -> dict[str, Any] | None:
        matches = [
            account
            for account in _list_accounts(client, workspace_id, library_id)
            if account.get("label") == ACCOUNT_LABEL
            and account.get("providerCatalogId") == OLLAMA_PROVIDER_ID
        ]
        if len(matches) > 1:
            raise BootstrapError("IronRAG returned duplicate fixture AI accounts")
        return matches[0] if matches else None

    account = find()
    if account is None:
        try:
            account = _expect_object(
                client.request(
                    "POST",
                    "/v1/ai/accounts",
                    {
                        "scopeKind": "library",
                        "workspaceId": workspace_id,
                        "libraryId": library_id,
                        "providerCatalogId": OLLAMA_PROVIDER_ID,
                        "label": ACCOUNT_LABEL,
                        "apiKey": config.mock_provider_api_key,
                        "baseUrl": config.mock_provider_url,
                    },
                ),
                "AI account create",
            )
        except ApiStatusError as error:
            if error.status_code != 409:
                raise
            account = find()
            if account is None:
                raise BootstrapError(
                    "AI account create conflicted but no fixture account exists"
                ) from None

    account_id = _expect_string(account, "id", "AI account")
    if (
        account.get("baseUrl", "").rstrip("/") != config.mock_provider_url.rstrip("/")
        or account.get("credentialState") != "active"
        or config.mock_provider_api_key is not None
    ):
        account = _expect_object(
            client.request(
                "PUT",
                f"/v1/ai/accounts/{urllib.parse.quote(account_id, safe='')}",
                {
                    "label": ACCOUNT_LABEL,
                    "apiKey": config.mock_provider_api_key,
                    "baseUrl": config.mock_provider_url,
                    "credentialState": "active",
                },
            ),
            "AI account update",
        )
        _expect_string(account, "id", "AI account update")
    return account


def _verify_models(
    client: IronRagClient, workspace_id: str, library_id: str, account_id: str
) -> None:
    query = urllib.parse.urlencode(
        {
            "providerCatalogId": OLLAMA_PROVIDER_ID,
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "accountId": account_id,
        }
    )
    models = _expect_list(client.request("GET", f"/v1/ai/models?{query}"), "AI model list")
    by_id = {model.get("id"): model for model in models if isinstance(model.get("id"), str)}
    expectations = {
        OLLAMA_CHAT_MODEL_ID: {"extract_graph", "query_compile", "query_answer"},
        OLLAMA_EMBEDDING_MODEL_ID: {"embed_chunk", "query_retrieve"},
    }
    for model_id, required_purposes in expectations.items():
        model = by_id.get(model_id)
        if model is None:
            raise BootstrapError("IronRAG v0.5.9 Ollama fixture model is missing")
        allowed = model.get("allowedBindingPurposes")
        if not isinstance(allowed, list) or not required_purposes.issubset(set(allowed)):
            raise BootstrapError("IronRAG v0.5.9 Ollama fixture model purposes are incompatible")
        if model.get("availabilityState") != "available":
            raise BootstrapError("mock provider did not advertise all required fixture models")


def _list_bindings(
    client: IronRagClient, workspace_id: str, library_id: str
) -> list[dict[str, Any]]:
    return _expect_list(
        client.request("GET", f"/v1/ai/bindings?{_scope_query(workspace_id, library_id)}"),
        "AI binding list",
    )


def _binding_payload(
    purpose: str, model_id: str, account_id: str, workspace_id: str, library_id: str
) -> dict[str, Any]:
    return {
        "scopeKind": "library",
        "workspaceId": workspace_id,
        "libraryId": library_id,
        "bindingPurpose": purpose,
        "accountId": account_id,
        "modelCatalogId": model_id,
        "systemPrompt": None,
        "temperature": None,
        "topP": None,
        "maxOutputTokensOverride": None,
        "extraParametersJson": {},
    }


def _binding_update_payload(model_id: str, account_id: str) -> dict[str, Any]:
    return {
        "accountId": account_id,
        "modelCatalogId": model_id,
        "systemPrompt": None,
        "temperature": None,
        "topP": None,
        "maxOutputTokensOverride": None,
        "extraParametersJson": {},
        "bindingState": "active",
    }


def _ensure_bindings(
    client: IronRagClient, workspace_id: str, library_id: str, account_id: str
) -> None:
    # Creating either vector purpose creates/updates its exact-scope counterpart
    # in v0.5.9.  Re-listing on every iteration keeps this idempotent.
    for purpose, model_id in BINDING_MODELS.items():
        matches = [
            binding
            for binding in _list_bindings(client, workspace_id, library_id)
            if binding.get("bindingPurpose") == purpose
        ]
        if len(matches) > 1:
            raise BootstrapError("IronRAG returned duplicate fixture binding purposes")
        if not matches:
            try:
                client.request(
                    "POST",
                    "/v1/ai/bindings",
                    _binding_payload(purpose, model_id, account_id, workspace_id, library_id),
                )
                continue
            except ApiStatusError as error:
                if error.status_code != 409:
                    raise
                matches = [
                    binding
                    for binding in _list_bindings(client, workspace_id, library_id)
                    if binding.get("bindingPurpose") == purpose
                ]
                if len(matches) != 1:
                    raise BootstrapError(
                        "AI binding create conflicted but no matching fixture binding exists"
                    ) from None

        binding = matches[0]
        binding_id = _expect_string(binding, "id", "AI binding")
        if (
            binding.get("accountId") != account_id
            or binding.get("modelCatalogId") != model_id
            or binding.get("bindingState") != "active"
        ):
            client.request(
                "PUT",
                f"/v1/ai/bindings/{urllib.parse.quote(binding_id, safe='')}",
                _binding_update_payload(model_id, account_id),
            )

    final = {
        binding.get("bindingPurpose"): binding
        for binding in _list_bindings(client, workspace_id, library_id)
    }
    for purpose, model_id in BINDING_MODELS.items():
        final_binding = final.get(purpose)
        if (
            final_binding is None
            or final_binding.get("accountId") != account_id
            or final_binding.get("modelCatalogId") != model_id
            or final_binding.get("bindingState") != "active"
        ):
            raise BootstrapError("IronRAG fixture binding verification failed")


def _verify_ingestion_ready(client: IronRagClient, workspace_id: str, library_id: str) -> None:
    libraries = _expect_list(
        client.request("GET", _library_path(workspace_id)), "library readiness list"
    )
    library = next((item for item in libraries if item.get("id") == library_id), None)
    readiness = library.get("ingestionReadiness") if library is not None else None
    if not isinstance(readiness, dict) or readiness.get("ready") is not True:
        raise BootstrapError("fixture library is not ingestion-ready after binding setup")


def _read_existing_runtime(path: Path) -> dict[str, str]:
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        return {}
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_mode & 0o077
        or metadata.st_uid != os.geteuid()
    ):
        raise BootstrapError("existing runtime file must be a regular mode-0600 file")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        current = os.fstat(descriptor)
        if (
            not stat.S_ISREG(current.st_mode)
            or current.st_mode & 0o077
            or current.st_uid != os.geteuid()
        ):
            raise BootstrapError("existing runtime file changed during secure open")
        if current.st_size > MAX_RUNTIME_BYTES:
            raise BootstrapError("existing runtime file exceeds the safety limit")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            raw = stream.read(MAX_RUNTIME_BYTES + 1)
    finally:
        os.close(descriptor)
    if len(raw) > MAX_RUNTIME_BYTES:
        raise BootstrapError("existing runtime file exceeds the safety limit")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        raise BootstrapError("existing runtime file is not UTF-8") from None

    if path.suffix.lower() == ".json":
        try:
            value = json.loads(text)
        except json.JSONDecodeError:
            raise BootstrapError("existing runtime JSON is invalid") from None
        if not isinstance(value, dict):
            raise BootstrapError("existing runtime JSON must be an object")
        return {
            key: item
            for key, item in value.items()
            if isinstance(key, str) and isinstance(item, str)
        }

    values: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        try:
            parts = shlex.split(line, comments=False, posix=True)
        except ValueError:
            raise BootstrapError("existing runtime dotenv is invalid") from None
        if len(parts) != 1 or "=" not in parts[0]:
            raise BootstrapError("existing runtime dotenv is invalid")
        key, value = parts[0].split("=", 1)
        values[key] = value
    return values


def _runtime_token_is_reusable(
    values: dict[str, str], config: Config, workspace_id: str, library_id: str
) -> str | None:
    token = values.get("IRONRAG_API_TOKEN")
    if (
        not token
        or values.get("IRONRAG_BASE_URL") != config.base_url
        or values.get("IRONRAG_WORKSPACE_SLUG") != config.workspace_slug
        or values.get("IRONRAG_LIBRARY_SLUG") != config.library_slug
        or values.get("IRONRAG_WORKSPACE_ID") != workspace_id
        or values.get("IRONRAG_LIBRARY_ID") != library_id
    ):
        return None
    probe = IronRagClient(config.base_url, config.request_timeout_seconds)
    probe.bearer_token = token
    try:
        me = _expect_object(probe.request("GET", "/v1/iam/me"), "runtime token identity")
        if not _has_exact_runtime_scope(me, library_id):
            return None
        libraries = _expect_list(
            probe.request("GET", _library_path(workspace_id)), "runtime token probe"
        )
    except BootstrapError:
        return None
    return token if any(library.get("id") == library_id for library in libraries) else None


def _has_exact_runtime_scope(identity: dict[str, Any], library_id: str) -> bool:
    principal = identity.get("principal")
    if (
        not isinstance(principal, dict)
        or principal.get("principalKind") != "api_token"
        or identity.get("user") is not None
        or identity.get("workspaceMemberships") != []
    ):
        return False

    grants = identity.get("effectiveGrants")
    if not isinstance(grants, list):
        return False

    expected_grants = {
        ("library", library_id, "library_read"),
        ("library", library_id, "library_write"),
    }
    actual_grants: set[tuple[str, str, str]] = set()
    for grant in grants:
        if not isinstance(grant, dict):
            return False
        resource_kind = grant.get("resourceKind")
        resource_id = grant.get("resourceId")
        permission_kind = grant.get("permissionKind")
        if (
            not isinstance(resource_kind, str)
            or not isinstance(resource_id, str)
            or not isinstance(permission_kind, str)
        ):
            return False
        actual_grants.add((resource_kind, resource_id, permission_kind))

    return len(grants) == len(expected_grants) and actual_grants == expected_grants


def _mint_runtime_token(client: IronRagClient, workspace_id: str, library_id: str) -> str:
    response = _expect_object(
        client.request(
            "POST",
            "/v1/iam/tokens",
            {
                "workspaceId": workspace_id,
                "label": TOKEN_LABEL,
                "expiresAt": None,
                "libraryIds": [library_id],
                "permissionKinds": ["library_read", "library_write"],
            },
        ),
        "runtime token mint",
    )
    return _expect_string(response, "token", "runtime token mint")


def _atomic_write_runtime(path: Path, values: dict[str, str]) -> None:
    for key, value in values.items():
        unsafe_value = any(character in value for character in "\r\n\0")
        if not key or "\n" in key or "=" in key or unsafe_value:
            raise BootstrapError("runtime environment contains an unsafe key or value")
    if path.suffix.lower() == ".json":
        body = f"{json.dumps(values, indent=2, sort_keys=True)}\n".encode()
    else:
        body = "".join(f"{key}={shlex.quote(value)}\n" for key, value in values.items()).encode()

    try:
        current = os.lstat(path)
    except FileNotFoundError:
        current = None
    if current is not None and (not stat.S_ISREG(current.st_mode) or current.st_mode & 0o077):
        raise BootstrapError("runtime output target must be a regular mode-0600 file")
    if current is not None and current.st_uid != os.geteuid():
        raise BootstrapError("runtime output target must be owned by the current user")

    temporary = path.with_name(f".{path.name}.{os.getpid()}.{secrets.token_hex(6)}.tmp")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor: int | None = None
    try:
        descriptor = os.open(temporary, flags, 0o600)
        offset = 0
        while offset < len(body):
            written = os.write(descriptor, body[offset:])
            if written <= 0:
                raise BootstrapError("runtime file write made no progress")
            offset += written
        os.fchmod(descriptor, 0o600)
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = None
        os.replace(temporary, path)
        os.chmod(path, 0o600, follow_symlinks=False)
        directory = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if descriptor is not None:
            os.close(descriptor)
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def bootstrap(config: Config) -> None:
    existing_runtime = _read_existing_runtime(config.runtime_file)
    if BOOTSTRAP_ONLY_KEYS.intersection(existing_runtime):
        raise BootstrapError(
            "IRONRAG_E2E_RUNTIME_FILE must not point at the bootstrap environment file"
        )
    client = IronRagClient(config.base_url, config.request_timeout_seconds)
    status = _wait_for_bootstrap_status(client, config.ready_timeout_seconds)
    _authenticate_admin(client, config, status)

    workspace = _get_workspace(client, config.workspace_slug)
    workspace_id = _expect_string(workspace, "id", "workspace")
    library = _get_or_create_library(client, workspace_id, config.library_slug)
    library_id = _expect_string(library, "id", "library")

    account = _get_or_create_account(client, config, workspace_id, library_id)
    account_id = _expect_string(account, "id", "AI account")
    _verify_models(client, workspace_id, library_id, account_id)
    _ensure_bindings(client, workspace_id, library_id, account_id)
    _verify_ingestion_ready(client, workspace_id, library_id)

    token = _runtime_token_is_reusable(existing_runtime, config, workspace_id, library_id)
    if token is None:
        token = _mint_runtime_token(client, workspace_id, library_id)
    runtime = {
        "IRONRAG_BASE_URL": config.base_url,
        "IRONRAG_API_TOKEN": token,
        "IRONRAG_WORKSPACE_SLUG": config.workspace_slug,
        "IRONRAG_LIBRARY_SLUG": config.library_slug,
        "IRONRAG_WORKSPACE_ID": workspace_id,
        "IRONRAG_LIBRARY_ID": library_id,
    }
    if _runtime_token_is_reusable(runtime, config, workspace_id, library_id) is None:
        raise BootstrapError("minted runtime token did not receive the expected library scope")
    _atomic_write_runtime(config.runtime_file, runtime)


def main() -> int:
    try:
        bootstrap(Config.from_environment())
    except BootstrapError as error:
        print(f"IronRAG full-stack bootstrap failed: {error}", file=sys.stderr)
        return 1
    except OSError:
        print(
            "IronRAG full-stack bootstrap failed: local filesystem operation failed",
            file=sys.stderr,
        )
        return 1
    print("IronRAG full-stack runtime was written with mode 0600.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
