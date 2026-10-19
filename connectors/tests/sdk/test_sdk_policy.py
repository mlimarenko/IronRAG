from __future__ import annotations

from ironrag_connector.policy import (
    DeleteAction,
    DuplicateContentAction,
    PolicyOverride,
    PushPolicy,
    UpdateAction,
    UpsertAction,
)


def test_default_policy_is_full_lifecycle() -> None:
    policy = PushPolicy()
    assert policy.on_new is UpsertAction.CREATE
    assert policy.on_changed is UpdateAction.REPLACE
    assert policy.on_missing is DeleteAction.DELETE
    assert policy.on_duplicate_content is DuplicateContentAction.SKIP


def test_merge_only_overrides_set_fields() -> None:
    base = PushPolicy()
    override = PolicyOverride(on_missing=DeleteAction.IGNORE)
    merged = base.merged_with(override)
    assert merged.on_missing is DeleteAction.IGNORE
    assert merged.on_new is UpsertAction.CREATE
    assert merged.on_changed is UpdateAction.REPLACE
    assert merged.include_comments is True


def test_include_comments_defaults_to_true_and_false_override_wins() -> None:
    base = PushPolicy()
    assert base.include_comments is True

    disabled = base.merged_with(PolicyOverride(include_comments=False))
    assert disabled.include_comments is False
    assert disabled.on_missing is DeleteAction.DELETE

    # A sparse override inherits; an explicit False is not "unset".
    assert disabled.merged_with(PolicyOverride()).include_comments is False
    assert disabled.merged_with(PolicyOverride(include_comments=True)).include_comments is True
