from __future__ import annotations

import importlib.util
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest


MODULE_PATH = Path(__file__).with_name("scan_worktree_secrets.py")
SPEC = importlib.util.spec_from_file_location("scan_worktree_secrets", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load scan_worktree_secrets module")
SCAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SCAN)


class WorktreeSecretScanTests(unittest.TestCase):
    def test_selected_paths_include_current_sources_and_skip_deleted_or_ignored_files(self) -> None:
        with TemporaryDirectory(prefix="ironrag-secret-scan-test-") as temporary_directory:
            root = Path(temporary_directory)
            subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
            (root / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
            (root / "tracked.txt").write_text("tracked\n", encoding="utf-8")
            (root / "deleted.txt").write_text("deleted\n", encoding="utf-8")
            subprocess.run(
                ["git", "add", ".gitignore", "tracked.txt", "deleted.txt"],
                cwd=root,
                check=True,
            )
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
