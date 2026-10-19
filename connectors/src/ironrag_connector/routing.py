"""YAML routing config and per-kind policy overrides.

Routing config schema
=====================

::

    default:
      library: <workspace-slug>/<library-slug>

    rules:
      - description: "Free text for logs"
        match:                      # any keys; matched against ref.routing_facts
          shelf: engineering        # exact string equality
          tag: archive              # if fact is a list/tuple, membership
        target:
          library: <workspace-slug>/<library-slug>

    policies:                       # optional per-kind overrides
      page:
        on_missing: delete
      image:
        on_missing: ignore
      issue:
        include_comments: false     # strict boolean; default true

The ``match`` fact bag is whatever the adapter chose to emit from
:meth:`SourceAdapter.iter_items` via ``SourceItemRef.routing_facts``. The
framework does not reserve any key names; ``shelf``, ``book``, ``tag``
are conventional but not mandatory.

``policies.<kind>.include_comments`` decides whether that kind's document
embeds the comments the source attaches to it. It is resolved per kind like
the four actions and hot-reloads with the file, but it is read by the
adapter (through the live policy table :func:`build_app` hands its factory),
not by the orchestrator: flipping it changes the projection and change
token, so the next sweep rematerializes the affected items. A kind without a
comment concept has nothing to embed either way. ``policies`` keys are
checked against the kinds the adapter emits (:func:`require_policy_kinds`)
on load and on every reload, so a misspelt kind is an error rather than a
silently retained default.

Once the catalog snapshot is resolved, if the operator omits ``default`` AND
no rule matches an item, resolution raises the typed intentional-no-match
signal ``UnroutedError``. Ordinary ``POLICY`` materializations are recorded as
``unrouted`` and skipped; a ``REQUIRED`` complete projection instead removes
its prior exact-key route copies because retaining them would preserve bytes no
longer intentionally routed. An uninitialized snapshot and catalog/config
resolution failures raise ordinary ``RoutingError`` and remain nondestructive.
"""

from __future__ import annotations

import asyncio
from collections.abc import Awaitable, Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import UUID

import yaml
from pydantic import BaseModel, ConfigDict, Field, StrictBool, field_validator

from .observability import get_logger
from .policy import (
    DeleteAction,
    DuplicateContentAction,
    PolicyOverride,
    PushPolicy,
    UpdateAction,
    UpsertAction,
)
from .source import SourceItemRef

log = get_logger(__name__)


class StrictRoutingModel(BaseModel):
    model_config = ConfigDict(extra="forbid")


class RouteTarget(StrictRoutingModel):
    library: str

    @field_validator("library")
    @classmethod
    def _canonical_library_ref(cls, value: str) -> str:
        return normalize_library_ref(value)


class RouteRule(StrictRoutingModel):
    description: str | None = None
    match: dict[str, Any]
    target: RouteTarget

    @field_validator("match")
    @classmethod
    def _match_not_empty(cls, v: dict[str, Any]) -> dict[str, Any]:
        if not v:
            raise ValueError("rule.match must declare at least one criterion")
        return v


class DefaultRoute(RouteTarget):
    pass


class PolicyOverrideModel(StrictRoutingModel):
    on_new: UpsertAction | None = None
    on_changed: UpdateAction | None = None
    on_missing: DeleteAction | None = None
    on_duplicate_content: DuplicateContentAction | None = None
    # Strict: YAML already spells booleans, so "yes"/1 are typos, not values.
    include_comments: StrictBool | None = None

    def as_override(self) -> PolicyOverride:
        return PolicyOverride(
            on_new=self.on_new,
            on_changed=self.on_changed,
            on_missing=self.on_missing,
            on_duplicate_content=self.on_duplicate_content,
            include_comments=self.include_comments,
        )


class RoutingConfig(StrictRoutingModel):
    default: DefaultRoute | None = None
    rules: list[RouteRule] = Field(default_factory=list)
    policies: dict[str, PolicyOverrideModel] = Field(default_factory=dict)


@dataclass(frozen=True)
class ResolvedLibraryTarget:
    library_ref: str
    workspace_id: UUID
    library_id: UUID


