"""SourceAdapter for git repositories.

What the adapter emits
======================

One ``file`` ref per (repo, branch, path) triple visible in the tracked
repos. ``routing_facts`` exposes:

* ``repo``       — repo slug from ``repos.yaml``
* ``branch``     — branch name as listed in ``repos.yaml``
* ``path``       — full path inside the repo
* ``dir``        — parent directory of ``path``
* ``ext``        — file extension (without dot, lowercase)
* ``top_dir``    — first path segment (handy for shallow filters)
* any custom keys from the repo's ``facts:`` block

Census and reaping
==================

The census unit is one ``(repo, branch)`` scope. A scope whose clone, fetch
or tree listing fails is logged and skipped so the other scopes still ship
their updates, and ``iter_items`` then ends with
:class:`~ironrag_connector.IncompleteCensusError` naming every failed scope:
the framework pushes what was enumerated and does not reap that sweep.

Operator admission rules — ``include``/``exclude`` globs, ``max_file_bytes``
(global and per repo), the repo and branch lists — define the census scope.
The census is complete over that scope, so a previously indexed file that
leaves it (a file that grew past the ceiling, a path newly matched by an
exclude, a branch dropped from ``repos.yaml``) is reaped or retained by the
``file`` kind's ``on_missing`` policy, exactly like a file deleted from the
branch. Content inside the scope that cannot be represented as text (a
binary blob) is a ``REQUIRED`` delete decided in :meth:`fetch`.

Change token
============

``change_token`` is the **blob sha** rather than the commit sha:
content-addressed, so a file edited in one branch and merged into a
second is treated as unchanged when the same blob is reached again.
"""

from __future__ import annotations

import mimetypes
from collections.abc import AsyncIterator, Iterator
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from ironrag_connector import (
    IncompleteCensusError,
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
)

from .config import GitReposSettings
from .git_client import GitClient, GitCommandError, GitError, TreeEntry
from .mapping import (
    CONNECTOR_NAME,
    KIND_FILE,
    KINDS,
    build_external_key,
    file_external_key,
    file_item_id,
    parse_external_key,
    parse_file_item_id,
)
from .observability import get_logger
from .repos import (
    DEFAULT_EXCLUDES,
    ReposConfig,
    RepoSpec,
    load_repos_config,
    should_include,
)

log = get_logger(__name__)


