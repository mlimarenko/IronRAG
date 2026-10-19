"""Thin async wrapper around the ``git`` CLI.

We shell out to ``git`` rather than depend on ``pygit2``/``GitPython``
to keep the dependency surface small and the container image lean.
All commands run under :func:`asyncio.create_subprocess_exec` so the
sync loop's bounded concurrency translates directly into bounded
git invocation parallelism.

Repository state lives in two flavors:

* **Remote-tracked**: bare clone at ``<clone_root>/<repo_slug>.git``.
  ``ensure_repo`` clones on first call, then fetches before every
  enumeration. Branches live in ``refs/heads/*`` — the namespace
  ``git clone --bare`` writes and a bare repository has no
  remote-tracking refspec for — and every fetch mirrors that namespace
  explicitly, so branch updates, new branches and upstream deletions all
  land where the enumerator resolves them.
* **Local path**: enumerated directly from the working tree. The
  current ``HEAD`` of each requested branch is read via
  ``git rev-parse``. No clone, no fetch.
"""

from __future__ import annotations

import asyncio
import shlex
from dataclasses import dataclass
from pathlib import Path

from .observability import get_logger
from .repos import RepoSpec

log = get_logger(__name__)

_BRANCH_REFSPEC = "+refs/heads/*:refs/heads/*"
"""Mirror refspec for a bare clone's branch namespace.

``git clone --bare`` writes branches to ``refs/heads/*`` and configures no
``remote.origin.fetch``, so a bare ``git fetch origin`` would update nothing.
Naming the refspec on every fetch keeps those refs current, prunes branches
deleted upstream and reaches clones made before this contract existed.
"""


class GitError(RuntimeError):
    """Base for every failure that makes a git read untrustworthy."""


class GitCommandError(GitError):
    """Raised when a ``git`` invocation exits non-zero."""

    def __init__(self, cmd: list[str], code: int, stderr: str) -> None:
        super().__init__(
            f"git command failed (exit {code}): {' '.join(shlex.quote(c) for c in cmd)}\n"
            f"stderr: {stderr.strip()}"
        )
        self.cmd = cmd
        self.code = code
        self.stderr = stderr


class GitTreeParseError(GitError):
    """``git ls-tree`` output contained a record the client could not trust.

    A tree listing that drops a record it cannot parse would still look like
    a complete census, so a malformed record fails the whole listing.
    """


@dataclass(frozen=True)
class TreeEntry:
    path: str
    blob_sha: str
    size: int


