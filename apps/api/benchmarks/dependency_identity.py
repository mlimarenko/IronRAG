#!/usr/bin/env python3
"""Secret-free endpoint and resolved dependency-profile identities."""

from __future__ import annotations

import argparse

import csv
import hashlib
import json
import os
import socket
import struct
import subprocess
import urllib.parse
from typing import Any, Sequence

from apps.api.benchmarks.lifecycle_state import stable_json_sha256
from apps.api.benchmarks.tls_pinning import activate_pinned_dependency_proxy, pinned_tls_context


DEPENDENCY_PIN_PROXIES_ENV = "IRONRAG_DEPENDENCY_PIN_PROXIES"
INVALID_PROXY_MAPPING = "dependency pin proxy mapping is invalid"
INVALID_REDIS_SCHEME = "Redis URL must use rediss"


def _register_proxy(kind: str, endpoint_sha256: str, local_port: int) -> None:
    raw = os.environ.get(DEPENDENCY_PIN_PROXIES_ENV, "{}")
    try:
        mapping = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(INVALID_PROXY_MAPPING) from error
    if not isinstance(mapping, dict):
        raise ValueError(INVALID_PROXY_MAPPING)
    endpoints = mapping.setdefault(kind, {})
    if not isinstance(endpoints, dict):
        raise ValueError(INVALID_PROXY_MAPPING)
    endpoints[endpoint_sha256] = local_port
    os.environ[DEPENDENCY_PIN_PROXIES_ENV] = json.dumps(mapping, sort_keys=True)


def _identity_query(query: str) -> str:
    return urllib.parse.urlencode(
        sorted(
            (key, value)
            for key, value in urllib.parse.parse_qsl(query, keep_blank_values=True)
            if key != "sslrootcert"
        )
    )


def database_endpoint_sha256(database_url: str) -> str:
    parsed = urllib.parse.urlsplit(database_url.strip())
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.hostname or not parsed.path:
        raise ValueError("database URL is invalid")
    query_pairs = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)
    if [value for key, value in query_pairs if key == "sslmode"] != ["verify-full"]:
        raise ValueError("database URL must require exactly one sslmode=verify-full")
    query = _identity_query(parsed.query)
    username = urllib.parse.unquote(parsed.username) if parsed.username else ""
    authority = f"{username + '@' if username else ''}{parsed.hostname.lower()}:{parsed.port or 5432}"
    endpoint = urllib.parse.urlunsplit(("postgresql", authority, parsed.path, query, ""))
    return hashlib.sha256(endpoint.encode("utf-8")).hexdigest()


def database_service_sha256(database_url: str) -> str:
    """Identify the PostgreSQL server/transport without the logical database or credentials."""
    parsed = urllib.parse.urlsplit(database_url.strip())
    if parsed.scheme not in {"postgres", "postgresql"} or not parsed.hostname or not parsed.path:
        raise ValueError("database URL is invalid")
    query_pairs = urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)
    if [value for key, value in query_pairs if key == "sslmode"] != ["verify-full"]:
        raise ValueError("database URL must require exactly one sslmode=verify-full")
    query = _identity_query(parsed.query)
    service = urllib.parse.urlunsplit(
        ("postgresql", f"{parsed.hostname.lower()}:{parsed.port or 5432}", "", query, "")
    )
    return hashlib.sha256(service.encode("utf-8")).hexdigest()


def redis_endpoint_sha256(redis_url: str) -> str:
    parsed = urllib.parse.urlsplit(redis_url.strip())
    if parsed.scheme != "rediss" or not parsed.hostname:
        raise ValueError(INVALID_REDIS_SCHEME)
    try:
        database = int(parsed.path.removeprefix("/") or "0")
    except ValueError as error:
        raise ValueError("Redis database is invalid") from error
    if database < 0 or parsed.query or parsed.fragment:
        raise ValueError("Redis URL is invalid")
    endpoint = urllib.parse.urlunsplit(
        (
            parsed.scheme.lower(),
            f"{parsed.hostname.lower()}:{parsed.port or 6379}",
            f"/{database}",
            "",
            "",
        )
    )
    return hashlib.sha256(endpoint.encode("utf-8")).hexdigest()


