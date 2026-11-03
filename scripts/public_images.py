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

IMAGES = {
    "ironrag-backend": {"linux/amd64"},
    "ironrag-frontend": {"linux/amd64"},
    "ironrag.bookstack": {"linux/amd64", "linux/arm64"},
    "ironrag.confluence": {"linux/amd64", "linux/arm64"},
    "ironrag.gitrepos": {"linux/amd64", "linux/arm64"},
    "ironrag.youtrack": {"linux/amd64", "linux/arm64"},
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
SEMVER = re.compile(r"v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


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
    for actual in labels.values():
        expected = {
            "org.opencontainers.image.version": "v" + descriptor["version"],
            "org.opencontainers.image.revision": public_commit,
            "org.opencontainers.image.source": "https://github.com/mlimarenko/IronRAG",
            "io.ironrag.source-commit": descriptor["source_commit"],
            "io.ironrag.pipeline-id": descriptor["pipeline_id"],
        }
        if any(actual.get(k) != v for k, v in expected.items()):
            raise Refused("existing image has foreign provenance")


def prepare(registry, descriptor, commit):
    version = registry.image_labels("v" + descriptor["version"])
    needs_build = True
    if version is not None:
        try:
            verify(version, descriptor, commit)
            needs_build = False
        except Refused:
            needs_build = True
    latest = registry.image_labels("latest")
    update_latest = True
    proven = set()
    if latest is not None:
        for labels in latest.values():
            actual = labels.get("org.opencontainers.image.version", "")
            current = version_tuple(actual)
            selected = version_tuple(descriptor["version"])
            if labels.get("org.opencontainers.image.revision") == ANCHOR and actual == "v0.6.0":
                if registry.latest_digest != FROZEN_LATEST[registry.image]:
                    raise Refused("frozen latest digest changed")
                continue  # Frozen pre-descriptor snapshot, not an active publisher.
            if (not re.fullmatch(r"[0-9a-f]{40}", labels.get("io.ironrag.source-commit", ""))
                    or not re.fullmatch(r"[1-9][0-9]*", labels.get("io.ironrag.pipeline-id", ""))
                    or labels.get("org.opencontainers.image.source") != "https://github.com/mlimarenko/IronRAG"):
                raise Refused("latest has foreign provenance")
            if current == selected:
                try:
                    verify(latest, descriptor, commit)
                except Refused:
                    pass
            else:
                revision = labels.get("org.opencontainers.image.revision", "")
                if not re.fullmatch(r"[0-9a-f]{40}", revision):
                    raise Refused("latest public revision is invalid")
                proof = (revision, actual)
                if proof not in proven:
                    base = "https://raw.githubusercontent.com/mlimarenko/IronRAG/"
                    frozen = json.loads(get(base + revision + "/.public-release.json"))
                    tagged = json.loads(get(base + actual + "/.public-release.json"))
                    if frozen != tagged:
                        raise Refused("latest public tag source differs")
                    verify(latest, frozen, revision)
                    proven.add(proof)
            if current > selected:
                update_latest = False
    return needs_build, update_latest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["all", "prepare", "verify"])
    parser.add_argument("--image", choices=IMAGES)
    args = parser.parse_args()
    try:
        descriptor = json.loads(Path(".public-release.json").read_text())
        commit = os.environ["GITHUB_SHA"]
        if (descriptor.get("schema") != "ironrag.public-release/v1"
                or os.environ["GITHUB_REF_NAME"] != "v" + descriptor["version"]
                or not re.fullmatch(r"[0-9a-f]{40}", descriptor["source_commit"])
                or not re.fullmatch(r"[1-9][0-9]*", descriptor["pipeline_id"])
                or not re.fullmatch(r"[1-9][0-9]*", descriptor["pipeline_iid"])):
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
