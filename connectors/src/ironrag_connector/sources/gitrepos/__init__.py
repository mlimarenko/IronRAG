"""Git-repository connector for IronRAG."""

from __future__ import annotations

from ...registry import SourcePlugin
from ...routing import PolicyOverrides
from .adapter import GitReposAdapter
from .config import GitReposSettings


def build_gitrepos_adapter(
    settings: GitReposSettings,
    _policies: PolicyOverrides,
) -> GitReposAdapter:
    """The registry factory; this source reads no per-kind policy."""
    return GitReposAdapter(settings)


PLUGIN = SourcePlugin("gitrepos", GitReposSettings, build_gitrepos_adapter)

__all__ = ["PLUGIN", "GitReposAdapter", "GitReposSettings"]