LibraryResolver = Callable[[set[str]], Awaitable[Mapping[str, ResolvedLibraryTarget]]]


@dataclass(frozen=True)
class ResolvedRoute(ResolvedLibraryTarget):
    rule_description: str
    """Empty string means the default route was used."""


@dataclass
class PolicyOverrides:
    """Resolved per-kind policy table built from YAML + env defaults."""

    default: PushPolicy
    by_kind: dict[str, PushPolicy]

    def for_kind(self, kind: str) -> PushPolicy:
        return self.by_kind.get(kind, self.default)


class RoutingError(RuntimeError):
    """A routing snapshot or catalog target cannot be resolved safely."""


class UnroutedError(RoutingError):
    """No rule matched an item and the operator configured no default."""


def normalize_library_ref(value: str) -> str:
    """Validate the MCP-compatible ``<workspace>/<library>`` catalog ref."""
    normalized = value.strip()
    if not normalized:
        raise ValueError("library ref must not be empty")
    if normalized.count("/") != 1:
        raise ValueError("library ref must use exactly one '<workspace>/<library>' separator")
    workspace_slug, library_slug = (segment.strip() for segment in normalized.split("/", 1))
    if not workspace_slug or not library_slug:
        raise ValueError("library ref must use non-empty '<workspace>/<library>' slugs")
    return f"{workspace_slug}/{library_slug}"


def load_routing_config(path: Path) -> RoutingConfig:
    if not path.is_file():
        raise FileNotFoundError(f"routing config not found at {path}")
    raw = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(raw, dict):
        raise ValueError(f"routing config at {path} must be a YAML mapping")
    return RoutingConfig.model_validate(raw)


def require_policy_kinds(config: RoutingConfig, kinds: Sequence[str]) -> None:
    """Refuse ``policies`` entries for kinds the adapter never emits.

    A per-kind override only takes effect on the kind it names, so a misspelt
    kind would silently keep the default: retention where deletion was asked
    for, or comments embedded where ``include_comments: false`` was written.
    Both the initial load and every hot reload pass through here.
    """
    unknown = sorted(set(config.policies) - set(kinds))
    if unknown:
        raise RoutingError(
            f"routing policies name kinds this adapter never emits: {unknown}; "
            f"known kinds: {sorted(kinds)}"
        )


class Router:
    """Resolve source facts through a catalog-compiled routing snapshot."""

    def __init__(
        self,
        config: RoutingConfig,
        *,
        resolved_targets: Mapping[str, ResolvedLibraryTarget] | None = None,
    ) -> None:
        self._config = config
        self._resolved_targets: dict[str, ResolvedLibraryTarget] | None = None
        if resolved_targets is not None:
            self._resolved_targets = _validate_resolved_targets(config, resolved_targets)

    async def initialize(self, resolver: LibraryResolver) -> None:
        """Resolve the complete configured snapshot before routing any item."""
        self._resolved_targets = await _resolve_targets(self._config, resolver)

    def replace_config(
        self,
        config: RoutingConfig,
        resolved_targets: Mapping[str, ResolvedLibraryTarget],
    ) -> None:
        """Atomically swap a fully resolved routing snapshot."""
        validated = _validate_resolved_targets(config, resolved_targets)
        self._config = config
        self._resolved_targets = validated

    def target_library_refs(self) -> set[str]:
        return _configured_library_refs(self._config)

    def target_libraries(self) -> set[UUID]:
        targets = self._require_resolved_targets()
        return {target.library_id for target in targets.values()}

    def resolve(self, ref: SourceItemRef) -> ResolvedRoute:
        self._require_resolved_targets()
        facts = ref.routing_facts or {}
        for rule in self._config.rules:
            if _facts_match(rule.match, facts):
                return self._resolved_route(
                    rule.target.library,
                    rule.description or "<unnamed rule>",
                )
        if self._config.default is None:
            raise UnroutedError(f"no rule matched {ref.kind}:{ref.item_id} and no default is set")
        return self._resolved_route(self._config.default.library, "")

    def _resolved_route(self, library_ref: str, description: str) -> ResolvedRoute:
        target = self._require_resolved_targets().get(library_ref)
        if target is None:
            raise RoutingError(f"library ref '{library_ref}' is not resolved")
        return ResolvedRoute(
            library_ref=target.library_ref,
            workspace_id=target.workspace_id,
            library_id=target.library_id,
            rule_description=description,
        )

    def _require_resolved_targets(self) -> dict[str, ResolvedLibraryTarget]:
        if self._resolved_targets is None:
            raise RoutingError("routing catalog targets are not resolved")
        return self._resolved_targets

    def build_policies(self, defaults: PushPolicy) -> PolicyOverrides:
        by_kind = {
            kind: defaults.merged_with(model.as_override())
            for kind, model in self._config.policies.items()
        }
        return PolicyOverrides(default=defaults, by_kind=by_kind)


