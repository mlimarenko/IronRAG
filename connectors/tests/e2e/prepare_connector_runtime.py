#!/usr/bin/env python3
"""Prepare least-privilege connector and host artifacts for full-stack E2E.

Inputs:

* ``YOUTRACK_E2E_URL`` and ``YOUTRACK_E2E_TOKEN`` from the YouTrack bootstrap.
* ``IRONRAG_E2E_RUNTIME_FILE`` from ``bootstrap_full_stack.py``.  It may be
  JSON or dotenv, must contain exactly the six documented runtime keys, and
  must be a regular current-user-owned mode-0600 file.

Output paths are supplied through ``CONNECTOR_E2E_ENV_FILE``,
``CONNECTOR_E2E_ROUTING_FILE``, and ``CONNECTOR_E2E_HOST_ENV_FILE``.  They must
be distinct, normalized absolute paths below ``/tmp`` with no symlinked parent
components.  Secret-bearing env artifacts are staged and atomically replaced
with mode 0600.  The non-secret routing file uses mode 0644 so the connector's
UID 10001 can read its bind mount.  The host environment is committed last and
acts as the generation marker.

Optional fixture overrides:

* ``YOUTRACK_E2E_INTERNAL_URL`` (default ``http://youtrack:8080``)
* ``IRONRAG_E2E_INTERNAL_URL`` (default ``http://frontend``)
* ``CONNECTOR_E2E_PORT`` (default ``18188``)
* ``CONNECTOR_E2E_CONTAINER``
* ``YOUTRACK_E2E_PAGE_SIZE`` (default ``7``)
* ``YOUTRACK_E2E_EXCLUDE_PROJECTS`` (``DEMO`` is always retained)
* ``CONNECTOR_E2E_SYNC_CONCURRENCY`` (default ``1``)

No secret, credential-derived value, response body, or generated token is
written to stdout or stderr.
"""

from __future__ import annotations

import contextlib
import ipaddress
import json
import os
import re
import secrets
import shlex
import stat
import sys
import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import UUID

