from __future__ import annotations

import importlib.util
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest


def _load(module_name: str):
    path = Path(__file__).with_name(f"{module_name}.py")
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {module_name} module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SCAN = _load("scan_worktree_secrets")
GIT_SANDBOX = _load("git_sandbox")


class WorktreeSecretScanTests(unittest.TestCase):
    def test_selected_paths_include_current_sources_and_skip_deleted_or_ignored_files(self) -> None:
        with TemporaryDirectory(prefix="ironrag-secret-scan-test-") as temporary_directory:
            root = Path(temporary_directory)
            GIT_SANDBOX.init_repository(root)
            (root / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
            (root / "tracked.txt").write_text("tracked\n", encoding="utf-8")
            (root / "deleted.txt").write_text("deleted\n", encoding="utf-8")
            GIT_SANDBOX.run_git(["add", ".gitignore", "tracked.txt", "deleted.txt"], cwd=root)
            (root / "deleted.txt").unlink()
            (root / "untracked.txt").write_text("untracked\n", encoding="utf-8")
            (root / "ignored.txt").write_text("ignored\n", encoding="utf-8")

            selected = {path.as_posix() for path in SCAN.selected_paths(root)}

            self.assertEqual(selected, {".gitignore", "tracked.txt", "untracked.txt"})

    def test_copy_selected_tree_preserves_paths_and_scans_symlink_payloads_as_data(self) -> None:
        with TemporaryDirectory(prefix="ironrag-secret-source-") as source_directory:
            with TemporaryDirectory(prefix="ironrag-secret-target-") as target_directory:
                source = Path(source_directory)
                target = Path(target_directory)
                nested = source / "nested"
                nested.mkdir()
                (nested / "source.txt").write_text("fixture\n", encoding="utf-8")
                (nested / "link.txt").symlink_to("source.txt")

                SCAN.copy_selected_tree(
                    source,
                    target,
                    (Path("nested/source.txt"), Path("nested/link.txt")),
                )

                self.assertEqual(
                    (target / "nested/source.txt").read_text(encoding="utf-8"),
                    "fixture\n",
                )
                self.assertFalse((target / "nested/link.txt").is_symlink())
                self.assertEqual(
                    (target / "nested/link.txt").read_text(encoding="utf-8"),
                    "source.txt",
                )


if __name__ == "__main__":
    unittest.main()
