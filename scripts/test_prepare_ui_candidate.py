"""Disposable candidate packaging and profile admission contracts."""
import json
from pathlib import Path
import tempfile
import unittest

from prepare_ui_candidate import digest, prepare


class CandidateContracts(unittest.TestCase):
    def test_aliases_copy_to_distinct_guid_packages_without_touching_existing_client(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gui, daemon = root / "ui-candidate.exe", root / "ui-candidate-daemon.exe"
            gui.write_bytes(b"MZ\x00\x00synthetic-gui-contract")
            daemon.write_bytes(b"MZ\x00\x00synthetic-daemon-contract")
            existing = root / "peerlume.exe"
            existing.write_bytes(b"preserve-live-binary")
            first, second = prepare(gui, daemon, root), prepare(gui, daemon, root)
            self.assertNotEqual(first, second)
            manifest = json.loads((first / "candidate.json").read_text())
            self.assertFalse(manifest["process_started"])
            self.assertEqual(digest(gui), manifest["artifacts"]["peerlume.exe"]["sha256"])
            self.assertEqual(existing.read_bytes(), b"preserve-live-binary")
            self.assertFalse(Path(manifest["profile"]).exists())
            self.assertEqual(manifest["launch_argv"][1:], ["--profile", manifest["profile"]])
            self.assertEqual(manifest["stop_disposable_owner_argv"][-1], "--stop")
            self.assertEqual(manifest["prepare_fixture_env"]["PEERLUME_UI_CANDIDATE_PROFILE"], manifest["profile"])

    def test_normal_user_binary_names_or_invalid_executables_are_rejected_before_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gui, daemon = root / "ui-candidate.exe", root / "ui-candidate-daemon.exe"
            gui.write_bytes(b"invalid")
            daemon.write_bytes(b"MZ\x00\x00fixture")
            with self.assertRaises(ValueError):
                prepare(root / "peerlume.exe", daemon, root)
            with self.assertRaises(ValueError):
                prepare(gui, daemon, root)
            self.assertEqual(list(root.glob("ui-candidate-*/candidate.json")), [])

    def test_linux_aliases_keep_canonical_executable_names(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gui, daemon = root / "ui-candidate", root / "ui-candidate-daemon"
            gui.write_bytes(b"\x7fELFsynthetic-gui")
            daemon.write_bytes(b"\x7fELFsynthetic-owner")
            directory = prepare(gui, daemon, root)
            self.assertTrue((directory / "peerlume").is_file())
            self.assertTrue((directory / "peerlume-daemon").is_file())


if __name__ == "__main__":
    unittest.main()