MAX_INPUT_BYTES = 64 * 1024
RUNTIME_KEYS = frozenset(
    {
        "IRONRAG_BASE_URL",
        "IRONRAG_API_TOKEN",
        "IRONRAG_WORKSPACE_SLUG",
        "IRONRAG_LIBRARY_SLUG",
        "IRONRAG_WORKSPACE_ID",
        "IRONRAG_LIBRARY_ID",
    }
)
ENV_KEY_PATTERN = re.compile(r"^[A-Z][A-Z0-9_]*$")
PROJECT_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")
CONTAINER_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
CATALOG_SLUG_PATTERN = re.compile(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$")
CONNECTOR_CONTAINER_PREFIX = "ironrag-youtrack-connector-e2e-"

# Where the fixture is allowed to write. `/tmp` on the workstation this runs on
# is a RAM-backed tmpfs the project rules keep test artifacts out of, so the
# root is configurable and defaults to an ignored directory in the repository.
TMP_ROOT_ENV = "IRONRAG_CONNECTOR_E2E_TMP"
DEFAULT_TMP_ROOT = Path(__file__).resolve().parents[3] / "tmp-connector-e2e"


def tmp_root() -> Path:
    """The directory this fixture may write into, resolved at call time."""
    configured = os.environ.get(TMP_ROOT_ENV)
    root = Path(configured) if configured else DEFAULT_TMP_ROOT
    return root.resolve()


class PrepareError(RuntimeError):
    """Expected error whose message is safe to expose."""


@dataclass(frozen=True)
class RuntimeContract:
    base_url: str
    api_token: str
    workspace_slug: str
    library_slug: str
    workspace_id: str
    library_id: str


@dataclass(frozen=True)
class Config:
    youtrack_host_url: str
    youtrack_token: str
    ironrag: RuntimeContract
    ironrag_runtime_file: Path
    connector_env_file: Path
    routing_file: Path
    host_env_file: Path
    youtrack_internal_url: str
    ironrag_internal_url: str
    connector_port: int
    connector_container: str
    page_size: int
    excluded_projects: tuple[str, ...]
    concurrency: int

    @classmethod
    def from_environment(cls) -> Config:
        ironrag_runtime_file = _tmp_path(
            "IRONRAG_E2E_RUNTIME_FILE", _required_env("IRONRAG_E2E_RUNTIME_FILE")
        )
        connector_env_file = _tmp_path(
            "CONNECTOR_E2E_ENV_FILE", _required_env("CONNECTOR_E2E_ENV_FILE")
        )
        routing_file = _tmp_path(
            "CONNECTOR_E2E_ROUTING_FILE", _required_env("CONNECTOR_E2E_ROUTING_FILE")
        )
        host_env_file = _tmp_path(
            "CONNECTOR_E2E_HOST_ENV_FILE", _required_env("CONNECTOR_E2E_HOST_ENV_FILE")
        )
        all_paths = {
            ironrag_runtime_file,
            connector_env_file,
            routing_file,
            host_env_file,
        }
        if len(all_paths) != 4:
            raise PrepareError("input and output artifact paths must all be distinct")

        runtime = _load_runtime_contract(ironrag_runtime_file)
        youtrack_host_url = _normalise_http_url(
            _required_env("YOUTRACK_E2E_URL"), "YOUTRACK_E2E_URL"
        )
        _require_loopback(youtrack_host_url, "YOUTRACK_E2E_URL")
        _require_loopback(runtime.base_url, "IRONRAG_BASE_URL in the runtime artifact")

        youtrack_token = _required_secret("YOUTRACK_E2E_TOKEN", prefixes=("perm:", "perm-"))
        connector_container = os.environ.get(
            "CONNECTOR_E2E_CONTAINER",
            "ironrag-youtrack-connector-e2e-connector-1",
        ).strip()
        if not CONTAINER_PATTERN.fullmatch(
            connector_container
        ) or not connector_container.startswith(CONNECTOR_CONTAINER_PREFIX):
            raise PrepareError(
                "CONNECTOR_E2E_CONTAINER must stay inside the isolated connector E2E namespace"
            )

        excluded_projects = _excluded_projects(os.environ.get("YOUTRACK_E2E_EXCLUDE_PROJECTS", ""))
        return cls(
            youtrack_host_url=youtrack_host_url,
            youtrack_token=youtrack_token,
            ironrag=runtime,
            ironrag_runtime_file=ironrag_runtime_file,
            connector_env_file=connector_env_file,
            routing_file=routing_file,
            host_env_file=host_env_file,
            youtrack_internal_url=_normalise_http_url(
                os.environ.get("YOUTRACK_E2E_INTERNAL_URL", "http://youtrack:8080"),
                "YOUTRACK_E2E_INTERNAL_URL",
            ),
            ironrag_internal_url=_normalise_http_url(
                os.environ.get("IRONRAG_E2E_INTERNAL_URL", "http://frontend"),
                "IRONRAG_E2E_INTERNAL_URL",
            ),
            connector_port=_bounded_int_env(
                "CONNECTOR_E2E_PORT", default=18188, minimum=1024, maximum=65535
            ),
            connector_container=connector_container,
            page_size=_bounded_int_env("YOUTRACK_E2E_PAGE_SIZE", default=7, minimum=1, maximum=500),
            excluded_projects=excluded_projects,
            concurrency=_bounded_int_env(
                "CONNECTOR_E2E_SYNC_CONCURRENCY", default=1, minimum=1, maximum=64
            ),
        )


@dataclass
class _StagedFile:
    path: Path
    directory_fd: int
    temporary_name: str | None
    mode: int

    def cleanup(self) -> None:
        if self.temporary_name is not None:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(self.temporary_name, dir_fd=self.directory_fd)
            self.temporary_name = None
        os.close(self.directory_fd)


def _required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise PrepareError(f"required environment variable {name} is missing or empty")
    if any(character in value for character in "\r\n\0"):
        raise PrepareError(f"{name} contains a forbidden control character")
    return value


def _required_secret(name: str, *, prefixes: tuple[str, ...] | None = None) -> str:
    value = _required_env(name)
    if any(character.isspace() for character in value):
        raise PrepareError(f"{name} contains forbidden whitespace")
    if prefixes is not None and not value.startswith(prefixes):
        raise PrepareError(f"{name} does not match the expected fixture token type")
    return value


def _bounded_int_env(name: str, *, default: int, minimum: int, maximum: int) -> int:
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        value = int(raw, 10)
    except ValueError:
        raise PrepareError(f"{name} must be an integer") from None
    if not minimum <= value <= maximum:
        raise PrepareError(f"{name} must be between {minimum} and {maximum}")
    return value


def _normalise_http_url(raw: str, variable: str) -> str:
    candidate = raw.strip()
    if not candidate or any(character.isspace() for character in candidate):
        raise PrepareError(f"{variable} must be an absolute HTTP(S) URL")
    try:
        parsed = urllib.parse.urlsplit(candidate)
    except ValueError:
        raise PrepareError(f"{variable} must be a valid absolute HTTP(S) URL") from None
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise PrepareError(f"{variable} must be an absolute HTTP(S) URL")
    if parsed.username is not None or parsed.password is not None:
        raise PrepareError(f"{variable} must not contain credentials")
    if parsed.query or parsed.fragment:
        raise PrepareError(f"{variable} must not contain a query or fragment")
    try:
        _ = parsed.port
    except ValueError:
        raise PrepareError(f"{variable} contains an invalid TCP port") from None
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path.rstrip("/"), "", ""))


