from __future__ import annotations

import os
from pathlib import Path
from uuid import UUID

import pytest
from pydantic import ValidationError

from ironrag_connector.policy import DeleteAction, PushPolicy
from ironrag_connector.routing import (
    LibraryResolver,
    PolicyOverrides,
    ResolvedLibraryTarget,
    Router,
    RoutingConfig,
    RoutingError,
    RoutingReloader,
    UnroutedError,
    load_routing_config,
    require_policy_kinds,
)
from ironrag_connector.source import SourceItemRef

WS_DEFAULT = UUID("00000000-0000-0000-0000-000000000099")
WS_PARTNER = UUID("00000000-0000-0000-0000-000000000098")
LIB_DEFAULT = UUID("00000000-0000-0000-0000-000000000000")
LIB_ENG = UUID("00000000-0000-0000-0000-000000000001")
LIB_ARCHIVE = UUID("00000000-0000-0000-0000-000000000002")

REF_DEFAULT = "main/product-docs"
REF_ENG = "main/engineering"
REF_ARCHIVE = "partner/archive"


def _ref(item_id: str, facts: dict[str, object] | None = None) -> SourceItemRef:
    return SourceItemRef(
        item_id=item_id,
        kind="page",
        external_key=f"x:page:{item_id}",
        change_token=None,
        routing_facts=facts or {},
    )


def _config() -> RoutingConfig:
    return RoutingConfig.model_validate(
        {
            "default": {"library": REF_DEFAULT},
            "rules": [
                {
                    "description": "engineering shelf",
                    "match": {"shelf": "engineering"},
                    "target": {"library": REF_ENG},
                },
                {
                    "description": "archive tag",
                    "match": {"tag": "archive"},
                    "target": {"library": REF_ARCHIVE},
                },
            ],
            "policies": {
                "page": {"on_missing": "delete"},
                "image": {"on_missing": "ignore"},
            },
        }
    )


def _targets() -> dict[str, ResolvedLibraryTarget]:
    return {
        REF_DEFAULT: ResolvedLibraryTarget(
            library_ref=REF_DEFAULT,
            workspace_id=WS_DEFAULT,
            library_id=LIB_DEFAULT,
        ),
        REF_ENG: ResolvedLibraryTarget(
            library_ref=REF_ENG,
            workspace_id=WS_DEFAULT,
            library_id=LIB_ENG,
        ),
        REF_ARCHIVE: ResolvedLibraryTarget(
            library_ref=REF_ARCHIVE,
            workspace_id=WS_PARTNER,
            library_id=LIB_ARCHIVE,
        ),
    }


def _router() -> Router:
    return Router(_config(), resolved_targets=_targets())


def test_rule_match_wins_over_default() -> None:
    resolved = _router().resolve(_ref("1", {"shelf": "engineering"}))
    assert resolved.workspace_id == WS_DEFAULT
    assert resolved.library_id == LIB_ENG
    assert resolved.library_ref == REF_ENG
    assert resolved.rule_description == "engineering shelf"


def test_default_falls_back_when_no_rule_matches() -> None:
    resolved = _router().resolve(_ref("2", {"shelf": "random"}))
    assert resolved.library_id == LIB_DEFAULT
    assert resolved.library_ref == REF_DEFAULT
    assert resolved.rule_description == ""


def test_facts_list_membership() -> None:
    resolved = _router().resolve(_ref("3", {"tag": ["other", "archive"]}))
    assert resolved.library_id == LIB_ARCHIVE


def test_missing_default_and_no_match_raises() -> None:
    cfg = RoutingConfig.model_validate(
        {"rules": [{"match": {"shelf": "x"}, "target": {"library": REF_ENG}}]}
    )
    router = Router(cfg, resolved_targets={REF_ENG: _targets()[REF_ENG]})
    ref = _ref("4", {})
    with pytest.raises(UnroutedError):
        router.resolve(ref)


def test_uninitialized_router_does_not_classify_no_match_as_intentional() -> None:
    cfg = RoutingConfig.model_validate(
        {"rules": [{"match": {"shelf": "x"}, "target": {"library": REF_ENG}}]}
    )
    router = Router(cfg)
    ref = _ref("4", {})

    with pytest.raises(RoutingError, match="not resolved") as exc_info:
        router.resolve(ref)

    assert not isinstance(exc_info.value, UnroutedError)