class GitClient:
    """Per-process git executor with a single clone root."""

    def __init__(
        self,
        clone_root: Path,
        *,
        fetch_timeout_seconds: float = 120.0,
        command_timeout_seconds: float = 60.0,
    ) -> None:
        self._clone_root = clone_root
        self._fetch_timeout = fetch_timeout_seconds
        self._cmd_timeout = command_timeout_seconds
        self._clone_root.mkdir(parents=True, exist_ok=True)
        self._locks: dict[str, asyncio.Lock] = {}

    def _git_dir(self, spec: RepoSpec) -> Path:
        if spec.local_path is not None:
            # Local working tree: git operations work against
            # `local_path` directly; .git is auto-resolved.
            return spec.local_path
        return self._clone_root / f"{spec.name}.git"

    def _is_bare(self, spec: RepoSpec) -> bool:
        return spec.local_path is None

    async def _run(
        self,
        cwd: Path,
        args: list[str],
        *,
        timeout_seconds: float | None = None,
        capture_stdout: bool = True,
    ) -> bytes:
        cmd = ["git", *args]
        try:
            proc = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=str(cwd),
                stdout=asyncio.subprocess.PIPE if capture_stdout else None,
                stderr=asyncio.subprocess.PIPE,
            )
        except OSError as exc:
            # A missing binary, an unreadable cwd or an exhausted process
            # table is the same "this git read did not happen" for callers.
            raise GitCommandError(cmd, -1, str(exc)) from exc
        try:
            async with asyncio.timeout(timeout_seconds or self._cmd_timeout):
                stdout, stderr = await proc.communicate()
        except TimeoutError:
            proc.kill()
            await proc.wait()
            raise GitCommandError(cmd, -1, "timeout") from None
        if proc.returncode != 0:
            raise GitCommandError(
                cmd,
                proc.returncode or -1,
                stderr.decode("utf-8", errors="replace"),
            )
        return stdout or b""

    def _lock_for(self, spec: RepoSpec) -> asyncio.Lock:
        # Serialize fetch/clone for one repo across concurrent sweeps.
        return self._locks.setdefault(spec.name, asyncio.Lock())

    async def ensure_repo(self, spec: RepoSpec) -> None:
        """Clone if missing, fetch otherwise. No-op for `local_path` repos."""
        if spec.local_path is not None:
            if not spec.local_path.is_dir():
                raise GitCommandError(
                    ["git", "rev-parse", "--git-dir"],
                    -1,
                    f"local_path {spec.local_path} does not exist",
                )
            return

        async with self._lock_for(spec):
            git_dir = self._git_dir(spec)
            if not git_dir.exists():
                log.info("gitrepos.clone.start", repo=spec.name, url=spec.url)
                await self._run(
                    self._clone_root,
                    [
                        "clone",
                        "--bare",
                        "--filter=blob:none",
                        spec.url or "",
                        str(git_dir),
                    ],
                    timeout_seconds=self._fetch_timeout,
                )
                log.info("gitrepos.clone.done", repo=spec.name)
            else:
                log.info("gitrepos.fetch.start", repo=spec.name)
                await self._run(
                    git_dir,
                    ["fetch", "--prune", "--prune-tags", "origin", _BRANCH_REFSPEC],
                    timeout_seconds=self._fetch_timeout,
                )
                log.info("gitrepos.fetch.done", repo=spec.name)

    async def head_sha(self, spec: RepoSpec, branch: str) -> str | None:
        """Resolve ``branch`` to a commit sha. ``None`` if branch missing."""
        ref = self._resolve_ref(spec, branch)
        try:
            out = await self._run(self._git_dir(spec), ["rev-parse", ref])
        except GitCommandError as exc:
            if "unknown revision" in exc.stderr.lower() or "ambiguous" in exc.stderr.lower():
                return None
            raise
        return out.decode().strip() or None

    async def list_tree(self, spec: RepoSpec, branch: str) -> list[TreeEntry]:
        """Return every blob reachable from ``branch``'s tree.

        The listing is a census: a record this parser cannot trust raises
        :class:`GitTreeParseError` rather than being dropped, because a
        dropped record would later read as a file that no longer exists.
        """
        ref = self._resolve_ref(spec, branch)
        out = await self._run(
            self._git_dir(spec),
            ["ls-tree", "-r", "-l", "-z", ref],
        )
        entries: list[TreeEntry] = []
        for chunk in out.split(b"\x00"):
            if not chunk:
                continue
            record = _parse_tree_record(chunk, spec=spec, branch=branch)
            if record is not None:
                entries.append(record)
        return entries

    async def read_blob(self, spec: RepoSpec, sha: str) -> bytes:
        return await self._run(self._git_dir(spec), ["cat-file", "blob", sha])

    def _resolve_ref(self, spec: RepoSpec, branch: str) -> str:
        """Bare clones keep branches at ``refs/heads/<branch>``, local tree uses raw branch."""
        if self._is_bare(spec):
            return f"refs/heads/{branch}"
        return branch


_TREE_OBJECT_TYPES = frozenset({b"blob", b"tree", b"commit"})
_OBJECT_ID_LENGTHS = frozenset({40, 64})
"""SHA-1 and SHA-256 object names; nothing else is a git object id."""


def _parse_tree_record(chunk: bytes, *, spec: RepoSpec, branch: str) -> TreeEntry | None:
    """One ``ls-tree -r -l -z`` record: ``<mode> SP <type> SP <sha> SP <size> TAB <path>``.

    Returns ``None`` for a well-formed record that is not a file: ``-r``
    already flattens trees, and a ``commit`` entry is a submodule pointer.
    A path that is not UTF-8 cannot become an IronRAG external key, so it is
    outside the representable scope and logged rather than indexed.
    """

    def malformed(detail: str) -> GitTreeParseError:
        return GitTreeParseError(
            f"malformed ls-tree record in {spec.name}@{branch}: {detail}: {chunk[:120]!r}"
        )

    meta, tab, path_bytes = chunk.partition(b"\t")
    if not tab or not path_bytes:
        raise malformed("no path")
    # `-l` right-aligns the size, so fields are separated by runs of blanks.
    parts = meta.split()
    if len(parts) != 4:
        raise malformed("expected mode, type, object id and size")
    mode, otype, sha, size = parts
    if not mode.isdigit() or otype not in _TREE_OBJECT_TYPES:
        raise malformed("unknown mode or object type")
    if len(sha) not in _OBJECT_ID_LENGTHS or not _is_hex(sha):
        raise malformed("object id is not a git object name")
    if otype != b"blob":
        return None
    if not size.isdigit():
        raise malformed("blob size is not an integer")
    try:
        path = path_bytes.decode("utf-8")
    except UnicodeDecodeError:
        log.warning("gitrepos.tree.path_not_utf8", repo=spec.name, branch=branch)
        return None
    return TreeEntry(path=path, blob_sha=sha.decode("ascii"), size=int(size))


def _is_hex(value: bytes) -> bool:
    return all(byte in b"0123456789abcdef" for byte in value)
