"""Single-instance pidfile lock.

Prevents two copies of the connector from sweeping the same library
concurrently. Acquired in the FastAPI lifespan startup hook and
released on shutdown.
"""

from __future__ import annotations

import os
from pathlib import Path

from .observability import get_logger

log = get_logger(__name__)


class PidfileBusyError(RuntimeError):
    pass


class PidfileLock:
    def __init__(self, path: Path) -> None:
        self._path = path
        self._pid = os.getpid()
        self._held = False

    def acquire(self) -> None:
        if self._path.exists():
            try:
                old_pid = int(self._path.read_text().strip())
            except (ValueError, OSError):
                old_pid = -1
            if old_pid > 0:
                try:
                    os.kill(old_pid, 0)
                    raise PidfileBusyError(
                        f"another connector is already running (pid={old_pid}); "
                        f"remove {self._path} if that process is stale"
                    )
                except ProcessLookupError:
                    pass
            self._path.unlink(missing_ok=True)
        self._path.parent.mkdir(parents=True, exist_ok=True)
        self._path.write_text(str(self._pid))
        self._held = True
        log.info("pidfile.acquired", path=str(self._path), pid=self._pid)

    def release(self) -> None:
        if not self._held:
            return
        try:
            if self._path.exists() and self._path.read_text().strip() == str(self._pid):
                self._path.unlink(missing_ok=True)
                log.info("pidfile.released", path=str(self._path), pid=self._pid)
        except OSError as exc:
            log.warning("pidfile.release_error", error=str(exc))
        self._held = False