def redis_service_sha256(redis_url: str) -> str:
    """Identify the Redis server/transport without the logical database or credentials."""
    parsed = urllib.parse.urlsplit(redis_url.strip())
    if parsed.scheme != "rediss" or not parsed.hostname:
        raise ValueError(INVALID_REDIS_SCHEME)
    try:
        database = int(parsed.path.removeprefix("/") or "0")
    except ValueError as error:
        raise ValueError("Redis database is invalid") from error
    if database < 0 or parsed.query or parsed.fragment:
        raise ValueError("Redis URL is invalid")
    service = urllib.parse.urlunsplit(
        (parsed.scheme.lower(), f"{parsed.hostname.lower()}:{parsed.port or 6379}", "", "", "")
    )
    return hashlib.sha256(service.encode("utf-8")).hexdigest()


def database_profile(database_url: str) -> dict[str, Any]:
    from scripts.ops.collect_vector_lifecycle_state import database_environment

    settings = (
        "server_version_num", "max_connections", "shared_buffers", "effective_cache_size",
        "work_mem", "maintenance_work_mem", "random_page_cost", "effective_io_concurrency",
        "max_worker_processes", "max_parallel_workers", "max_parallel_workers_per_gather",
    )
    query = (
        "select name, setting, unit from pg_settings where name in ("
        + ",".join("'" + name + "'" for name in settings)
        + ") order by name"
    )
    completed = subprocess.run(
        ["psql", "-X", "--csv", "--tuples-only", "--set=ON_ERROR_STOP=1", "--command", query],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=database_environment(database_url),
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError("cannot collect PostgreSQL resolved profile")
    rows = [row for row in csv.reader(completed.stdout.splitlines()) if row]
    if len(rows) != len(settings) or any(len(row) != 3 for row in rows):
        raise RuntimeError("PostgreSQL resolved profile is incomplete")
    payload = {row[0]: {"setting": row[1], "unit": row[2]} for row in rows}
    return {"resolved": payload, "sha256": stable_json_sha256(payload)}


def redis_cli(redis_url: str, *arguments: str) -> subprocess.CompletedProcess[str]:
    parsed = urllib.parse.urlsplit(redis_url.strip())
    if parsed.scheme != "rediss" or not parsed.hostname:
        raise ValueError(INVALID_REDIS_SCHEME)
    try:
        proxy_mapping = json.loads(os.environ.get(DEPENDENCY_PIN_PROXIES_ENV, "{}"))
        local_port = proxy_mapping["redis"][redis_endpoint_sha256(redis_url)]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise ValueError("Redis exact leaf pin proxy is not active") from error
    command = [
        "redis-cli", "--no-auth-warning", "-h", "127.0.0.1",
        "-p", str(local_port), "-n", str(int(parsed.path.removeprefix("/") or "0")),
        "--raw",
    ]
    if parsed.username:
        command.extend(("--user", parsed.username))
    environment = os.environ.copy()
    if parsed.password:
        environment["REDISCLI_AUTH"] = parsed.password
    else:
        environment.pop("REDISCLI_AUTH", None)
    return subprocess.run(
        [*command, *arguments], capture_output=True, text=True, encoding="utf-8",
        env=environment, check=False,
    )


def redis_profile(redis_url: str) -> dict[str, Any]:
    info = redis_cli(redis_url, "INFO", "server")
    config = redis_cli(
        redis_url, "CONFIG", "GET", "maxmemory", "maxmemory-policy", "databases", "io-threads"
    )
    if info.returncode != 0 or config.returncode != 0:
        raise RuntimeError("cannot collect Redis resolved profile")
    allowed_info = {
        "redis_version",
        "redis_mode",
        "os",
        "arch_bits",
        "multiplexing_api",
        "gcc_version",
        # Process-unique server identity: equal profiles now prove that both
        # logical databases are served by the same live Redis process.
        "run_id",
    }
    info_values = {}
    for line in info.stdout.splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            if key in allowed_info:
                info_values[key] = value.strip()
    config_lines = [line.strip() for line in config.stdout.splitlines() if line.strip()]
    if len(config_lines) % 2:
        raise RuntimeError("Redis resolved profile CONFIG response is invalid")
    config_values = dict(zip(config_lines[::2], config_lines[1::2], strict=True))
    payload = {"server": info_values, "config": dict(sorted(config_values.items()))}
    if not info_values.get("redis_version") or not info_values.get("run_id") or len(config_values) != 4:
        raise RuntimeError("Redis resolved profile is incomplete")
    return {"resolved": payload, "sha256": stable_json_sha256(payload)}


def _database_tls_peer_certificate(database_url: str) -> bytes:
    parsed = urllib.parse.urlsplit(database_url.strip())
    database_endpoint_sha256(database_url)
    parameters = dict(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True))
    context = pinned_tls_context(cafile=parameters.get("sslrootcert") or None)
    with socket.create_connection((parsed.hostname, parsed.port or 5432), timeout=15) as raw:
        raw.sendall(struct.pack("!II", 8, 80877103))
        if raw.recv(1) != b"S":
            raise ValueError("PostgreSQL server rejected TLS")
        with context.wrap_socket(raw, server_hostname=parsed.hostname) as secured:
            certificate = secured.getpeercert(binary_form=True)
    if not certificate:
        raise ValueError("PostgreSQL TLS peer did not provide a certificate")
    return certificate