class GitReposAdapter(SourceAdapter):
    name = CONNECTOR_NAME
    kinds = KINDS
    primary_kinds = (KIND_FILE,)

    def __init__(
        self,
        settings: GitReposSettings,
        *,
        repos_config: ReposConfig | None = None,
        git_client: GitClient | None = None,
    ) -> None:
        self._settings = settings
        self._reload_repos_config = repos_config is None
        self._repos_mtime_ns: int | None = None
        if repos_config is None:
            self._repos = load_repos_config(settings.gitrepos_config_path)
            self._repos_mtime_ns = _mtime_ns(settings.gitrepos_config_path)
        else:
            self._repos = repos_config
        self._client = git_client or GitClient(
            settings.gitrepos_clone_root,
            fetch_timeout_seconds=settings.gitrepos_fetch_timeout_seconds,
            command_timeout_seconds=settings.gitrepos_command_timeout_seconds,
        )

    async def close(self) -> None:
        # GitClient holds no long-lived sockets — subprocess per call.
        return None

    def external_key(self, kind: str, item_id: str) -> str:
        return build_external_key(kind, item_id)

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return parse_external_key(external_key)

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind == KIND_FILE:
            return SourceDeleteEnforcement.POLICY
        raise ValueError(f"unsupported {self.name} item kind: {kind}")

    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        self._reload_config_if_changed()
        failed_scopes: list[str] = []
        total_scopes = 0
        for spec in self._repos.repos:
            total_scopes += len(spec.branches)
            try:
                await self._client.ensure_repo(spec)
            except GitError as exc:
                log.error(
                    "gitrepos.repo.ensure_failed",
                    repo=spec.name,
                    error=str(exc),
                )
                failed_scopes.extend(_scope(spec, branch) for branch in spec.branches)
                continue
            for branch in spec.branches:
                try:
                    entries = await self._client.list_tree(spec, branch)
                except GitError as exc:
                    log.error(
                        "gitrepos.branch.list_failed",
                        repo=spec.name,
                        branch=branch,
                        error=str(exc),
                    )
                    failed_scopes.append(_scope(spec, branch))
                    continue
                for ref in self._admitted_refs(spec, branch, entries):
                    yield ref
        if failed_scopes:
            raise IncompleteCensusError(
                f"{len(failed_scopes)} of {total_scopes} git scopes could not be enumerated",
                scopes=tuple(failed_scopes),
            )

    def _admitted_refs(
        self, spec: RepoSpec, branch: str, entries: list[TreeEntry]
    ) -> Iterator[SourceItemRef]:
        max_bytes = min(spec.max_file_bytes, self._settings.gitrepos_max_file_bytes)
        excludes = spec.effective_excludes()
        for entry in entries:
            if entry.size > max_bytes:
                log.info(
                    "gitrepos.file.skipped_too_large",
                    repo=spec.name,
                    branch=branch,
                    path=entry.path,
                    size=entry.size,
                    limit=max_bytes,
                )
                continue
            if not should_include(entry.path, spec.include, excludes):
                continue
            yield SourceItemRef(
                item_id=file_item_id(spec.name, branch, entry.path),
                kind=KIND_FILE,
                external_key=file_external_key(spec.name, branch, entry.path),
                change_token=entry.blob_sha,
                routing_facts=_routing_facts(spec, branch, entry.path),
                raw={
                    "repo": spec.name,
                    "branch": branch,
                    "path": entry.path,
                    "blob_sha": entry.blob_sha,
                    "size": entry.size,
                },
            )

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        if ref.kind != KIND_FILE:
            log.warning("gitrepos.fetch.unexpected_kind", kind=ref.kind)
            return SourceFetchDeferred(f"unexpected ref kind: {ref.kind}")
        triple = parse_file_item_id(ref.item_id)
        if triple is None:
            log.warning("gitrepos.fetch.bad_item_id", item_id=ref.item_id)
            return SourceFetchDeferred("malformed git file item id")
        repo_slug, branch, path = triple
        spec = self._spec_by_name(repo_slug)
        if spec is None:
            log.warning(
                "gitrepos.fetch.repo_removed",
                repo=repo_slug,
                detail="repo dropped from repos.yaml since sweep started",
            )
            return SourceFetchDelete(
                SourceDeleteEnforcement.POLICY,
                "repository was removed from connector configuration",
                cacheable=False,
            )
        sha = ref.change_token or ref.raw.get("blob_sha")
        if not sha:
            log.warning("gitrepos.fetch.no_blob_sha", repo=repo_slug, path=path)
            return SourceFetchDeferred("source ref has no blob sha")
        try:
            payload = await self._client.read_blob(spec, sha)
        except GitCommandError as exc:
            log.warning(
                "gitrepos.fetch.read_failed",
                repo=repo_slug,
                path=path,
                sha=sha,
                error=str(exc),
            )
            return SourceFetchDeferred("git blob read failed")
        if _looks_binary(payload):
            # Ingest every text extension but never binary blobs (images,
            # archives, compiled artifacts). Detection is content-based, not
            # an extension blocklist: a blob is binary iff it carries a NUL
            # byte in its first sniff window — the same heuristic git uses to
            # classify a blob for diffs. This stays language/extension
            # agnostic, so an unknown text extension is still ingested while
            # a mislabelled `.txt` that is actually binary is skipped.
            log.info(
                "gitrepos.file.skipped_binary",
                repo=repo_slug,
                branch=branch,
                path=path,
                size=len(payload),
            )
            return SourceFetchDelete(
                SourceDeleteEnforcement.REQUIRED,
                "blob is binary and cannot remain represented by stale indexed text",
                cacheable=True,
            )
        mime = _guess_mime(path)
        return SourceFetchMaterialized(
            SourceItem(
                ref=ref,
                payload=payload,
                mime_type=mime,
                file_name=Path(path).name,
                title=_title(spec.name, branch, path),
                document_hint=_document_hint(spec, branch, path),
                # No idempotency_key override: the framework scopes revisions
                # to this source identity and the rendered payload bytes.
            )
        )

    def _spec_by_name(self, name: str) -> RepoSpec | None:
        for spec in self._repos.repos:
            if spec.name == name:
                return spec
        return None

    def _reload_config_if_changed(self) -> None:
        if not self._reload_repos_config:
            return
        current_mtime_ns = _mtime_ns(self._settings.gitrepos_config_path)
        if current_mtime_ns == self._repos_mtime_ns:
            return
        self._repos = load_repos_config(self._settings.gitrepos_config_path)
        self._repos_mtime_ns = current_mtime_ns
        log.info(
            "gitrepos.config.reloaded",
            path=str(self._settings.gitrepos_config_path),
        )


