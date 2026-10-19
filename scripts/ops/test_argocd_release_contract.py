import json
import runpy
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

ADAPTER = runpy.run_path(str(Path(__file__).resolve().parents[1] / "ci/apply_argocd_release.py"))


class ReleaseContractTests(unittest.TestCase):
    def test_application_revision_defaults_to_the_release_commit(self):
        # A component input cannot expand $CI_COMMIT_SHA: the value is
        # interpolated into single quotes and never re-expanded, so the
        # revision defaults to the commit instead of a required duplicate
        # argument (OPS-632).
        args = SimpleNamespace(application_revision=None, commit="c" * 40)
        self.assertEqual(ADAPTER["resolve_application_revision"](args), "c" * 40)

    def test_application_revision_override_wins_over_the_commit(self):
        args = SimpleNamespace(application_revision="d" * 40, commit="c" * 40)
        self.assertEqual(ADAPTER["resolve_application_revision"](args), "d" * 40)
    def test_image_deployment_accepts_only_an_empty_files_collection(self):
        commit = "a" * 40
        repository = "registry.example.test/team/backend"
        digest = "sha256:" + "b" * 64
        release = {
            "schema": "ci-components.release/v1",
            "release_id": "release-123-" + commit[:12],
            "version": "1.2.3",
            "commit": commit,
            "pipeline_id": "123",
            "files": [],
            "images": [
                {
                    "artifact_key": "backend",
                    "repository": repository,
                    "digest": digest,
                    "digest_ref": repository + "@" + digest,
                    "platforms": "linux/amd64",
                    "build_job": "image:backend:build",
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.json"
            arguments = dict(
                expected_commit=commit,
                expected_pipeline_id="123",
                expected_platform="linux/amd64",
                expected_images={"backend": repository},
                expected_build_jobs={"backend": "image:backend:build"},
            )
            path.write_text(json.dumps(release))
            self.assertEqual(ADAPTER["load_release"](path, **arguments), {"backend": digest})
            for files in (None, {}, [{"artifact_key": "package"}]):
                with self.subTest(files=files):
                    release["files"] = files
                    path.write_text(json.dumps(release))
                    with self.assertRaises(ADAPTER["AdapterError"]):
                        ADAPTER["load_release"](path, **arguments)

    def _write_release(self, directory, release):
        path = Path(directory) / "release.json"
        path.write_text(json.dumps(release))
        return path

    def _release(self, *, build_job):
        commit = "a" * 40
        repository = "registry.example.test/team/backend"
        digest = "sha256:" + "b" * 64
        return {
            "schema": "ci-components.release/v1",
            "release_id": "release-123-" + commit[:12],
            "version": "1.2.3",
            "commit": commit,
            "pipeline_id": "123",
            "files": [],
            "images": [
                {
                    "artifact_key": "backend",
                    "repository": repository,
                    "digest": digest,
                    "digest_ref": repository + "@" + digest,
                    "platforms": "linux/amd64",
                    "build_job": build_job,
                }
            ],
        }

    def test_release_accepts_the_real_build_producer(self):
        # The bundle records the real build job; the adapter must accept it and
        # the old proxy gate identity must never validate (OPS-850).
        arguments = {
            "expected_commit": "a" * 40,
            "expected_pipeline_id": "123",
            "expected_platform": "linux/amd64",
            "expected_images": {"backend": "registry.example.test/team/backend"},
            "expected_build_jobs": {"backend": "image:backend:build"},
        }
        with tempfile.TemporaryDirectory() as directory:
            path = self._write_release(
                directory, self._release(build_job="image:backend:build")
            )
            self.assertEqual(
                ADAPTER["load_release"](path, **arguments),
                {"backend": "sha256:" + "b" * 64},
            )

    def test_release_rejects_a_gate_producer_and_a_renamed_field(self):
        arguments = {
            "expected_commit": "a" * 40,
            "expected_pipeline_id": "123",
            "expected_platform": "linux/amd64",
            "expected_images": {"backend": "registry.example.test/team/backend"},
            "expected_build_jobs": {"backend": "image:backend:build"},
        }
        for build_job in ("image:backend:gate", "", "image:backend:build:extra"):
            with (
                self.subTest(build_job=build_job),
                tempfile.TemporaryDirectory() as directory,
            ):
                path = self._write_release(
                    directory, self._release(build_job=build_job)
                )
                with self.assertRaises(ADAPTER["AdapterError"]) as raised:
                    ADAPTER["load_release"](path, **arguments)
                self.assertIn("build identity mismatch", str(raised.exception))
        with tempfile.TemporaryDirectory() as directory:
            release = self._release(build_job="image:backend:build")
            release["images"][0]["gate_job"] = release["images"][0].pop("build_job")
            path = self._write_release(directory, release)
            with self.assertRaises(ADAPTER["AdapterError"]) as raised:
                ADAPTER["load_release"](path, **arguments)
            self.assertIn("unsupported schema", str(raised.exception))

    def test_expected_build_jobs_reject_a_gate_suffix(self):
        self.assertEqual(
            ADAPTER["parse_expected_build_jobs"](["backend=image:backend:build"]),
            {"backend": "image:backend:build"},
        )
        with self.assertRaises(ADAPTER["AdapterError"]):
            ADAPTER["parse_expected_build_jobs"](["backend=image:backend:gate"])
