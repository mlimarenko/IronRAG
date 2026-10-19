"""`ironrag-connector <source>` — the one entry point every image runs.

The source is an argument rather than a script name because PEP 621 cannot
bake an argument into a console script: five scripts would mean five wrapper
functions, which is the duplication this distribution exists to remove.

Order matters. The plugin is resolved *first*, and only the resolved plugin's
settings class is instantiated. Every source reads the same unprefixed
`RUN_MODE`, and YouTrack rejects `webhook`/`both` outright — validating all
five would make one source's environment reject another source's start-up.
The adapter itself is built inside :func:`build_app`, after routing has been
loaded, because the plugin factory receives the live per-kind policy table.
"""

from __future__ import annotations

import argparse
from collections.abc import Sequence

import uvicorn

from .orchestrator import Orchestrator
from .registry import available_sources, load_source
from .server import WebhookHandler, build_app
from .source import SourceAdapter


def _parse(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="ironrag-connector",
        description="Run one IronRAG source connector.",
    )
    parser.add_argument(
        "source",
        nargs="?",
        help=f"source to run; one of: {', '.join(available_sources())}",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the sources this build ships and exit",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> None:
    args = _parse(argv)
    if args.list:
        for name in available_sources():
            print(name)
        return
    if not args.source:
        raise SystemExit(
            "ironrag-connector needs a source; one of: "
            f"{', '.join(available_sources())} (or --list)"
        )

    plugin = load_source(args.source)
    settings = plugin.settings_cls()

    make_handler = plugin.webhook_factory
    webhook_factory = None
    if make_handler is not None:

        def webhook_factory(
            adapter: SourceAdapter, orchestrator: Orchestrator
        ) -> list[WebhookHandler]:
            return [make_handler(settings, adapter, orchestrator)]

    app = build_app(settings, plugin.adapter_factory, webhook_factory=webhook_factory)
    uvicorn.run(
        app,
        host=settings.host,
        port=settings.port,
        log_level=settings.log_level,
    )
