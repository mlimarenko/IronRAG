from __future__ import annotations

from typing import Literal

from pydantic import Field

from ironrag_connector import BaseConnectorSettings

ConfluenceAuthMode = Literal["auto", "anonymous", "basic", "bearer"]


class ConfluenceSettings(BaseConnectorSettings):
    confluence_base_url: str
    confluence_auth_mode: ConfluenceAuthMode = "auto"
    confluence_username: str | None = None
    confluence_password: str | None = None
    confluence_token: str | None = None

    confluence_page_size: int = Field(default=100, ge=1, le=250)
    confluence_retry_max: int = Field(default=5, ge=0, le=20)
    confluence_retry_backoff_seconds: float = Field(default=1.5, ge=0.1)
    confluence_retry_max_sleep_seconds: float = Field(default=30.0, ge=1.0)
    confluence_min_request_interval_seconds: float = Field(default=0.2, ge=0.0)

    confluence_space_keys: str | None = None
    confluence_exclude_space_keys: str | None = None
    confluence_cql: str | None = None
    confluence_include_archived: bool = False

    confluence_include_attachments: bool = True
    confluence_attachment_max_bytes: int = Field(default=25 * 1024 * 1024, ge=1)

    confluence_webhook_secret: str | None = None

    def included_space_keys(self) -> tuple[str, ...] | None:
        """Return explicit include-list or None for "all spaces" mode."""
        return _parse_include_space_keys(self.confluence_space_keys)

    def excluded_space_keys(self) -> tuple[str, ...]:
        return _parse_csv(self.confluence_exclude_space_keys)


def _parse_csv(raw: str | None) -> tuple[str, ...]:
    if not raw:
        return ()
    parts = [part.strip() for part in raw.split(",")]
    deduped: list[str] = []
    seen: set[str] = set()
    for part in parts:
        if not part:
            continue
        if part in seen:
            continue
        seen.add(part)
        deduped.append(part)
    return tuple(deduped)


def _parse_include_space_keys(raw: str | None) -> tuple[str, ...] | None:
    values = _parse_csv(raw)
    if not values:
        return None
    if any(value.lower() in {"*", "all", "any"} for value in values):
        return None
    return values
