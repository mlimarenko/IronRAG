"""Make the sibling test directories importable by name.

Tests are flat modules with unique basenames rather than packages, so a helper
in `tests/e2e/` (the full-stack bootstrap) can be imported by a unit test in
`tests/sources/`. pytest only inserts the directory of a *collected* file, and
`-k` or a path argument can leave one of them out — which would turn a missing
helper into a collection error that looks like a broken test rather than a
missing sys.path entry.
"""

from __future__ import annotations

import sys
from pathlib import Path

for directory in ("sdk", "sources", "e2e"):
    path = str(Path(__file__).parent / directory)
    if path not in sys.path:
        sys.path.insert(0, path)
