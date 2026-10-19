"""Per-kind push policies.

Every source item has a ``kind`` (page, attachment, image, …). The
operator can choose, for each kind:

* :class:`UpsertAction` — what to do for items the connector has never
  pushed before. ``create`` uploads them; ``skip`` ignores them entirely.
* :class:`UpdateAction` — what to do when an existing item's
  ``change_token`` advanced. ``replace`` pushes a new revision; ``skip``
  leaves the IronRAG document as it was.
* :class:`DeleteAction` — what to do when an item that the framework
  previously saw is no longer in the source. ``delete`` soft-deletes the
  IronRAG document via DELETE /v1/content/documents/{id}; ``ignore``
  leaves it in place.

A fourth knob, ``on_duplicate_content``, decides how the framework reacts
when IronRAG returns 409 ``duplicate content`` on upload — typically the
same byte sequence is already in the library under a different
``external_key``. ``skip`` treats that as a success (the canonical
behavior; identical bytes get deduped by IronRAG); ``fail`` raises so the
operator sees the conflict.

All actions are explicit strings rather than booleans so the YAML config
reads symmetrically and new actions can be added without touching the
schema.

``include_comments`` is the one materialization field: whether a kind's
document embeds the comments the source attaches to it. The framework only
carries it; the adapter that materializes the kind reads it through the
policy table it was bound to, so a routing hot reload changes the next
sweep's projection and change tokens. Its default is fixed to ``true`` and
has no environment counterpart -- ``routing.yaml`` is the only place it is
set.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import StrEnum


class UpsertAction(StrEnum):
    CREATE = "create"
    SKIP = "skip"


class UpdateAction(StrEnum):
    REPLACE = "replace"
    SKIP = "skip"


class DeleteAction(StrEnum):
    DELETE = "delete"
    IGNORE = "ignore"


class DuplicateContentAction(StrEnum):
    SKIP = "skip"
    FAIL = "fail"


@dataclass(frozen=True)
class PushPolicy:
    """Resolved policy for one specific ``kind``."""

    on_new: UpsertAction = UpsertAction.CREATE
    on_changed: UpdateAction = UpdateAction.REPLACE
    on_missing: DeleteAction = DeleteAction.DELETE
    on_duplicate_content: DuplicateContentAction = DuplicateContentAction.SKIP
    include_comments: bool = True

    def merged_with(self, overrides: PolicyOverride) -> PushPolicy:
        return PushPolicy(
            on_new=overrides.on_new or self.on_new,
            on_changed=overrides.on_changed or self.on_changed,
            on_missing=overrides.on_missing or self.on_missing,
            on_duplicate_content=overrides.on_duplicate_content or self.on_duplicate_content,
            include_comments=(
                self.include_comments
                if overrides.include_comments is None
                else overrides.include_comments
            ),
        )


@dataclass(frozen=True)
class PolicyOverride:
    """Sparse overrides for one kind. Any None field inherits."""

    on_new: UpsertAction | None = None
    on_changed: UpdateAction | None = None
    on_missing: DeleteAction | None = None
    on_duplicate_content: DuplicateContentAction | None = None
    include_comments: bool | None = None