@pytest.mark.parametrize(
    "target",
    [
        {"workspace": str(WS_DEFAULT), "library": str(LIB_DEFAULT)},
        {"library": str(LIB_DEFAULT)},
        {"library": "main"},
        {"library": "main/product-docs/extra"},
        {"library": " /product-docs"},
    ],
)
def test_legacy_or_malformed_targets_are_rejected(target: dict[str, str]) -> None:
    with pytest.raises(ValidationError):
        RoutingConfig.model_validate({"default": target})


def test_unknown_routing_keys_are_rejected() -> None:
    with pytest.raises(ValidationError, match="extra_forbidden"):
        RoutingConfig.model_validate(
            {"default": {"library": REF_DEFAULT}, "fallback": "silently-ignored-before"}
        )


def test_router_requires_catalog_resolution_before_use() -> None:
    router = Router(_config())
    ref = _ref("1")
    with pytest.raises(RoutingError, match="not resolved") as exc_info:
        router.resolve(ref)
    assert not isinstance(exc_info.value, UnroutedError)


@pytest.mark.asyncio
async def test_router_resolves_all_unique_library_refs_in_one_snapshot() -> None:
    calls: list[set[str]] = []

    async def resolve(refs: set[str]) -> dict[str, ResolvedLibraryTarget]:
        calls.append(refs)
        return _targets()

    router = Router(_config())
    await router.initialize(resolve)

    assert calls == [{REF_DEFAULT, REF_ENG, REF_ARCHIVE}]
    assert router.target_libraries() == {LIB_DEFAULT, LIB_ENG, LIB_ARCHIVE}


def test_policy_overrides_applied(tmp_path: Path) -> None:
    yaml_path = tmp_path / "r.yaml"
    yaml_path.write_text(
        f"""
default: {{ library: {REF_DEFAULT} }}
policies:
  image:
    on_missing: ignore
""".strip()
    )
    cfg = load_routing_config(yaml_path)
    router = Router(cfg, resolved_targets={REF_DEFAULT: _targets()[REF_DEFAULT]})
    defaults = PushPolicy()
    table = router.build_policies(defaults)
    assert table.for_kind("page").on_missing is DeleteAction.DELETE
    assert table.for_kind("image").on_missing is DeleteAction.IGNORE


def test_include_comments_is_a_per_kind_policy_that_defaults_to_true(tmp_path: Path) -> None:
    yaml_path = tmp_path / "r.yaml"
    yaml_path.write_text(
        f"""
default: {{ library: {REF_DEFAULT} }}
policies:
  issue:
    include_comments: false
  article:
    include_comments: true
  image:
    on_missing: ignore
""".strip()
    )
    cfg = load_routing_config(yaml_path)
    router = Router(cfg, resolved_targets={REF_DEFAULT: _targets()[REF_DEFAULT]})
    table = router.build_policies(PushPolicy())

    assert table.for_kind("issue").include_comments is False
    assert table.for_kind("issue").on_missing is DeleteAction.DELETE
    assert table.for_kind("article").include_comments is True
    # A kind with other overrides, and a kind with no overrides, keep the default.
    assert table.for_kind("image").include_comments is True
    assert table.for_kind("page").include_comments is True


@pytest.mark.parametrize("value", ["yes", "false", "maybe", 0, 1, [True], {"issue": False}])
def test_include_comments_rejects_non_boolean_values(value: object) -> None:
    with pytest.raises(ValidationError, match="include_comments"):
        RoutingConfig.model_validate(
            {
                "default": {"library": REF_DEFAULT},
                "policies": {"issue": {"include_comments": value}},
            }
        )


@pytest.mark.asyncio
async def test_routing_reloader_flips_include_comments_in_place(tmp_path: Path) -> None:
    yaml_path, _unused_router, policies, reloader = _reloader(
        tmp_path, kinds=("page", "image", "issue", "article")
    )
    assert policies.for_kind("issue").include_comments is True

    yaml_path.write_text(
        f"""
default: {{ library: {REF_DEFAULT} }}
policies:
  issue:
    include_comments: false
""".strip(),
        encoding="utf-8",
    )
    _bump_mtime(yaml_path)

    assert await reloader.reload_if_changed() is True
    # The same table object every holder was handed now answers differently.
    assert policies.for_kind("issue").include_comments is False
    assert policies.for_kind("article").include_comments is True


