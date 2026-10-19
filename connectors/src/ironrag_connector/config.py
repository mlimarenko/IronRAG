"""Runtime configuration shared by every connector built on the framework.

Adapter-specific credentials (vendor base URL, API token, signing
secret) belong in a subclass:

    class MyConnectorSettings(BaseConnectorSettings):
        vendor_base_url: str
        vendor_api_token: str
        vendor_webhook_secret: str | None = None

Pydantic loads ``.env.local`` and ``.env`` so secrets stay outside the
process environment when desired. ``extra="ignore"`` lets the same
``.env`` file power several connectors at once during dev.
"""

from __future__ import annotations

from enum import StrEnum
from pathlib import Path

from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict

from .policy import (
    DeleteAction,
    DuplicateContentAction,
    PushPolicy,
    UpdateAction,
    UpsertAction,
)


class RunMode(StrEnum):
    """How the connector picks up changes from the source.

    * ``poll`` — only the periodic sweep runs. Webhook handlers passed
      to ``build_app`` are ignored. Pick this when the vendor has no
      webhook support or you want a single, predictable cadence.
    * ``webhook`` — webhook handlers are mounted, the periodic sweep is
      disabled. Connectors with dependents queue coalesced background canonical
      sweeps after deliveries, so released copies reach the completeness-gated
      reaper. Manual ``/sync/run`` still works for catch-up. Pick this when the
      vendor's webhooks are reliable and ordered.
    * ``both`` — sweep AND webhooks run side by side. The sweep is the
      reconciliation safety net for missed/dropped webhook deliveries.
      Default and recommended for most connectors.
    """

    POLL = "poll"
    WEBHOOK = "webhook"
    BOTH = "both"


class PolicyDefaults(BaseSettings):
    """Global default policy. Per-kind overrides live in routing.yaml."""

    model_config = SettingsConfigDict(env_prefix="DEFAULT_POLICY_", extra="ignore")

    on_new: UpsertAction = UpsertAction.CREATE
    on_changed: UpdateAction = UpdateAction.REPLACE
    on_missing: DeleteAction = DeleteAction.DELETE
    on_duplicate_content: DuplicateContentAction = DuplicateContentAction.SKIP

    def as_push_policy(self) -> PushPolicy:
        return PushPolicy(
            on_new=self.on_new,
            on_changed=self.on_changed,
            on_missing=self.on_missing,
            on_duplicate_content=self.on_duplicate_content,
        )


class BaseConnectorSettings(BaseSettings):
    """Everything the framework itself needs. Subclass for vendor creds."""

    model_config = SettingsConfigDict(env_file=(".env.local", ".env"), extra="ignore")

    # --- IronRAG ---
    ironrag_base_url: str
    ironrag_api_token: str
    request_timeout_seconds: float = Field(60.0, ge=1.0)
    revision_create_timeout_seconds: float = Field(default=300.0, ge=1.0)
    """Budget for a synchronous revision-create POST (:meth:`create_revision`).

    Revision creation is documented as asynchronous (202 + Location), but the
    server replies only once the inline append has materialized. This budget
    must therefore exceed that materialization; 300s leaves headroom over a
    large document while still bounding a genuinely hung request. Every other
    call keeps ``request_timeout_seconds``.

    Distinct from the removed ``ironrag_mutation_timeout_seconds``, which the
    0.2.0 line superseded with the operation poll budget
    (``operation_poll_budget_seconds``); this setting bounds the create call
    itself, not the polled operation.
    """
    operation_poll_interval_seconds: float = Field(default=1.0, ge=0.05)
    """Delay between polls in :meth:`IronRagClient.wait_for_operation`.

    Every asynchronous mutation (revision, delete) is a 202 + Location to
    ``/v1/ops/operations/{id}``; this is the interval the SDK's one
    poll-to-terminal primitive sleeps between polls.
    """
    operation_poll_budget_seconds: float = Field(default=120.0, ge=1.0)
    """Maximum wall-clock time :meth:`IronRagClient.wait_for_operation` polls
    before raising ``IronRagMutationTimeoutError``. Distinct from
    ``sync_item_timeout_seconds``: an item's overall budget also covers
    routing, fetch, and dependents, not just the one operation poll."""
    rewalk_concurrency: int = Field(default=4, ge=1, le=64)
    """Fan-out bound for a post-upgrade full re-walk driven by
    :meth:`IronRagClient.walk_all_documents`. Confirm against each source
    system's documented rate limit before the first production re-walk."""
    reaper_list_timeout_seconds: float = Field(default=30.0, ge=1.0)
    """Maximum time for one IronRAG reaper document-list request.

    Reaping runs after item enumeration, outside the per-item timeout. This
    bound prevents a slow document listing endpoint from holding the whole
    sweep lock after source processing has finished.
    """

    # --- Routing ---
    routing_config_path: Path = Field(default=Path("routing.yaml"))

    # --- Run mode ---
    run_mode: RunMode = RunMode.BOTH

    # --- Sync loop (only consulted when run_mode != webhook) ---
    sync_interval_seconds: int = Field(default=1800, ge=60)
    sync_run_on_startup: bool = False
    sync_concurrency: int = Field(default=4, ge=1, le=64)
    sync_item_timeout_seconds: float = Field(default=300.0, ge=1.0)
    """Maximum wall-clock time for one source ref, including dependents."""
    sync_item_retry_timeout_seconds: float = Field(default=30.0, ge=1.0)
    """Per-item budget for a ref that is timing out on consecutive sweeps.

    A ref that timed out at the full budget is usually stuck behind a vendor
    rate limit that spaces request starts; replaying it under the same budget
    every sweep burns the whole sweep, so the shorter budget is used while the
    timeouts stay consecutive. The full budget returns on the first attempt and
    every ``FULL_BUDGET_ESCALATION_ATTEMPTS``-th attempt, so a legitimately slow
    ref still ingests instead of being starved. Any non-timeout completion
    clears the count. The effective budget never exceeds
    ``sync_item_timeout_seconds``.
    """

    # --- Persistent state ---
    state_db_path: Path = Field(default=Path("./connector-state.sqlite"))

    # --- HTTP server ---
    host: str = "0.0.0.0"
    port: int = 8088
    log_level: str = "info"
    admin_bearer_token: str | None = None

    # --- Process lock ---
    pidfile_path: Path | None = None
    """Defaults beside ``state_db_path`` if unset."""

    def policy_defaults(self) -> PushPolicy:
        return PolicyDefaults().as_push_policy()
