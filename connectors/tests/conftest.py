"""Make the sibling test directories importable by name.

Tests are flat modules with unique basenames rather than packages, so a helper
in `tests/e2e/` (the full-stack bootstrap) can be imported by a unit test in
`tests/sources/`. pytest only inserts the directory of a *collected* file, and
`-k` or a path argument can leave one of them out — which would turn a missing
helper into a collection error that looks like a broken test rather than a
missing sys.path entry.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
from pathlib import Path

import pytest

for directory in ("sdk", "sources", "e2e"):
    path = str(Path(__file__).parent / directory)
    if path not in sys.path:
        sys.path.insert(0, path)


@pytest.fixture(scope="session")
def native_content_receipts() -> dict[str, bytes]:
    """Serialize real public Rust DTOs once; preserve each envelope's native bytes."""
    process = subprocess.Popen(
        ["make", "--no-print-directory", "backend-content-receipt-fixture"],
        cwd=Path(__file__).resolve().parents[2],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=1800)
    except subprocess.TimeoutExpired:
        # Terminate this fixture's process group, not just make: its Cargo
        # admission wrapper must receive the signal and reap its build tree.
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
        raise
    if process.returncode:
        raise subprocess.CalledProcessError(process.returncode, process.args, stdout, stderr)
    return {name: body.encode() for name, body in json.loads(stdout).items()}