class RoutingReloader:
    """Reload routing.yaml into the framework-owned router when it changes."""

    def __init__(
        self,
        *,
        path: Path,
        router: Router,
        policies: PolicyOverrides,
        defaults: PushPolicy,
        kinds: Sequence[str],
        resolver: LibraryResolver,
    ) -> None:
        self._path = path
        self._router = router
        self._policies = policies
        self._defaults = defaults
        self._kinds = tuple(kinds)
        self._resolver = resolver
        self._mtime_ns = _mtime_ns(path)
        self._reload_lock = asyncio.Lock()

    async def reload_if_changed(self) -> bool:
        async with self._reload_lock:
            return await self._reload_if_changed_locked()

    async def _reload_if_changed_locked(self) -> bool:
        try:
            current_mtime_ns = _mtime_ns(self._path)
        except OSError as exc:
            log.error(
                "routing.reload_error",
                path=str(self._path),
                error_type=type(exc).__name__,
                error=str(exc) or repr(exc),
            )
            return False
        if current_mtime_ns == self._mtime_ns:
            return False
        try:
            config = load_routing_config(self._path)
            require_policy_kinds(config, self._kinds)
            resolved_targets = await _resolve_targets(config, self._resolver)
        except Exception as exc:
            log.error(
                "routing.reload_error",
                path=str(self._path),
                error_type=type(exc).__name__,
                error=str(exc) or repr(exc),
            )
            return False
        self._router.replace_config(config, resolved_targets)
        updated = self._router.build_policies(self._defaults)
        self._policies.default = updated.default
        self._policies.by_kind = updated.by_kind
        self._mtime_ns = current_mtime_ns
        log.info(
            "routing.reloaded",
            path=str(self._path),
            rules=len(config.rules),
            has_default=config.default is not None,
            policy_overrides=list(config.policies.keys()),
        )
        return True


def _facts_match(criteria: dict[str, Any], facts: dict[str, Any]) -> bool:
    for key, expected in criteria.items():
        actual = facts.get(key)
        if actual is None:
            return False
        if isinstance(actual, (list, tuple, set)):
            if expected not in actual:
                return False
        else:
            if actual != expected:
                return False
    return True


def _configured_library_refs(config: RoutingConfig) -> set[str]:
    refs = {rule.target.library for rule in config.rules}
    if config.default is not None:
        refs.add(config.default.library)
    return refs


async def _resolve_targets(
    config: RoutingConfig,
    resolver: LibraryResolver,
) -> dict[str, ResolvedLibraryTarget]:
    refs = _configured_library_refs(config)
    resolved = await resolver(refs)
    return _validate_resolved_targets(config, resolved)


def _validate_resolved_targets(
    config: RoutingConfig,
    targets: Mapping[str, ResolvedLibraryTarget],
) -> dict[str, ResolvedLibraryTarget]:
    expected = _configured_library_refs(config)
    actual = set(targets)
    if actual != expected:
        missing = sorted(expected - actual)
        unexpected = sorted(actual - expected)
        raise RoutingError(
            "catalog resolver returned an incomplete snapshot "
            f"(missing={missing}, unexpected={unexpected})"
        )
    result = dict(targets)
    for library_ref, target in result.items():
        if target.library_ref != library_ref:
            raise RoutingError(
                f"catalog resolver returned '{target.library_ref}' for '{library_ref}'"
            )
    return result


def _mtime_ns(path: Path) -> int:
    return path.stat().st_mtime_ns
