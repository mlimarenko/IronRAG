#!/usr/bin/env python3
"""Validate a release bundle and update Helm digests, connector images and the chart revision."""

from __future__ import annotations

import argparse
import json
import os
import re
import stat
import sys
import tempfile
from pathlib import Path
from typing import Any

MAX_CONTRACT_BYTES = 1024 * 1024
MAX_MANIFEST_BYTES = 1024 * 1024
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
GIT_COMMIT = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")
PIPELINE_ID = re.compile(r"[1-9]\d*", re.ASCII)
ARTIFACT_KEY = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]*")
REPOSITORY = re.compile(
    r"(?:(?:localhost|[a-z0-9.-]+\.[a-z0-9.-]+)(?::[0-9]+)?|"
    r"[a-z0-9.-]+:[0-9]+)/[a-z0-9._/-]+"
)
SEMVER = re.compile(
    r"(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)\."
    r"(0|[1-9][0-9]*)"
    r"(?:-((?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)

RELEASE_SCHEMA = "ci-components.release/v1"

TOP_LEVEL_FIELDS = {
    "schema",
    "release_id",
    "version",
    "commit",
    "pipeline_id",
    "images",
    "files",
}
IMAGE_FIELDS = {
    "artifact_key",
    "repository",
    "digest",
    "digest_ref",
    "platforms",
    "build_job",
}

TOP_KEY = re.compile(r"^[A-Za-z][A-Za-z0-9_-]*$")


class AdapterError(ValueError):
    """A release or GitOps contract violated the deployment boundary."""


def reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise AdapterError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def read_regular_text(path: Path, *, maximum_bytes: int, label: str) -> str:
    try:
        file_stat = path.lstat()
    except FileNotFoundError as error:
        raise AdapterError(f"{label} is missing") from error
    if stat.S_ISLNK(file_stat.st_mode) or not stat.S_ISREG(file_stat.st_mode):
        raise AdapterError(f"{label} must be a regular non-symlink file")
    if file_stat.st_size > maximum_bytes:
        raise AdapterError(f"{label} exceeds the size limit")
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise AdapterError(f"{label} is not UTF-8") from error


def parse_expected_images(values: list[str]) -> dict[str, str]:
    expected: dict[str, str] = {}
    seen_repositories: set[str] = set()
    for value in values:
        artifact_key, separator, repository = value.partition("=")
        if separator != "=" or ARTIFACT_KEY.fullmatch(artifact_key) is None:
            raise AdapterError("--image must use ARTIFACT_KEY=LITERAL_REPOSITORY")
        if REPOSITORY.fullmatch(repository) is None:
            raise AdapterError(f"invalid repository for image {artifact_key}")
        if artifact_key in expected:
            raise AdapterError(f"duplicate expected image key: {artifact_key}")
        if repository in seen_repositories:
            raise AdapterError(f"duplicate expected repository: {repository}")
        expected[artifact_key] = repository
        seen_repositories.add(repository)
    if not expected:
        raise AdapterError("at least one --image contract is required")
    return expected


def parse_expected_build_jobs(values: list[str]) -> dict[str, str]:
    expected: dict[str, str] = {}
    build_job_pattern = re.compile(r"[A-Za-z0-9][A-Za-z0-9:_.-]*:build")
    for value in values:
        artifact_key, separator, job_name = value.partition("=")
        if separator != "=" or ARTIFACT_KEY.fullmatch(artifact_key) is None:
            raise AdapterError("--build-job must use ARTIFACT_KEY=JOBNAME")
        if build_job_pattern.fullmatch(job_name) is None:
            raise AdapterError(f"invalid build job name for {artifact_key}")
        if artifact_key in expected:
            raise AdapterError(f"duplicate build-job artifact key: {artifact_key}")
        expected[artifact_key] = job_name
    return expected


def _load_release_document(
    release_path: Path,
    *,
    expected_commit: str,
    expected_pipeline_id: str,
) -> dict[str, Any]:
    raw = read_regular_text(
        release_path, maximum_bytes=MAX_CONTRACT_BYTES, label="release contract"
    )
    try:
        release = json.loads(raw, object_pairs_hook=reject_duplicate_json_keys)
    except (json.JSONDecodeError, AdapterError) as error:
        raise AdapterError("release contract is not canonical JSON") from error
    if not isinstance(release, dict) or set(release) != TOP_LEVEL_FIELDS:
        raise AdapterError("release contract has an unsupported top-level schema")
    if release["schema"] != RELEASE_SCHEMA:
        raise AdapterError("release contract schema is unsupported")
    if release["files"] != []:
        raise AdapterError("image deployment cannot consume file artifacts")
    if GIT_COMMIT.fullmatch(expected_commit) is None:
        raise AdapterError("expected commit is not a full lowercase Git object ID")
    if PIPELINE_ID.fullmatch(expected_pipeline_id) is None:
        raise AdapterError("expected pipeline ID is not canonical")
    if release["commit"] != expected_commit:
        raise AdapterError("release contract belongs to another commit")
    if str(release["pipeline_id"]) != expected_pipeline_id:
        raise AdapterError("release contract belongs to another pipeline")
    if (
        not isinstance(release["version"], str)
        or SEMVER.fullmatch(release["version"]) is None
    ):
        raise AdapterError("release version is not strict SemVer")
    expected_release_id = f"release-{expected_pipeline_id}-{expected_commit[:12]}"
    if release["release_id"] != expected_release_id:
        raise AdapterError("release ID is inconsistent with pipeline and commit")
    if not isinstance(release["images"], list):
        raise AdapterError("release images must be an array")
    return release


def _release_image_digest(
    image: Any,
    *,
    expected_platform: str,
    expected_images: dict[str, str],
    expected_build_jobs: dict[str, str],
    seen_artifact_keys: set[str],
) -> tuple[str, str]:
    if not isinstance(image, dict) or set(image) != IMAGE_FIELDS:
        raise AdapterError("release image has an unsupported schema")
    artifact_key = image["artifact_key"]
    if not isinstance(artifact_key, str) or artifact_key not in expected_images:
        raise AdapterError("release contains an unexpected image")
    if artifact_key in seen_artifact_keys:
        raise AdapterError(f"release contains duplicate image: {artifact_key}")
    seen_artifact_keys.add(artifact_key)
    repository = expected_images[artifact_key]
    if image["repository"] != repository:
        raise AdapterError(f"repository mismatch for image {artifact_key}")
    digest = image["digest"]
    if not isinstance(digest, str) or DIGEST.fullmatch(digest) is None:
        raise AdapterError(f"invalid digest for image {artifact_key}")
    if image["digest_ref"] != f"{repository}@{digest}":
        raise AdapterError(f"digest reference mismatch for image {artifact_key}")
    if image["platforms"] != expected_platform:
        raise AdapterError(f"platform mismatch for image {artifact_key}")
    if image["build_job"] != expected_build_jobs[artifact_key]:
        raise AdapterError(f"build identity mismatch for image {artifact_key}")
    return artifact_key, digest


def load_release(
    release_path: Path,
    *,
    expected_commit: str,
    expected_pipeline_id: str,
    expected_platform: str,
    expected_images: dict[str, str],
    expected_build_jobs: dict[str, str],
) -> dict[str, str]:
    release = _load_release_document(
        release_path,
        expected_commit=expected_commit,
        expected_pipeline_id=expected_pipeline_id,
    )

    digests: dict[str, str] = {}
    seen_artifact_keys: set[str] = set()
    for image in release["images"]:
        artifact_key, digest = _release_image_digest(
            image,
            expected_platform=expected_platform,
            expected_images=expected_images,
            expected_build_jobs=expected_build_jobs,
            seen_artifact_keys=seen_artifact_keys,
        )
        digests[artifact_key] = digest

    if set(digests) != set(expected_images):
        raise AdapterError("release image set is incomplete")
    return digests


# --- Kustomize `images:` block rewrite --------------------------------------
# Carried over unchanged from apply_release_bundle.py.


def _indented_block_end(
    lines: list[str],
    start: int,
    *,
    allowed_prefixes: str | tuple[str, ...],
) -> int:
    for index in range(start, len(lines)):
        line = lines[index].rstrip("\r\n")
        if line and not line.startswith(allowed_prefixes):
            return index
    return len(lines)


def _kustomization_image_block(lines: list[str]) -> tuple[int, int]:
    image_block_starts = [
        index
        for index, line in enumerate(lines)
        if re.fullmatch(r"images:\s*(?:#.*)?(?:\r?\n)?", line)
    ]
    if len(image_block_starts) != 1:
        raise AdapterError(
            "kustomization must contain exactly one top-level images block"
        )
    block_start = image_block_starts[0] + 1
    return block_start, _indented_block_end(
        lines,
        block_start,
        allowed_prefixes=(" ", "\t", "#"),
    )


def _kustomization_entries(
    lines: list[str],
    block_start: int,
    block_end: int,
) -> tuple[dict[str, list[int]], list[tuple[int, str]]]:
    entry_pattern = re.compile(
        r"^ {2}-\s+name:\s*[\"']?([^\"'\s#]+)[\"']?\s*(?:#.*)?(?:\r?\n)?$"
    )
    entries: dict[str, list[int]] = {}
    entry_starts: list[tuple[int, str]] = []
    for index in range(block_start, block_end):
        match = entry_pattern.fullmatch(lines[index])
        if match is None:
            continue
        repository = match.group(1)
        entries.setdefault(repository, []).append(index)
        entry_starts.append((index, repository))
    return entries, sorted(entry_starts)


def _require_expected_kustomization_entries(
    entries: dict[str, list[int]],
    expected_images: dict[str, str],
) -> None:
    for repository in expected_images.values():
        if len(entries.get(repository, [])) != 1:
            raise AdapterError(
                f"kustomization must contain image repository exactly once: {repository}"
            )


def _entry_ends(entry_starts: list[tuple[int, str]], block_end: int) -> dict[int, int]:
    entry_end_by_start: dict[int, int] = {}
    for position, (entry_start, _) in enumerate(entry_starts):
        next_start = (
            entry_starts[position + 1][0]
            if position + 1 < len(entry_starts)
            else block_end
        )
        entry_end_by_start[entry_start] = next_start
    return entry_end_by_start


def _rewrite_kustomization_entries(
    lines: list[str],
    *,
    entries: dict[str, list[int]],
    entry_end_by_start: dict[int, int],
    expected_images: dict[str, str],
    digests: dict[str, str],
) -> None:
    field_pattern = re.compile(
        r"^(?P<indent>\s+)(?P<field>newTag|digest):.*?(?P<ending>\r?\n)?$"
    )
    artifact_by_repository = {
        repository: artifact_key for artifact_key, repository in expected_images.items()
    }
    for repository, indexes in entries.items():
        if repository not in artifact_by_repository:
            continue
        entry_start = indexes[0]
        matching_fields: list[tuple[int, re.Match[str]]] = []
        for index in range(entry_start + 1, entry_end_by_start[entry_start]):
            match = field_pattern.fullmatch(lines[index])
            if match is not None:
                matching_fields.append((index, match))
        if len(matching_fields) != 1:
            raise AdapterError(
                f"kustomization image must contain exactly one newTag or digest: {repository}"
            )
        field_index, field_match = matching_fields[0]
        artifact_key = artifact_by_repository[repository]
        ending = field_match.group("ending") or ""
        lines[field_index] = (
            f'{field_match.group("indent")}digest: "{digests[artifact_key]}"{ending}'
        )


def render_kustomization(
    source: str, *, expected_images: dict[str, str], digests: dict[str, str]
) -> str:
    lines = source.splitlines(keepends=True)
    block_start, block_end = _kustomization_image_block(lines)
    entries, entry_starts = _kustomization_entries(lines, block_start, block_end)
    _require_expected_kustomization_entries(entries, expected_images)
    _rewrite_kustomization_entries(
        lines,
        entries=entries,
        entry_end_by_start=_entry_ends(entry_starts, block_end),
        expected_images=expected_images,
        digests=digests,
    )
    return "".join(lines)


# --- Helm values `image.digest` rewrite -------------------------------------
# New: the Kustomize entry format above does not apply here, because api/
# worker/web render from the pinned Helm chart and take their image purely
# from values — there is no in-tree manifest line to match by repository, so
# each target is instead named explicitly by its top-level values key.


def parse_values_digest_targets(values: list[str]) -> dict[str, str]:
    targets: dict[str, str] = {}
    for value in values:
        top_key, separator, artifact_key = value.partition("=")
        if (
            separator != "="
            or TOP_KEY.fullmatch(top_key) is None
            or ARTIFACT_KEY.fullmatch(artifact_key) is None
        ):
            raise AdapterError("--values-digest must use TOPKEY=ARTIFACT_KEY")
        if top_key in targets:
            raise AdapterError(f"duplicate values-digest top-level key: {top_key}")
        targets[top_key] = artifact_key
    return targets


def _values_top_blocks(lines: list[str], targets: dict[str, str]) -> dict[str, int]:
    top_key_pattern = re.compile(r"^([A-Za-z][A-Za-z0-9_-]*):\s*(?:#.*)?(?:\r?\n)?$")
    starts: dict[str, int] = {}
    for index, line in enumerate(lines):
        match = top_key_pattern.fullmatch(line)
        if match is None or match.group(1) not in targets:
            continue
        top_key = match.group(1)
        if top_key in starts:
            raise AdapterError(f"values file has duplicate top-level key: {top_key}")
        starts[top_key] = index
    missing = set(targets) - set(starts)
    if missing:
        raise AdapterError(
            f"values file is missing top-level key(s): {', '.join(sorted(missing))}"
        )
    return starts


def _rewrite_values_image(
    lines: list[str],
    *,
    top_key: str,
    block_start: int,
    artifact_key: str,
    expected_repository: str,
    digest: str,
) -> None:
    image_pattern = re.compile(r"^ {2}image:\s*(?:#.*)?(?:\r?\n)?$")
    repository_pattern = re.compile(r"^ {4}repository:\s*(\S+)\s*(?:\r?\n)?$")
    digest_pattern = re.compile(r"^(?P<indent> {4})digest:\s*(\S+)(?P<ending>\r?\n)?$")
    block_end = _indented_block_end(lines, block_start + 1, allowed_prefixes=" ")
    image_starts = [
        index
        for index in range(block_start + 1, block_end)
        if image_pattern.fullmatch(lines[index])
    ]
    if len(image_starts) != 1:
        raise AdapterError(
            f"values key {top_key!r} must contain exactly one 'image:' block"
        )
    image_start = image_starts[0] + 1
    image_end = _indented_block_end(
        lines[:block_end], image_start, allowed_prefixes="  "
    )
    repository_matches = [
        match
        for index in range(image_start, image_end)
        if (match := repository_pattern.fullmatch(lines[index])) is not None
    ]
    if (
        len(repository_matches) != 1
        or repository_matches[0].group(1) != expected_repository
    ):
        raise AdapterError(
            f"values key {top_key!r} image.repository does not match the reviewed input"
        )
    digest_indexes = [
        index
        for index in range(image_start, image_end)
        if digest_pattern.fullmatch(lines[index])
    ]
    if len(digest_indexes) != 1:
        raise AdapterError(
            f"values key {top_key!r} must contain exactly one image.digest field"
        )
    digest_index = digest_indexes[0]
    digest_match = digest_pattern.fullmatch(lines[digest_index])
    if digest_match is None:
        raise AssertionError(f"validated digest field disappeared for {artifact_key}")
    ending = digest_match.group("ending") or ""
    lines[digest_index] = f"    digest: {digest}{ending}"


def render_values(
    source: str,
    *,
    targets: dict[str, str],
    expected_images: dict[str, str],
    digests: dict[str, str],
) -> str:
    lines = source.splitlines(keepends=True)
    for top_key, block_start in _values_top_blocks(lines, targets).items():
        artifact_key = targets[top_key]
        _rewrite_values_image(
            lines,
            top_key=top_key,
            block_start=block_start,
            artifact_key=artifact_key,
            expected_repository=expected_images[artifact_key],
            digest=digests[artifact_key],
        )

    return "".join(lines)


# --- Application `sources[0].targetRevision` rewrite ------------------------
# New: the Helm chart source is pinned to an exact commit deliberately (see
# the header comment on services/ironrag.yaml) — a chart-template-only change
# must still bump this, or it never reaches the cluster even with every image
# digest current.


def _application_source_start(lines: list[str], repo_url: str) -> int:
    source_item_pattern = re.compile(r"^( {4}- )repoURL:\s*(\S+)\s*(?:\r?\n)?$")
    matching_start: int | None = None
    for index, line in enumerate(lines):
        match = source_item_pattern.fullmatch(line)
        if match is not None and match.group(2) == repo_url:
            if matching_start is not None:
                raise AdapterError(
                    f"application has duplicate source entries for {repo_url}"
                )
            matching_start = index
    if matching_start is None:
        raise AdapterError(f"application has no source entry for {repo_url}")
    return matching_start


def _application_source_end(lines: list[str], source_start: int) -> int:
    for index in range(source_start + 1, len(lines)):
        if re.match(r"^ {4}- \S", lines[index]) or re.match(r"^[A-Za-z]", lines[index]):
            return index
    return len(lines)


def render_application_revision(
    source: str, *, repo_url: str, new_revision: str
) -> str:
    if GIT_COMMIT.fullmatch(new_revision) is None:
        raise AdapterError("application revision is not a full lowercase Git object ID")
    lines = source.splitlines(keepends=True)
    revision_pattern = re.compile(
        r"^(?P<indent> {6})targetRevision:\s*(?P<value>\S+)(?P<ending>\r?\n)?$"
    )
    matching_source_start = _application_source_start(lines, repo_url)
    item_end = _application_source_end(lines, matching_source_start)

    revision_indexes = [
        index
        for index in range(matching_source_start, item_end)
        if revision_pattern.fullmatch(lines[index])
    ]
    if len(revision_indexes) != 1:
        raise AdapterError(
            f"source entry for {repo_url} must contain exactly one targetRevision field"
        )
    revision_index = revision_indexes[0]
    match = revision_pattern.fullmatch(lines[revision_index])
    ending = match.group("ending") or ""
    if match.group("value") == new_revision:
        return source
    lines[revision_index] = (
        f"{match.group('indent')}targetRevision: {new_revision}{ending}"
    )
    return "".join(lines)


def atomic_write(path: Path, content: str) -> None:
    original_mode = stat.S_IMODE(path.stat().st_mode)
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=path.parent,
            prefix=f".{path.name}.",
            delete=False,
        ) as temporary_file:
            temporary_name = temporary_file.name
            temporary_file.write(content)
            temporary_file.flush()
            os.fsync(temporary_file.fileno())
        os.chmod(temporary_name, original_mode)
        os.replace(temporary_name, path)
        temporary_name = None
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gitops-dir", required=True, type=Path)
    parser.add_argument("--release", required=True, type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--pipeline-id", required=True)
    parser.add_argument("--platform", default="linux/amd64")
    parser.add_argument(
        "--image", action="append", default=[], metavar="KEY=REPOSITORY"
    )
    parser.add_argument(
        "--build-job", action="append", default=[], metavar="KEY=JOBNAME"
    )
    parser.add_argument("--values", required=True, type=Path)
    parser.add_argument(
        "--values-digest", action="append", default=[], metavar="TOPKEY=KEY"
    )
    parser.add_argument("--kustomization", required=True, type=Path)
    parser.add_argument("--kustomize-key", action="append", default=[], metavar="KEY")
    parser.add_argument("--application", required=True, type=Path)
    parser.add_argument("--application-repo-url", required=True)
    parser.add_argument(
        "--application-revision",
        help="targetRevision to write; defaults to --commit, the release commit",
    )
    return parser.parse_args()


def resolve_release_routing(
    args: argparse.Namespace, expected_images: dict[str, str]
) -> tuple[dict[str, str], list[str]]:
    """Validate that every expected image is routed to values or kustomization."""
    values_targets = parse_values_digest_targets(args.values_digest)
    if not values_targets:
        raise AdapterError("at least one --values-digest target is required")
    for artifact_key in values_targets.values():
        if artifact_key not in expected_images:
            raise AdapterError(
                f"--values-digest references an unknown image key: {artifact_key}"
            )

    kustomize_keys = list(dict.fromkeys(args.kustomize_key))
    if not kustomize_keys:
        raise AdapterError("at least one --kustomize-key is required")
    for artifact_key in kustomize_keys:
        if artifact_key not in expected_images:
            raise AdapterError(
                f"--kustomize-key references an unknown image key: {artifact_key}"
            )

    unrouted = set(expected_images) - set(values_targets.values()) - set(kustomize_keys)
    if unrouted:
        raise AdapterError(
            f"image key(s) routed to neither values nor kustomization: {', '.join(sorted(unrouted))}"
        )
    return values_targets, kustomize_keys


def resolve_application_revision(args: argparse.Namespace) -> str:
    """Return the targetRevision to write into the Application manifest.

    The release commit is the revision the Application pins. A caller that
    passes it through a component input cannot expand `$CI_COMMIT_SHA` (input
    values are interpolated into single quotes and never re-expanded), so the
    commit is the default rather than a required duplicate argument.
    """
    return args.application_revision or args.commit


def run() -> None:
    args = parse_args()
    application_revision = resolve_application_revision(args)
    for name in ("values", "kustomization", "application"):
        path = getattr(args, name)
        if not path.is_absolute():
            setattr(args, name, args.gitops_dir / path)
    expected_images = parse_expected_images(args.image)
    expected_build_jobs = parse_expected_build_jobs(args.build_job)
    if set(expected_build_jobs) != set(expected_images):
        raise AdapterError("--build-job must name exactly the same keys as --image")

    digests = load_release(
        args.release,
        expected_commit=args.commit,
        expected_pipeline_id=args.pipeline_id,
        expected_platform=args.platform,
        expected_images=expected_images,
        expected_build_jobs=expected_build_jobs,
    )

    values_targets, kustomize_keys = resolve_release_routing(args, expected_images)

    values_source = read_regular_text(
        args.values, maximum_bytes=MAX_MANIFEST_BYTES, label="Helm values file"
    )
    values_rendered = render_values(
        values_source,
        targets=values_targets,
        expected_images=expected_images,
        digests=digests,
    )
    if values_rendered != values_source:
        atomic_write(args.values, values_rendered)

    kustomization_expected_images = {
        key: expected_images[key] for key in kustomize_keys
    }
    kustomization_source = read_regular_text(
        args.kustomization, maximum_bytes=MAX_MANIFEST_BYTES, label="kustomization"
    )
    kustomization_rendered = render_kustomization(
        kustomization_source,
        expected_images=kustomization_expected_images,
        digests=digests,
    )
    if kustomization_rendered != kustomization_source:
        atomic_write(args.kustomization, kustomization_rendered)

    application_source = read_regular_text(
        args.application, maximum_bytes=MAX_MANIFEST_BYTES, label="Application manifest"
    )
    application_rendered = render_application_revision(
        application_source,
        repo_url=args.application_repo_url,
        new_revision=application_revision,
    )
    if application_rendered != application_source:
        atomic_write(args.application, application_rendered)

    print(
        f"Applied {len(digests)} immutable image digests "
        f"({', '.join(sorted(values_targets))} via values, {', '.join(sorted(kustomize_keys))} via kustomization) "
        f"and set targetRevision={application_revision}."
    )


def main() -> int:
    try:
        run()
    except (AdapterError, OSError) as error:
        print(f"argocd release adapter: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