def database_tls_certificate_sha256(database_url: str) -> str:
    return hashlib.sha256(_database_tls_peer_certificate(database_url)).hexdigest()


def _redis_tls_peer_certificate(redis_url: str) -> bytes:
    parsed = urllib.parse.urlsplit(redis_url.strip())
    redis_endpoint_sha256(redis_url)
    context = pinned_tls_context()
    with socket.create_connection((parsed.hostname, parsed.port or 6379), timeout=15) as raw:
        with context.wrap_socket(raw, server_hostname=parsed.hostname) as secured:
            certificate = secured.getpeercert(binary_form=True)
    if not certificate:
        raise ValueError("Redis TLS peer did not provide a certificate")
    return certificate


def redis_tls_certificate_sha256(redis_url: str) -> str:
    return hashlib.sha256(_redis_tls_peer_certificate(redis_url)).hexdigest()


def activate_database_leaf_pin(
    database_url: str,
    trusted_certificate_sha256: str,
) -> None:
    certificate = _database_tls_peer_certificate(database_url)
    if hashlib.sha256(certificate).hexdigest() != trusted_certificate_sha256:
        raise ValueError("PostgreSQL TLS peer differs from trust pin")
    parsed = urllib.parse.urlsplit(database_url.strip())
    port = activate_pinned_dependency_proxy(
        parsed.hostname or "", parsed.port or 5432, trusted_certificate_sha256,
        "postgres", certificate,
    )
    _register_proxy("database", database_endpoint_sha256(database_url), port)


def activate_redis_leaf_pin(
    redis_url: str,
    trusted_certificate_sha256: str,
) -> None:
    certificate = _redis_tls_peer_certificate(redis_url)
    if hashlib.sha256(certificate).hexdigest() != trusted_certificate_sha256:
        raise ValueError("Redis TLS peer differs from trust pin")
    parsed = urllib.parse.urlsplit(redis_url.strip())
    port = activate_pinned_dependency_proxy(
        parsed.hostname or "", parsed.port or 6379, trusted_certificate_sha256,
        "redis", certificate,
    )
    _register_proxy("redis", redis_endpoint_sha256(redis_url), port)


def is_lowercase_sha256(value: str) -> bool:
    """One concrete lowercase SHA-256 and nothing else."""
    return len(value) == 64 and all(character in "0123456789abcdef" for character in value)


def add_role_trust_arguments(parser: argparse.ArgumentParser) -> None:
    """Register the per-role certificate, endpoint and profile trust pins.

    Both the experiment runner and the independent verifier take the same set,
    because the verifier has to be able to reject exactly what the runner
    claimed; the two lists drifting apart would make verification vacuous.
    """
    for role in ("baseline", "candidate"):
        for dependency in ("database", "redis"):
            parser.add_argument(
                f"--trusted-{role}-{dependency}-certificate-sha256", required=True
            )
        for endpoint in ("api", "database", "redis"):
            parser.add_argument(f"--trusted-{role}-{endpoint}-endpoint-sha256", required=True)
        for dependency in ("database", "redis"):
            parser.add_argument(f"--trusted-{role}-{dependency}-profile-sha256", required=True)


def role_trust_argument_names(role: str) -> list[str]:
    """The destination names of every trust pin belonging to one role."""
    return (
        [f"trusted_{role}_api_certificate_sha256"]
        + [
            f"trusted_{role}_{dependency}_certificate_sha256"
            for dependency in ("database", "redis")
        ]
        + [
            f"trusted_{role}_{endpoint}_endpoint_sha256"
            for endpoint in ("api", "database", "redis")
        ]
        + [
            f"trusted_{role}_{dependency}_profile_sha256"
            for dependency in ("database", "redis")
        ]
    )


def require_sha256_arguments(
    parser: argparse.ArgumentParser, args: argparse.Namespace, names: Sequence[str]
) -> None:
    """Reject any of these arguments that is not one concrete lowercase SHA-256."""
    for name in names:
        if not is_lowercase_sha256(getattr(args, name)):
            parser.error(f"--{name.replace('_', '-')} must be lowercase SHA-256")