def _require_loopback(url: str, description: str) -> None:
    hostname = urllib.parse.urlsplit(url).hostname
    is_loopback = hostname == "localhost"
    if hostname and not is_loopback:
        try:
            is_loopback = ipaddress.ip_address(hostname).is_loopback
        except ValueError:
            is_loopback = False
    if not is_loopback:
        raise PrepareError(f"{description} must be an explicit loopback URL")


def _excluded_projects(raw: str) -> tuple[str, ...]:
    result = ["DEMO"]
    seen = {"DEMO"}
    for part in raw.split(","):
        value = part.strip()
        if not value or value in seen:
            continue
        if not PROJECT_PATTERN.fullmatch(value):
            raise PrepareError(
                "YOUTRACK_E2E_EXCLUDE_PROJECTS contains an invalid project identifier"
            )
        seen.add(value)
        result.append(value)
    return tuple(result)


def _tmp_path(variable: str, raw: str) -> Path:
    root = tmp_root()
    path = Path(raw)
    if not path.is_absolute():
        raise PrepareError(f"{variable} must be an absolute path below {root}")
    if any(part in {".", ".."} for part in path.parts):
        raise PrepareError(f"{variable} must be a normalized path below {root}")
    try:
        common = os.path.commonpath((str(root), str(path)))
    except ValueError:
        raise PrepareError(f"{variable} must be a path below {root}") from None
    if common != str(root) or path == root or not path.name:
        raise PrepareError(f"{variable} must name a file below {root}")
    return path


def _open_directory_without_symlinks(directory: Path) -> int:
    if not directory.is_absolute():
        raise PrepareError("artifact parent must be absolute")
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open("/", flags)
    try:
        for component in directory.parts[1:]:
            next_descriptor = os.open(component, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = next_descriptor
        metadata = os.fstat(descriptor)
        if not stat.S_ISDIR(metadata.st_mode):
            raise PrepareError("artifact parent is not a directory")
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _validate_existing_output_at(directory_fd: int, name: str, mode: int) -> None:
    try:
        metadata = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_IMODE(metadata.st_mode) != mode
        or metadata.st_uid != os.geteuid()
    ):
        raise PrepareError(
            "existing output has an unsafe type, mode, or owner for its artifact kind"
        )


