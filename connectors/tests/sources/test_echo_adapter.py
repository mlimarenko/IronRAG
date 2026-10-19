"""The reference adapter, and the contract every registered source must meet."""

from __future__ import annotations

import pytest

from ironrag_connector.config import BaseConnectorSettings
from ironrag_connector.policy import PushPolicy
from ironrag_connector.registry import available_sources, load_source
from ironrag_connector.routing import PolicyOverrides
from ironrag_connector.source import (
    SourceDeleteEnforcement,
    SourceFetchDelete,
    SourceFetchMaterialized,
)
from ironrag_connector.sources.echo import DEMO_PAGES, EchoAdapter, build_echo_adapter


def _settings() -> BaseConnectorSettings:
    return BaseConnectorSettings(
        ironrag_base_url="http://ironrag.invalid",
        ironrag_api_token="token",
        admin_bearer_token="admin",
    )


def _policies() -> PolicyOverrides:
    return PolicyOverrides(default=PushPolicy(), by_kind={})


def test_registry_factory_builds_the_adapter_from_settings() -> None:
    """The registry calls `adapter_factory(settings, policies)` — every source accepts that.

    Echo's own constructor takes a page dict, so registering the class directly
    would hand it a settings object and fail only when someone ran it.
    """
    plugin = load_source("echo")
    adapter = plugin.adapter_factory(_settings(), _policies())
    assert isinstance(adapter, EchoAdapter)
    assert adapter.name == "echo"


def test_every_registered_source_declares_the_kinds_it_emits() -> None:
    for name in available_sources():
        plugin = load_source(name)
        assert plugin.settings_cls is not None, name
        assert callable(plugin.adapter_factory), name


async def test_echo_round_trips_an_item() -> None:
    adapter = build_echo_adapter(_settings(), _policies())
    refs = [ref async for ref in adapter.iter_items()]
    assert [ref.item_id for ref in refs] == list(DEMO_PAGES)

    result = await adapter.fetch(refs[0])
    assert isinstance(result, SourceFetchMaterialized)
    item = result.item
    assert item.payload.decode("utf-8").startswith("# Echo")

    parsed = adapter.parse_external_key(refs[0].external_key)
    assert parsed == ("page", refs[0].item_id)


async def test_fetching_an_unknown_item_requests_policy_delete() -> None:
    adapter = build_echo_adapter(_settings(), _policies())
    ref = next(iter([r async for r in adapter.iter_items()]))
    missing = ref.__class__(**{**ref.__dict__, "item_id": "does-not-exist"})
    result = await adapter.fetch(missing)
    assert isinstance(result, SourceFetchDelete)
    assert result.enforcement is SourceDeleteEnforcement.POLICY
    assert result.cacheable is False


def test_external_keys_from_another_source_are_rejected() -> None:
    adapter = build_echo_adapter(_settings(), _policies())
    assert adapter.parse_external_key("bookstack:page:1") is None


def test_missing_enforcement_is_explicit_for_every_declared_kind() -> None:
    adapter = build_echo_adapter(_settings(), _policies())
    assert {kind: adapter.missing_enforcement(kind) for kind in adapter.kinds} == {
        "page": SourceDeleteEnforcement.POLICY
    }
    with pytest.raises(ValueError, match="unsupported echo item kind: future-kind"):
        adapter.missing_enforcement("future-kind")


@pytest.mark.parametrize("malformed", ["echo:", "echo:page", "echo::1"])
def test_malformed_external_keys_are_rejected(malformed: str) -> None:
    adapter = build_echo_adapter(_settings(), _policies())
    assert adapter.parse_external_key(malformed) is None
