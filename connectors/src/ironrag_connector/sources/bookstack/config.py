"""BookStack-specific settings on top of the framework's base.

Adds vendor credentials, export format, retry/rate-limit knobs, and the
BookStack webhook-signing secrets. Inherits IronRAG creds, sync loop
tuning, server bind, state path, pidfile, and admin bearer from
``BaseConnectorSettings``.
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from ironrag_connector import BaseConnectorSettings

ExportFormat = Literal["markdown", "html", "plaintext", "pdf"]


class BookStackSettings(BaseConnectorSettings):
    # --- BookStack REST API ---
    bookstack_base_url: str
    bookstack_token_id: str
    bookstack_token_secret: str

    bookstack_page_format: ExportFormat = "markdown"
    bookstack_retry_max: int = 5
    bookstack_retry_backoff_seconds: float = 1.5
    bookstack_retry_max_sleep_seconds: float = 30.0
    bookstack_shelf_cache_ttl_seconds: int = Field(900, ge=10)
    bookstack_min_request_interval_seconds: float = Field(default=0.7, ge=0.0)

    # --- BookStack webhook signing ---
    # Either or both: HMAC-SHA256 (X-BookStack-Signature) and a fixed
    # bearer. At least one is required for the /webhook/bookstack route
    # to be considered safe to mount.
    bookstack_webhook_secret: str | None = None
    bookstack_webhook_bearer: str | None = None