def _secure_read(path: Path) -> bytes:
    directory_fd = _open_directory_without_symlinks(path.parent)
    descriptor: int | None = None
    try:
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path.name, flags, dir_fd=directory_fd)
        metadata = os.fstat(descriptor)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or stat.S_IMODE(metadata.st_mode) != 0o600
            or metadata.st_uid != os.geteuid()
            or metadata.st_nlink != 1
        ):
            raise PrepareError(
                "IronRAG runtime must be a regular single-link current-user-owned mode-0600 file"
            )
        if metadata.st_size > MAX_INPUT_BYTES:
            raise PrepareError("IronRAG runtime exceeds the input safety limit")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            body = stream.read(MAX_INPUT_BYTES + 1)
        if len(body) > MAX_INPUT_BYTES:
            raise PrepareError("IronRAG runtime exceeds the input safety limit")
        return body
    finally:
        if descriptor is not None:
            os.close(descriptor)
        os.close(directory_fd)


def _load_runtime_contract(path: Path) -> RuntimeContract:
    raw = _secure_read(path)
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        raise PrepareError("IronRAG runtime must be UTF-8") from None
    values = _parse_runtime_json(text) if path.suffix.lower() == ".json" else _parse_dotenv(text)
    if frozenset(values) != RUNTIME_KEYS:
        raise PrepareError("IronRAG runtime does not match the six-key bootstrap contract")

    base_url = _normalise_http_url(values["IRONRAG_BASE_URL"], "IRONRAG_BASE_URL")
    api_token = _validate_runtime_token(values["IRONRAG_API_TOKEN"])
    workspace_slug = _catalog_slug(values["IRONRAG_WORKSPACE_SLUG"], "workspace")
    library_slug = _catalog_slug(values["IRONRAG_LIBRARY_SLUG"], "library")
    workspace_id = _canonical_uuid(values["IRONRAG_WORKSPACE_ID"], "workspace")
    library_id = _canonical_uuid(values["IRONRAG_LIBRARY_ID"], "library")
    return RuntimeContract(
        base_url=base_url,
        api_token=api_token,
        workspace_slug=workspace_slug,
        library_slug=library_slug,
        workspace_id=workspace_id,
        library_id=library_id,
    )


def _parse_runtime_json(text: str) -> dict[str, str]:
    def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise PrepareError("IronRAG runtime JSON contains duplicate keys")
            result[key] = value
        return result

    try:
        payload = json.loads(text, object_pairs_hook=reject_duplicates)
    except json.JSONDecodeError:
        raise PrepareError("IronRAG runtime JSON is invalid") from None
    if not isinstance(payload, dict) or not all(
        isinstance(key, str) and isinstance(value, str) for key, value in payload.items()
    ):
        raise PrepareError("IronRAG runtime JSON must be a flat string map")
    return payload


def _parse_dotenv(text: str) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        try:
            parts = shlex.split(line, comments=False, posix=True)
        except ValueError:
            raise PrepareError("IronRAG runtime dotenv is invalid") from None
        if len(parts) != 1 or "=" not in parts[0]:
            raise PrepareError("IronRAG runtime dotenv is invalid")
        key, value = parts[0].split("=", 1)
        if not ENV_KEY_PATTERN.fullmatch(key) or key in values:
            raise PrepareError("IronRAG runtime dotenv contains an invalid or duplicate key")
        values[key] = value
    return values


def _validate_runtime_token(value: str) -> str:
    if (
        not value.startswith(("inrg_", "irt_"))
        or len(value) < 16
        or any(character.isspace() for character in value)
        or "\0" in value
    ):
        raise PrepareError("IRONRAG_API_TOKEN does not match the bootstrap token contract")
    return value


def _canonical_uuid(value: str, kind: str) -> str:
    try:
        parsed = UUID(value)
    except ValueError:
        raise PrepareError(f"IronRAG runtime contains an invalid {kind} identifier") from None
    canonical = str(parsed)
    if value != canonical:
        raise PrepareError(f"IronRAG runtime {kind} identifier must use canonical UUID syntax")
    return canonical