def _scope(spec: RepoSpec, branch: str) -> str:
    return f"{spec.name}@{branch}"


def _routing_facts(spec: RepoSpec, branch: str, path: str) -> dict[str, Any]:
    p = Path(path)
    facts: dict[str, Any] = {
        "repo": spec.name,
        "branch": branch,
        "path": path,
        "dir": str(p.parent) if str(p.parent) != "." else "",
        "ext": p.suffix.lstrip(".").lower(),
        "top_dir": path.split("/", 1)[0] if "/" in path else "",
    }
    if spec.url:
        facts["repo_url"] = spec.url
    facts.update(spec.facts)
    return facts


def _mtime_ns(path: Path) -> int:
    return path.stat().st_mtime_ns


def _title(repo: str, branch: str, path: str) -> str:
    return f"{repo}:{branch} — {path}"


def _document_hint(spec: RepoSpec, branch: str, path: str) -> str | None:
    """Best-effort canonical URL for citations.

    For known forge hosts (github.com, gitlab.com, generic gitlab/gitea
    on self-hosted boxes) we synthesize a blob/branch link. For unknown
    hosts and local-path repos we fall back to ``repo:branch:path`` so
    citations still carry a stable label.
    """
    url = spec.url
    if not url:
        return f"{spec.name}:{branch}:{path}"
    # github.com / gitlab.com / gitea / generic — common pattern is
    # `<host>/<owner>/<repo>(/-)?/{blob,tree}/<branch>/<path>`. We
    # only handle the canonical http(s) forms; ssh urls become a label.
    web = _ssh_to_https(url)
    if not web:
        return f"{spec.name}:{branch}:{path}"
    base = web.removesuffix(".git")
    forge = _forge_of(base)
    if forge == "gitlab":
        return f"{base}/-/blob/{branch}/{path}"
    # github + gitea share the same path layout.
    return f"{base}/blob/{branch}/{path}"


def _ssh_to_https(url: str) -> str | None:
    if urlsplit(url).scheme in {"http", "https"}:
        return url
    # git@github.com:org/repo.git  -> https://github.com/org/repo.git
    if url.startswith("git@") and ":" in url:
        host_part, path_part = url[len("git@") :].split(":", 1)
        return f"https://{host_part}/{path_part}"
    if url.startswith("ssh://"):
        # ssh://git@host:port/org/repo.git
        rest = url[len("ssh://") :]
        if "@" in rest:
            rest = rest.split("@", 1)[1]
        if "/" not in rest:
            return None
        host, path_part = rest.split("/", 1)
        if ":" in host:
            host = host.split(":", 1)[0]
        return f"https://{host}/{path_part}"
    return None


def _forge_of(https_url: str) -> str:
    if "gitlab" in https_url:
        return "gitlab"
    if "bitbucket" in https_url:
        return "bitbucket"
    return "github"


_BINARY_SNIFF_BYTES = 8000
"""Window git itself inspects to classify a blob as binary."""


def _looks_binary(payload: bytes) -> bool:
    """A blob is binary iff it contains a NUL byte in its first sniff window.

    This mirrors git's own ``buffer_is_binary`` heuristic. It is
    encoding-agnostic: UTF-8, UTF-16-without-BOM aside, and legacy 8-bit
    text encodings (which never embed NUL) are treated as text, while
    images / archives / executables / compiled objects (which do) are
    treated as binary. No extension list is consulted.
    """
    return b"\x00" in payload[:_BINARY_SNIFF_BYTES]


def _guess_mime(path: str) -> str:
    # Binary blobs are already filtered out before this point, so an unknown
    # extension is text — fall back to text/plain rather than octet-stream.
    guessed, _ = mimetypes.guess_type(path)
    return guessed or "text/plain"


__all__ = ["DEFAULT_EXCLUDES", "GitReposAdapter"]
