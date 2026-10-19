"""Git-repository connector settings.

Inherits IronRAG creds, sync-loop tuning, run mode, state path,
admin bearer, pidfile, and server bind from
:class:`ironrag_connector.BaseConnectorSettings`. Adds:

* The path to ``repos.yaml`` that describes the tracked
  repositories.
* A clone root for bare clones of remote repos.
* Per-fetch timeouts so a stuck network does not pin the sweep.

There is no vendor base URL or vendor API token: a git connector
talks to remote hosts via plain ``git`` (SSH or https), and the
credentials it needs are exactly the credentials of the host user
under which the container runs. Mount your SSH key or git
credentials helper at runtime — no connector-level secrets to
manage.
"""

from __future__ import annotations

from pathlib import Path

from pydantic import Field

from ironrag_connector import BaseConnectorSettings


class GitReposSettings(BaseConnectorSettings):
    gitrepos_config_path: Path = Field(default=Path("repos.yaml"))
    gitrepos_clone_root: Path = Field(default=Path("./repo-cache"))
    gitrepos_fetch_timeout_seconds: float = Field(default=180.0, gt=0)
    gitrepos_command_timeout_seconds: float = Field(default=60.0, gt=0)
    gitrepos_max_file_bytes: int = Field(default=1_048_576, ge=1)
    """Hard ceiling on per-file payload size. A repo entry can lower this further."""