def _catalog_slug(value: str, kind: str) -> str:
    if not CATALOG_SLUG_PATTERN.fullmatch(value):
        raise PrepareError(f"IronRAG runtime contains an invalid {kind} slug")
    return value


def _dotenv(values: dict[str, str]) -> bytes:
    lines: list[str] = []
    for key, value in values.items():
        if not ENV_KEY_PATTERN.fullmatch(key):
            raise PrepareError("generated environment contains an invalid key")
        if any(character in value for character in "\r\n\0"):
            raise PrepareError("generated environment contains an unsafe value")
        lines.append(f"{key}={shlex.quote(value)}")
    return f"{'\n'.join(lines)}\n".encode()


def _routing_yaml(runtime: RuntimeContract) -> bytes:
    return (
        "default:\n"
        f"  library: '{runtime.workspace_slug}/{runtime.library_slug}'\n"
        "\n"
        "policies:\n"
        "  article:\n"
        "    on_new: create\n"
        "    on_changed: replace\n"
        "    on_missing: delete\n"
        "    on_duplicate_content: skip\n"
    ).encode()


def _connector_environment(config: Config, admin_token: str) -> dict[str, str]:
    return {
        "YOUTRACK_BASE_URL": config.youtrack_internal_url,
        "YOUTRACK_TOKEN": config.youtrack_token,
        "YOUTRACK_PAGE_SIZE": str(config.page_size),
        "YOUTRACK_RETRY_MAX": "5",
        "YOUTRACK_RETRY_BACKOFF_SECONDS": "0.1",
        "YOUTRACK_RETRY_MAX_SLEEP_SECONDS": "2",
        "YOUTRACK_MIN_REQUEST_INTERVAL_SECONDS": "0",
        "YOUTRACK_SNAPSHOT_VALIDATION_PASSES": "2",
        "YOUTRACK_EXCLUDE_PROJECTS": ",".join(config.excluded_projects),
        "YOUTRACK_INCLUDE_ARCHIVED_PROJECTS": "false",
        "IRONRAG_BASE_URL": config.ironrag_internal_url,
        "IRONRAG_API_TOKEN": config.ironrag.api_token,
        "REQUEST_TIMEOUT_SECONDS": "60",
        "OPERATION_POLL_INTERVAL_SECONDS": "1",
        "OPERATION_POLL_BUDGET_SECONDS": "120",
        "REWALK_CONCURRENCY": "4",
        "REAPER_LIST_TIMEOUT_SECONDS": "30",
        "ROUTING_CONFIG_PATH": "/app/routing.yaml",
        "RUN_MODE": "poll",
        "SYNC_INTERVAL_SECONDS": "3600",
        "SYNC_RUN_ON_STARTUP": "false",
        "SYNC_CONCURRENCY": str(config.concurrency),
        "SYNC_ITEM_TIMEOUT_SECONDS": "300",
        "STATE_DB_PATH": "/var/lib/ironrag-connector/state.sqlite",
        "HOST": "0.0.0.0",
        "PORT": "8088",
        "LOG_LEVEL": "info",
        "ADMIN_BEARER_TOKEN": admin_token,
        "DEFAULT_POLICY_ON_NEW": "create",
        "DEFAULT_POLICY_ON_CHANGED": "replace",
        "DEFAULT_POLICY_ON_MISSING": "delete",
        "DEFAULT_POLICY_ON_DUPLICATE_CONTENT": "skip",
    }


