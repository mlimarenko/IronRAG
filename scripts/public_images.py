#!/usr/bin/env python3
"""Check public registry provenance before immutable image publication."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import urllib.error
import urllib.request
from urllib.parse import urlsplit

PLATFORM_AMD64 = "linux/amd64"
PLATFORM_ARM64 = "linux/arm64"
IMAGES = {
    "ironrag-backend": {PLATFORM_AMD64},
    "ironrag-frontend": {PLATFORM_AMD64},
    "ironrag.bookstack": {PLATFORM_AMD64, PLATFORM_ARM64},
    "ironrag.confluence": {PLATFORM_AMD64, PLATFORM_ARM64},
    "ironrag.gitrepos": {PLATFORM_AMD64, PLATFORM_ARM64},
    "ironrag.youtrack": {PLATFORM_AMD64, PLATFORM_ARM64},
}
ANCHOR = "31f3f205e20fdf18ffb78b6b046fded327f9133c"
FROZEN_LATEST = {
    "ironrag-backend": "sha256:4a3fcaaaf3bbbad71ed31cb1e0b676958062dc06ea3771cabae3872e7d897bab",
    "ironrag-frontend": "sha256:c81862001d11ff6a57aa2ff76c9b66240c85b41687a7288fb8c0efe8b6c01076",
    "ironrag.bookstack": "sha256:d527ff424ea552437421a5b5328f6f2089d8e05817ccb61bb74b878d91d88499",
    "ironrag.confluence": "sha256:15796b338e73e21a4bf803843a7f9f14d8949d1a62b5ddc60d34b660f25f490e",
    "ironrag.gitrepos": "sha256:3bf161604359633a493cad5f0b05fad04afe96526946f90a5d86b2e33927b706",
    "ironrag.youtrack": "sha256:ddf52f174e8f71a3ffc6866a5e3ca463a63508c170211e530c80a9a809baafaf",
}
VERSION_LABEL = "org.opencontainers.image.version"
REVISION_LABEL = "org.opencontainers.image.revision"
SOURCE_LABEL = "org.opencontainers.image.source"
SOURCE_COMMIT_LABEL = "io.ironrag.source-commit"
PIPELINE_ID_LABEL = "io.ironrag.pipeline-id"
SOURCE_REPOSITORY = "https://github.com/mlimarenko/IronRAG"
RAW_RELEASE_BASE = "https://raw.githubusercontent.com/mlimarenko/IronRAG/"
DESCRIPTOR_FILE = ".public-release.json"
SEMVER = re.compile(r"v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", re.ASCII)
COMMIT_SHA = re.compile(r"[0-9a-f]{40}", re.ASCII)
PIPELINE_NUMBER = re.compile(r"[1-9]\d*", re.ASCII)


class Refused(ValueError):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        origin, target = urlsplit(req.full_url), urlsplit(newurl)
        # Docker serves config blobs from its CDN. Never forward registry auth
        # to that origin or permit arbitrary redirects from authenticated APIs.
        if (origin.hostname == "registry-1.docker.io" and "/blobs/sha256:" in origin.path
                and target.scheme == "https" and target.hostname == "production.cloudfront.docker.com"
                and target.port in {None, 443} and not target.username and not target.password):
            return urllib.request.Request(newurl, headers={"Accept": "application/json"})
        raise Refused("registry redirect refused")


def get(url, headers=None, missing=False):
    try:
        with urllib.request.build_opener(NoRedirect).open(
                urllib.request.Request(url, headers=headers or {}), timeout=30) as response:
            raw = response.read(8 * 1024 * 1024 + 1)
            if len(raw) > 8 * 1024 * 1024:
                raise Refused("registry document too large")
            return raw
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        if missing and status == 404:
            return None
        raise Refused(f"registry request failed (HTTP {status})") from None


class Registry:
    def __init__(self, image):
        if image not in IMAGES:
            raise Refused("image not approved")
        self.image = image
        self.base = "https://registry-1.docker.io/v2/pipingspace/" + image
        headers = {}
        username, password = os.getenv("DOCKERHUB_USERNAME"), os.getenv("DOCKERHUB_TOKEN")
        if username and password:
            headers["Authorization"] = "Basic " + base64.b64encode(
                (username + ":" + password).encode()).decode()
        token = json.loads(get("https://auth.docker.io/token?service=registry.docker.io&scope=repository:pipingspace/" + image + ":pull", headers))["token"]
        self.headers = {"Authorization": "Bearer " + token, "Accept": ", ".join([
            "application/vnd.oci.image.index.v1+json", "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.list.v2+json",
            "application/vnd.docker.distribution.manifest.v2+json"])}

    def document(self, kind, reference, missing=False):
        if not re.fullmatch(r"[a-zA-Z0-9_.:-]+", reference):
            raise Refused("invalid registry reference")
        raw = get(self.base + "/" + kind + "/" + reference, self.headers, missing)
        if raw is None:
            return None
        digest = "sha256:" + hashlib.sha256(raw).hexdigest()
        if reference.startswith("sha256:") and digest != reference:
            raise Refused("registry digest mismatch")
        return json.loads(raw), digest

    def image_labels(self, tag):
        result = self.document("manifests", tag, missing=True)
        if result is None:
            return None
        root, digest = result
        if tag == "latest":
            self.latest_digest = digest
        manifests = root.get("manifests")
        labels = {}
        if manifests is None:
            manifests = [{"digest": digest}]
        for item in manifests:
            platform = item.get("platform", {})
            if platform.get("os") == "unknown":
                continue  # BuildKit attestations are not runnable platforms.
            manifest, _ = self.document("manifests", item["digest"])
            config, _ = self.document("blobs", manifest["config"]["digest"])
            name = config["os"] + "/" + config["architecture"]
            if name in labels:
                raise Refused("duplicate runnable platform")
            labels[name] = config.get("config", {}).get("Labels", {})
        if set(labels) != IMAGES[self.image]:
            raise Refused("image platforms differ from publication contract")
        return labels


def version_tuple(value):
    match = SEMVER.fullmatch(value)
    if not match:
        raise Refused("invalid stable image version")
    return tuple((len(p), p) for p in match.groups())


def verify(labels, descriptor, public_commit):
    expected = {
        VERSION_LABEL: "v" + descriptor["version"],
        REVISION_LABEL: public_commit,
        SOURCE_LABEL: SOURCE_REPOSITORY,
        SOURCE_COMMIT_LABEL: descriptor["source_commit"],
        PIPELINE_ID_LABEL: descriptor["pipeline_id"],
    }
    for actual in labels.values():
        if any(actual.get(k) != v for k, v in expected.items()):
            raise Refused("existing image has foreign provenance")


def needs_build(registry, descriptor, commit):
    """Refuse to reuse an immutable version tag unless its provenance matches."""
    labels = registry.image_labels("v" + descriptor["version"])
    if labels is None:
        return True
    try:
        verify(labels, descriptor, commit)
    except Refused:
        return True
    return False


def is_frozen_snapshot(registry, labels, actual):
    """Recognize the frozen pre-descriptor latest digest instead of publishing it."""
    if labels.get(REVISION_LABEL) != ANCHOR or actual != "v0.6.0":
        return False
    if registry.latest_digest != FROZEN_LATEST[registry.image]:
        raise Refused("frozen latest digest changed")
    return True


def require_active_provenance(labels):
    if (
        not COMMIT_SHA.fullmatch(labels.get(SOURCE_COMMIT_LABEL, ""))
        or not PIPELINE_NUMBER.fullmatch(labels.get(PIPELINE_ID_LABEL, ""))
        or labels.get(SOURCE_LABEL) != SOURCE_REPOSITORY
    ):
        raise Refused("latest has foreign provenance")


def tolerate_foreign_latest(latest, descriptor, commit):
    """A same-version latest tag is replaced, never repaired in place."""
    try:
        verify(latest, descriptor, commit)
    except Refused:
        pass


def public_release_descriptor(revision):
    return json.loads(get(RAW_RELEASE_BASE + revision + "/" + DESCRIPTOR_FILE))


def prove_previous_release(latest, labels, actual, proven):
    """Bind an older latest tag to the public descriptor of its own revision."""
    revision = labels.get(REVISION_LABEL, "")
    if not COMMIT_SHA.fullmatch(revision):
        raise Refused("latest public revision is invalid")
    proof = (revision, actual)
    if proof in proven:
        return
    frozen = public_release_descriptor(revision)
    if frozen != public_release_descriptor(actual):
        raise Refused("latest public tag source differs")
    verify(latest, frozen, revision)
    proven.add(proof)


def latest_allows_update(registry, descriptor, commit, latest):
    """Decide whether this release may advance latest over every published tag."""
    proven = set()
    update_latest = True
    for labels in latest.values():
        actual = labels.get(VERSION_LABEL, "")
        current = version_tuple(actual)
        selected = version_tuple(descriptor["version"])
        if is_frozen_snapshot(registry, labels, actual):
            continue  # Frozen pre-descriptor snapshot, not an active publisher.
        require_active_provenance(labels)
        if current == selected:
            tolerate_foreign_latest(latest, descriptor, commit)
        else:
            prove_previous_release(latest, labels, actual, proven)
        if current > selected:
            update_latest = False
    return update_latest


def prepare(registry, descriptor, commit):
    build = needs_build(registry, descriptor, commit)
    latest = registry.image_labels("latest")
    if latest is None:
        return build, True
    return build, latest_allows_update(registry, descriptor, commit, latest)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["all", "prepare", "verify"])
    parser.add_argument("--image", choices=IMAGES)
    args = parser.parse_args()
    try:
        descriptor = json.loads(Path(DESCRIPTOR_FILE).read_text())
        commit = os.environ["GITHUB_SHA"]
        if (descriptor.get("schema") != "ironrag.public-release/v1"
                or os.environ["GITHUB_REF_NAME"] != "v" + descriptor["version"]
                or not COMMIT_SHA.fullmatch(descriptor["source_commit"])
                or not PIPELINE_NUMBER.fullmatch(descriptor["pipeline_id"])
                or not PIPELINE_NUMBER.fullmatch(descriptor["pipeline_iid"])):
            raise Refused("release descriptor mismatch")
        version_tuple(descriptor["version"])
        for image in IMAGES if args.mode == "all" else [args.image]:
            registry = Registry(image)
            if args.mode == "verify":
                labels = registry.image_labels("v" + descriptor["version"])
                if labels is None:
                    raise Refused("published image missing")
                verify(labels, descriptor, commit)
            else:
                build, latest = prepare(registry, descriptor, commit)
                if args.mode == "prepare":
                    with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
                        output.write(f"build={str(build).lower()}\nlatest={str(latest).lower()}\n")
        print("Public image provenance and platforms confirmed")
    except (ValueError, OSError, KeyError, TypeError):
        parser.exit(1, "Public image provenance refused or unconfirmed\n")


if __name__ == "__main__":
    main()
