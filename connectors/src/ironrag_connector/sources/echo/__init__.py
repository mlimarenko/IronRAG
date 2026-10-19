"""Echo connector — a trivial reference implementation.

Shows the minimum a SourceAdapter must do: enumerate items with a stable
change_token, fetch their bytes, and round-trip an external_key through
``external_key`` / ``parse_external_key``. The "source" is a dict in memory;
swap in your vendor's REST calls to turn it into a real connector.

It ships inside the distribution rather than as an example project so that the
framework's own tests exercise a registered source, and so the reference
implementation cannot drift out of step with the adapter contract.
"""

from __future__ import annotations

from ...config import BaseConnectorSettings
from ...registry import SourcePlugin
from ...routing import PolicyOverrides
from .adapter import EchoAdapter, EchoPage

DEMO_PAGES = {
    "1": EchoPage(
        item_id="1",
        title="Echo page",
        body="# Echo\n\nA page that exists only in memory.\n",
        updated_at="1",
        tag="demo",
    ),
}


def build_echo_adapter(_settings: BaseConnectorSettings, _policies: PolicyOverrides) -> EchoAdapter:
    """Adapt Echo's in-memory constructor to the registry's factory contract.

    Every other source builds its adapter from its settings; Echo has no vendor
    to configure, so it takes a page dict instead, and it reads no per-kind
    policy. Without this shim the registry would hand it a settings object and
    fail at start-up — which is what the registration gate and
    `test_echo_adapter` exist to catch.
    """
    return EchoAdapter(dict(DEMO_PAGES))


PLUGIN = SourcePlugin("echo", BaseConnectorSettings, build_echo_adapter)

__all__ = ["DEMO_PAGES", "PLUGIN", "EchoAdapter", "EchoPage", "build_echo_adapter"]
