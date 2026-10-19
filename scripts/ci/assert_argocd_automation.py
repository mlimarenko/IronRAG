#!/usr/bin/env python3
"""Require Argo CD automated sync and self-healing before publishing GitOps changes."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import yaml

MAX_MANIFEST_BYTES = 256 * 1024


def refuse(message: str) -> None:
    print(f"Publish refused: {message}", file=sys.stderr)
    raise SystemExit(1)


def find_application(root: Path, name: str) -> Path:
    if not root.is_dir():
        refuse(f"{root} is not a directory — the manifests are not where expected.")
    matches = []
    for path in sorted(root.rglob("*.yaml")):
        if path.stat().st_size > MAX_MANIFEST_BYTES:
            continue
        try:
            documents = list(yaml.safe_load_all(path.read_text(encoding="utf-8")))
        except yaml.YAMLError:
            continue
        for document in documents:
            if (
                isinstance(document, dict)
                and document.get("kind") == "Application"
                and (document.get("metadata") or {}).get("name") == name
            ):
                matches.append(path)
                break
    if not matches:
        refuse(f"no Argo CD Application named {name!r} under {root}.")
    if len(matches) > 1:
        refuse(f"{len(matches)} Applications named {name!r} under {root}: " + ", ".join(str(m) for m in matches))
    return matches[0]


def automation_of(path: Path) -> dict[str, object]:
    if not path.is_file():
        refuse(f"{path} is missing — the Application it describes cannot be checked.")
    size = path.stat().st_size
    if size > MAX_MANIFEST_BYTES:
        refuse(f"{path} is {size} bytes, larger than an Application manifest should be.")

    documents = [d for d in yaml.safe_load_all(path.read_text(encoding="utf-8")) if isinstance(d, dict)]
    applications = [d for d in documents if d.get("kind") == "Application"]
    if len(applications) != 1:
        refuse(f"{path} holds {len(applications)} Argo CD Applications; expected exactly one.")

    policy = applications[0].get("spec", {}).get("syncPolicy") or {}
    automated = policy.get("automated")
    if not isinstance(automated, dict):
        refuse(f"{path} has no syncPolicy.automated — Argo CD would never apply the release this job is about to push.")
    return automated


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gitops-dir", required=True, type=Path)
    parser.add_argument("--region-root", required=True, type=Path)
    parser.add_argument("--application-name", required=True)
    args = parser.parse_args()

    region_root = args.region_root
    if not region_root.is_absolute():
        region_root = args.gitops_dir / region_root
    path = find_application(region_root, args.application_name)
    automated = automation_of(path)
    if automated.get("selfHeal") is not True:
        refuse(f"{path} has automated sync without selfHeal; a pushed release would not be reconciled onto a drifted cluster.")
    print(f"  {path}: automated sync with self-healing — release will be applied.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
