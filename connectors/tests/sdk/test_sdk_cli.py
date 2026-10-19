"""The dispatcher that replaced five per-connector entry points."""

from __future__ import annotations

import pytest

from ironrag_connector.cli import _parse, main
from ironrag_connector.policy import PushPolicy
from ironrag_connector.registry import (
    SourcePlugin,
    UnknownSourceError,
    available_sources,
    load_source,
)
from ironrag_connector.routing import PolicyOverrides


def test_every_shipped_source_resolves_to_a_complete_plugin() -> None:
    names = available_sources()
    assert names == ("bookstack", "confluence", "echo", "gitrepos", "youtrack")
    for name in names:
        plugin = load_source(name)
        assert isinstance(plugin, SourcePlugin)
        assert plugin.name == name
        # A registration that resolves but cannot build anything is the failure
        # mode a string-keyed registry hides until runtime.
        assert callable(plugin.adapter_factory)
        assert plugin.settings_cls is not None


def test_unknown_source_names_the_ones_that_exist() -> None:
    with pytest.raises(UnknownSourceError) as excinfo:
        load_source("sharepoint")
    message = str(excinfo.value)
    assert "sharepoint" in message
    for name in available_sources():
        assert name in message


def test_list_prints_every_source(capsys: pytest.CaptureFixture[str]) -> None:
    main(["--list"])
    printed = capsys.readouterr().out.split()
    assert printed == list(available_sources())


def test_no_source_is_an_error_not_a_default() -> None:
    with pytest.raises(SystemExit) as excinfo:
        main([])
    assert "source" in str(excinfo.value)


def test_cli_validates_only_the_selected_source(monkeypatch: pytest.MonkeyPatch) -> None:
    """Settings are built after dispatch, and only for the chosen source.

    Every source reads the same unprefixed RUN_MODE, and YouTrack rejects
    anything but `poll`. If the CLI validated all five, a BookStack deployment
    running in webhook mode could not start at all.
    """
    for variable, value in (
        ("IRONRAG_BASE_URL", "http://ironrag.invalid"),
        ("IRONRAG_API_TOKEN", "token"),
        ("ADMIN_BEARER_TOKEN", "admin"),
        ("RUN_MODE", "webhook"),
        ("BOOKSTACK_BASE_URL", "http://bookstack.invalid"),
        ("BOOKSTACK_TOKEN_ID", "id"),
        ("BOOKSTACK_TOKEN_SECRET", "secret"),
        ("YOUTRACK_BASE_URL", "http://youtrack.invalid"),
        ("YOUTRACK_TOKEN", "token"),
    ):
        monkeypatch.setenv(variable, value)

    bookstack = load_source("bookstack")
    settings = bookstack.settings_cls()
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    assert bookstack.adapter_factory(settings, policies) is not None

    youtrack = load_source("youtrack")
    with pytest.raises(ValueError, match="poll"):
        youtrack.settings_cls()


def test_parse_accepts_a_source_argument() -> None:
    assert _parse(["bookstack"]).source == "bookstack"
    assert _parse(["--list"]).list is True
