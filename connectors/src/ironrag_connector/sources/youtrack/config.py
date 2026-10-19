from __future__ import annotations

from urllib.parse import urlsplit, urlunsplit

from pydantic import Field, SecretStr, field_validator

from ironrag_connector import BaseConnectorSettings
from ironrag_connector.config import RunMode


class YouTrackSettings(BaseConnectorSettings):
    """Operator settings for the YouTrack REST adapter."""

    youtrack_base_url: str
    youtrack_token: SecretStr

    youtrack_page_size: int = Field(default=100, ge=1, le=500)
    youtrack_retry_max: int = Field(default=5, ge=0, le=20)
    youtrack_retry_backoff_seconds: float = Field(default=1.0, ge=0.1)
    youtrack_retry_max_sleep_seconds: float = Field(default=30.0, ge=0.1)
    youtrack_min_request_interval_seconds: float = Field(default=0.1, ge=0.0)
    youtrack_snapshot_validation_passes: int = Field(default=2, ge=2, le=3)

    youtrack_projects: str | None = None
    youtrack_exclude_projects: str | None = None
    youtrack_include_archived_projects: bool = False

    # Point-selection gate, ANDed with the project include/exclude filters
    # above: when set, only items (of either kind) carrying this exact tag
    # sync, regardless of project. Unset (the default) leaves scope entirely
    # to the project filters -- no behaviour change for existing deployments.
    youtrack_require_tag: str | None = None

    # Only explicitly configured custom fields cross the source boundary.
    # YouTrack projects may contain private/service fields, so requesting every
    # field is not a safe default. Operators can extend this allowlist without
    # putting station-specific vocabulary in product code.
    youtrack_custom_fields: str = "Type,State,Priority,Assignee"

    # The built-in YouTrack webhook integration does not emit Knowledge Base
    # article events, so polling is the only complete reconciliation source.
    run_mode: RunMode = RunMode.POLL

    @field_validator("run_mode")
    @classmethod
    def _require_poll_mode(cls, value: RunMode) -> RunMode:
        if value is not RunMode.POLL:
            raise ValueError(
                "RUN_MODE must be 'poll': YouTrack Knowledge Base has no "
                "complete lifecycle webhooks"
            )
        return value

    @field_validator("youtrack_base_url")
    @classmethod
    def _validate_base_url(cls, value: str) -> str:
        candidate = value.strip()
        if not candidate or any(character.isspace() for character in candidate):
            raise ValueError("YOUTRACK_BASE_URL must be an HTTP(S) URL")
        parsed = urlsplit(candidate)
        if parsed.scheme not in {"http", "https"} or not parsed.hostname:
            raise ValueError("YOUTRACK_BASE_URL must be an HTTP(S) URL")
        if parsed.username is not None or parsed.password is not None:
            raise ValueError("YOUTRACK_BASE_URL must not contain credentials")
        if parsed.query or parsed.fragment:
            raise ValueError("YOUTRACK_BASE_URL must not contain a query or fragment")

        path = parsed.path.rstrip("/")
        if path.endswith("/api"):
            path = path[: -len("/api")]
        return urlunsplit((parsed.scheme, parsed.netloc, path, "", ""))

    def included_projects(self) -> tuple[str, ...]:
        return _parse_csv(self.youtrack_projects)

    def excluded_projects(self) -> tuple[str, ...]:
        return _parse_csv(self.youtrack_exclude_projects)

    def indexed_custom_fields(self) -> tuple[str, ...]:
        return _parse_csv(self.youtrack_custom_fields)


def _parse_csv(raw: str | None) -> tuple[str, ...]:
    if not raw:
        return ()
    result: list[str] = []
    seen: set[str] = set()
    for value in raw.split(","):
        item = value.strip()
        if not item or item in seen:
            continue
        seen.add(item)
        result.append(item)
    return tuple(result)
