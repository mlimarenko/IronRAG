"""YouTrack Knowledge Base connector for IronRAG."""

from __future__ import annotations

from ...registry import SourcePlugin
from ...routing import PolicyOverrides
from .adapter import YouTrackAdapter
from .config import YouTrackSettings


def build_youtrack_adapter(
    settings: YouTrackSettings,
    policies: PolicyOverrides,
) -> YouTrackAdapter:
    """The registry factory; comment sync is read from the per-kind policy table."""
    return YouTrackAdapter(settings, policies)


PLUGIN = SourcePlugin("youtrack", YouTrackSettings, build_youtrack_adapter)

__all__ = ["PLUGIN", "YouTrackAdapter", "YouTrackSettings"]
