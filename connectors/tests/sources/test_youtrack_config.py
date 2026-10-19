from __future__ import annotations

import pytest
from pydantic import ValidationError

from ironrag_connector.config import RunMode
from ironrag_connector.sources.youtrack.config import YouTrackSettings


def _settings(**overrides: object) -> YouTrackSettings:
    data: dict[str, object] = {
        "youtrack_base_url": "https://tracker.example.com/youtrack/",
        "youtrack_token": "perm:synthetic-token",
        "ironrag_base_url": "https://ironrag.example.com",
        "ironrag_api_token": "synthetic-ironrag-token",
        "admin_bearer_token": "synthetic-admin-token",
    }
    data.update(overrides)
    return YouTrackSettings(**data)


def test_settings_normalize_base_url_and_default_to_polling() -> None:
    settings = _settings()

    assert settings.youtrack_base_url == "https://tracker.example.com/youtrack"
    assert settings.run_mode is RunMode.POLL
    assert settings.youtrack_snapshot_validation_passes == 2


@pytest.mark.parametrize("run_mode", [RunMode.WEBHOOK, RunMode.BOTH])
def test_settings_reject_run_modes_without_complete_kb_events(
    run_mode: RunMode,
) -> None:
    with pytest.raises(ValidationError, match="RUN_MODE must be 'poll'"):
        _settings(run_mode=run_mode)


def test_project_filters_are_trimmed_deduplicated_and_ordered() -> None:
    settings = _settings(
        youtrack_projects=" DOCS,0-7,DOCS, ",
        youtrack_exclude_projects="ARCHIVE, 0-9,ARCHIVE",
    )

    assert settings.included_projects() == ("DOCS", "0-7")
    assert settings.excluded_projects() == ("ARCHIVE", "0-9")


@pytest.mark.parametrize(
    "url",
    [
        "ftp://tracker.example.com",
        "https://user:password@tracker.example.com",
        "https://tracker.example.com/path?token=value",
    ],
)
def test_settings_reject_unsafe_base_urls(url: str) -> None:
    with pytest.raises(ValidationError):
        _settings(youtrack_base_url=url)


def test_token_is_redacted_from_settings_repr() -> None:
    settings = _settings(youtrack_token="perm:must-not-leak")

    assert "must-not-leak" not in repr(settings)


def test_single_snapshot_validation_pass_is_rejected() -> None:
    with pytest.raises(ValidationError):
        _settings(youtrack_snapshot_validation_passes=1)


def test_require_tag_defaults_to_unset() -> None:
    assert _settings().youtrack_require_tag is None


def test_require_tag_is_read_verbatim_and_case_sensitive() -> None:
    settings = _settings(youtrack_require_tag="ironrag-sync")

    assert settings.youtrack_require_tag == "ironrag-sync"


def test_indexed_custom_fields_are_explicit_deduplicated_and_ordered() -> None:
    defaults = _settings()
    configured = _settings(youtrack_custom_fields=" Type,State, Product / Area,Type, Assignee ")

    assert defaults.indexed_custom_fields() == (
        "Type",
        "State",
        "Priority",
        "Assignee",
    )
    assert configured.indexed_custom_fields() == (
        "Type",
        "State",
        "Product / Area",
        "Assignee",
    )
