"""External-key encoding for the git-repository connector.

A git connector pushes one IronRAG document per (repo, branch, path)
triplet. The triplet is the stable sync identity: rename a repo slug
and IronRAG sees an orphan + a new doc; move a file inside the tree
and the same happens. This is intentional — file moves should be
treated as delete + create at the RAG level so chunks/embeddings
recompute against the new location vocabulary.

External-key layout::

    gitrepos:file:<repo>:<branch>:<path>

`repo`, `branch`, `path` are URL-encoded (RFC 3986 unreserved) so the
colon separator never collides with content. `parse_external_key`
inverts the encoding.
"""

from __future__ import annotations

from urllib.parse import quote, unquote

CONNECTOR_NAME = "gitrepos"
KIND_FILE = "file"
KINDS: tuple[str, ...] = (KIND_FILE,)

_SAFE = ""  # quote everything except RFC3986 unreserved (alpha/digit/-/_/./~)


def _enc(part: str) -> str:
    return quote(part, safe=_SAFE)


def _dec(part: str) -> str:
    return unquote(part)


def file_item_id(repo_slug: str, branch: str, path: str) -> str:
    """Stable, ASCII-safe identity for one (repo, branch, path) triple.

    The framework's default idempotency-key builder substitutes
    ``item_id`` directly into a Postgres-bound string. Postgres TEXT
    rejects NUL bytes, so the separator must be a printable ASCII
    character that cannot appear inside any component. We URL-encode
    each component and join with ``:`` — the same encoding the
    external_key uses, minus the ``gitrepos:file:`` prefix.
    """
    return f"{_enc(repo_slug)}:{_enc(branch)}:{_enc(path)}"


def parse_file_item_id(item_id: str) -> tuple[str, str, str] | None:
    parts = item_id.split(":", 2)
    if len(parts) != 3:
        return None
    repo_enc, branch_enc, path_enc = parts
    if not repo_enc or not branch_enc or not path_enc:
        return None
    try:
        return _dec(repo_enc), _dec(branch_enc), _dec(path_enc)
    except ValueError:
        return None


def file_external_key(repo_slug: str, branch: str, path: str) -> str:
    return f"{CONNECTOR_NAME}:{KIND_FILE}:{_enc(repo_slug)}:{_enc(branch)}:{_enc(path)}"


def build_external_key(kind: str, item_id: str) -> str:
    if kind != KIND_FILE:
        raise ValueError(f"unknown gitrepos kind: {kind}")
    # Empty item_id is the reaper's way of asking for the kind's
    # external-key prefix, which it uses to list IronRAG documents
    # belonging to this adapter.
    if item_id == "":
        return f"{CONNECTOR_NAME}:{KIND_FILE}:"
    triple = parse_file_item_id(item_id)
    if triple is None:
        raise ValueError(f"malformed gitrepos item_id: {item_id!r}")
    repo, branch, path = triple
    return file_external_key(repo, branch, path)


def parse_external_key(external_key: str) -> tuple[str, str] | None:
    parts = external_key.split(":", 4)
    if len(parts) != 5:
        return None
    source, kind, repo_enc, branch_enc, path_enc = parts
    if source != CONNECTOR_NAME or kind != KIND_FILE:
        return None
    try:
        repo = _dec(repo_enc)
        branch = _dec(branch_enc)
        path = _dec(path_enc)
    except ValueError:
        return None
    if not repo or not branch or not path:
        return None
    return kind, file_item_id(repo, branch, path)
