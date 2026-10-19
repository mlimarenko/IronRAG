"""What build this connector is, as one answerable question.

`/health` reporting only "ok" made "which version is actually running" a
question nobody could answer from the outside — which is how a compose file
kept serving a two-minor-old image without anyone noticing.

The build stamp wins over the installed distribution's metadata. In an image
both are present, and they can disagree: the distribution version is whatever
`pyproject.toml` said when the wheel was built, while `IRONRAG_CONNECTOR_VERSION`
carries the release tag the image was actually built and published under. The
tag is the answer to "which build is this".
"""

from __future__ import annotations

import os
from importlib.metadata import PackageNotFoundError
from importlib.metadata import version as distribution_version

DISTRIBUTION = "ironrag-connectors"
VERSION_ENV = "IRONRAG_CONNECTOR_VERSION"
UNKNOWN = "unknown"


def connector_version() -> str:
    """The running build's version: the build stamp, else the installed
    distribution, else `unknown`."""
    stamped = os.environ.get(VERSION_ENV, "").strip()
    if stamped:
        return stamped
    try:
        return distribution_version(DISTRIBUTION)
    except PackageNotFoundError:
        return UNKNOWN
