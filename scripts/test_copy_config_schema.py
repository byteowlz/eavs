#!/usr/bin/env python3
"""Offline regression tests; never touch the real neighbouring schemas repo."""
import pathlib
import shutil
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("copy_config_schema.sh")


class SchemaCopyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = pathlib.Path(self.tmp.name)
        self.repo = root / "eavs"
        self.schemas = root / "schemas"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "config").mkdir()
        (self.repo / "config/config.schema.json").write_text('{"type":"object"}\n')
        shutil.copyfile(SCRIPT, self.repo / "scripts/copy_config_schema.sh")
        self.schemas.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("commit", "--allow-empty", "-qm", "initial")

    def git(self, *args):
        return subprocess.check_output(
            ["git", "-C", str(self.schemas), *args], text=True
        ).strip()

    def run_copy(self):
        return subprocess.run(
            ["bash", str(self.repo / "scripts/copy_config_schema.sh")],
            capture_output=True, text=True, check=False,
        )

    def test_clean_copy_does_not_commit(self):
        head = self.git("rev-parse", "HEAD")
        result = self.run_copy()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual(self.git("diff", "--cached", "--name-only"), "")
        self.assertEqual(
            (self.schemas / "eavs/eavs.config.schema.json").read_bytes(),
            (self.repo / "config/config.schema.json").read_bytes(),
        )

    def test_dirty_repository_is_preserved(self):
        (self.schemas / "unrelated").write_text("keep")
        self.assertNotEqual(self.run_copy().returncode, 0)
        self.assertEqual((self.schemas / "unrelated").read_text(), "keep")
        self.assertFalse((self.schemas / "eavs").exists())

    def test_symlink_destination_is_refused(self):
        target = pathlib.Path(self.tmp.name) / "outside"
        target.mkdir()
        (self.schemas / "eavs").symlink_to(target, target_is_directory=True)
        self.assertNotEqual(self.run_copy().returncode, 0)
        self.assertEqual(list(target.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