def _host_environment(config: Config, admin_token: str) -> dict[str, str]:
    return {
        "YOUTRACK_E2E_URL": config.youtrack_host_url,
        "YOUTRACK_E2E_TOKEN": config.youtrack_token,
        "IRONRAG_E2E_URL": config.ironrag.base_url,
        "IRONRAG_E2E_TOKEN": config.ironrag.api_token,
        "IRONRAG_E2E_WORKSPACE_ID": config.ironrag.workspace_id,
        "IRONRAG_E2E_LIBRARY_ID": config.ironrag.library_id,
        "CONNECTOR_E2E_URL": f"http://127.0.0.1:{config.connector_port}",
        "CONNECTOR_E2E_ADMIN_TOKEN": admin_token,
        "CONNECTOR_E2E_CONTAINER": config.connector_container,
        "CONNECTOR_E2E_PORT": str(config.connector_port),
        "CONNECTOR_E2E_ENV_FILE": str(config.connector_env_file),
        "CONNECTOR_E2E_ROUTING_FILE": str(config.routing_file),
    }


def _stage_file(path: Path, body: bytes, mode: int) -> _StagedFile:
    directory_fd = _open_directory_without_symlinks(path.parent)
    temporary_name: str | None = None
    descriptor: int | None = None
    try:
        _validate_existing_output_at(directory_fd, path.name, mode)
        temporary_name = f".{path.name}.{os.getpid()}.{secrets.token_hex(8)}.tmp"
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(temporary_name, flags, mode, dir_fd=directory_fd)
        offset = 0
        while offset < len(body):
            written = os.write(descriptor, body[offset:])
            if written <= 0:
                raise PrepareError("artifact write made no progress")
            offset += written
        os.fchmod(descriptor, mode)
        os.fsync(descriptor)
        os.close(descriptor)
        descriptor = None
        return _StagedFile(path, directory_fd, temporary_name, mode)
    except BaseException:
        if descriptor is not None:
            os.close(descriptor)
        if temporary_name is not None:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(temporary_name, dir_fd=directory_fd)
        os.close(directory_fd)
        raise


def _commit_staged(staged: list[_StagedFile]) -> None:
    try:
        for item in staged:
            if item.temporary_name is None:
                raise PrepareError("artifact staging state is invalid")
            os.replace(
                item.temporary_name,
                item.path.name,
                src_dir_fd=item.directory_fd,
                dst_dir_fd=item.directory_fd,
            )
            item.temporary_name = None
            metadata = os.stat(item.path.name, dir_fd=item.directory_fd, follow_symlinks=False)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or stat.S_IMODE(metadata.st_mode) != item.mode
                or metadata.st_uid != os.geteuid()
            ):
                raise PrepareError("committed artifact failed its mode and ownership check")
            os.fsync(item.directory_fd)
    finally:
        for item in staged:
            item.cleanup()


def prepare(config: Config) -> None:
    outputs = (
        (config.routing_file, 0o644),
        (config.connector_env_file, 0o600),
        (config.host_env_file, 0o600),
    )
    for path, mode in outputs:
        directory_fd = _open_directory_without_symlinks(path.parent)
        try:
            _validate_existing_output_at(directory_fd, path.name, mode)
        finally:
            os.close(directory_fd)

    admin_token = f"e2e_{secrets.token_urlsafe(36)}"
    connector_env = _dotenv(_connector_environment(config, admin_token))
    routing = _routing_yaml(config.ironrag)
    host_env = _dotenv(_host_environment(config, admin_token))

    staged: list[_StagedFile] = []
    try:
        # Host env is the commit marker and is intentionally replaced last.
        staged.append(_stage_file(config.routing_file, routing, 0o644))
        staged.append(_stage_file(config.connector_env_file, connector_env, 0o600))
        staged.append(_stage_file(config.host_env_file, host_env, 0o600))
    except BaseException:
        for item in staged:
            item.cleanup()
        raise
    _commit_staged(staged)


def main() -> int:
    try:
        prepare(Config.from_environment())
    except PrepareError as error:
        print(f"Connector full-stack runtime preparation failed: {error}", file=sys.stderr)
        return 1
    except OSError:
        print(
            "Connector full-stack runtime preparation failed: secure filesystem operation failed",
            file=sys.stderr,
        )
        return 1
    print("Connector env artifacts were written with mode 0600 and routing with mode 0644.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