def test_policies_for_kinds_the_adapter_never_emits_are_rejected() -> None:
    cfg = RoutingConfig.model_validate(
        {
            "default": {"library": REF_DEFAULT},
            "policies": {"page": {"on_missing": "ignore"}, "pgae": {"include_comments": False}},
        }
    )

    with pytest.raises(RoutingError, match=r"pgae.*page") as exc_info:
        require_policy_kinds(cfg, ("page", "image"))
    assert not isinstance(exc_info.value, UnroutedError)

    require_policy_kinds(cfg, ("page", "pgae"))


@pytest.mark.asyncio
async def test_routing_reloader_rejects_policies_for_unknown_kinds(tmp_path: Path) -> None:
    yaml_path, router, policies, reloader = _reloader(tmp_path, kinds=("page",))
    yaml_path.write_text(
        f"default: {{ library: {REF_ENG} }}\npolicies:\n  pgae:\n    include_comments: false\n",
        encoding="utf-8",
    )
    _bump_mtime(yaml_path)

    assert await reloader.reload_if_changed() is False
    # Neither half of the snapshot moved: the typo cannot disable or retarget anything.
    assert router.resolve(_ref("1")).library_id == LIB_DEFAULT
    assert policies.for_kind("page").include_comments is True


@pytest.mark.asyncio
async def test_routing_reloader_updates_router_and_policy_table_atomically(
    tmp_path: Path,
) -> None:
    yaml_path, router, policies, reloader = _reloader(
        tmp_path, kinds=("page", "image", "issue", "article")
    )

    assert router.resolve(_ref("1")).library_id == LIB_DEFAULT
    assert policies.for_kind("image").on_missing is DeleteAction.DELETE

    yaml_path.write_text(
        f"""
default: {{ library: {REF_ENG} }}
policies:
  image:
    on_missing: ignore
""".strip(),
        encoding="utf-8",
    )
    _bump_mtime(yaml_path)

    assert await reloader.reload_if_changed() is True
    assert router.resolve(_ref("1")).library_id == LIB_ENG
    assert policies.for_kind("image").on_missing is DeleteAction.IGNORE


@pytest.mark.asyncio
async def test_routing_reloader_keeps_previous_snapshot_when_resolution_fails(
    tmp_path: Path,
) -> None:
    attempts = 0

    async def fail_once(refs: set[str]) -> dict[str, ResolvedLibraryTarget]:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise RoutingError("catalog ref is not visible")
        return await _resolve_all(refs)

    yaml_path, router, _policies, reloader = _reloader(
        tmp_path, kinds=("page", "image", "issue", "article"), resolver=fail_once
    )

    yaml_path.write_text(f"default: {{ library: {REF_ENG} }}\n", encoding="utf-8")
    _bump_mtime(yaml_path)

    assert await reloader.reload_if_changed() is False
    assert router.resolve(_ref("1")).library_id == LIB_DEFAULT

    # Failed candidates do not advance the accepted mtime, so the same file
    # is retried and can become active after a transient catalog outage.
    assert await reloader.reload_if_changed() is True
    assert router.resolve(_ref("1")).library_id == LIB_ENG


async def _resolve_all(refs: set[str]) -> dict[str, ResolvedLibraryTarget]:
    return {ref: _targets()[ref] for ref in refs}


def _reloader(
    tmp_path: Path,
    *,
    kinds: tuple[str, ...],
    resolver: LibraryResolver = _resolve_all,
) -> tuple[Path, Router, PolicyOverrides, RoutingReloader]:
    """A routing file at the default route, its router, live policy table and reloader."""
    yaml_path = tmp_path / "routing.yaml"
    yaml_path.write_text(f"default: {{ library: {REF_DEFAULT} }}\n", encoding="utf-8")
    router = Router(
        load_routing_config(yaml_path),
        resolved_targets={REF_DEFAULT: _targets()[REF_DEFAULT]},
    )
    policies = router.build_policies(PushPolicy())
    reloader = RoutingReloader(
        path=yaml_path,
        router=router,
        policies=policies,
        defaults=PushPolicy(),
        kinds=kinds,
        resolver=resolver,
    )
    return yaml_path, router, policies, reloader


def _bump_mtime(path: Path) -> None:
    next_mtime_ns = path.stat().st_mtime_ns + 1_000_000
    os.utime(path, ns=(next_mtime_ns, next_mtime_ns))
