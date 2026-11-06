"""Local coverage capture admission and Cargo artifact contracts."""
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from measure_gui_coverage import isolated_environment, main, sha256, test_executable as cargo_test_executable, tool_paths


def artifact(executable="C:/repo/target/debug/deps/peerlume-fixture.exe"):
    return {"reason": "compiler-artifact", "target": {"name": "peerlume", "kind": ["lib"]},
            "profile": {"test": True}, "executable": executable}


class LocalCaptureContracts(unittest.TestCase):
    def test_test_environment_cannot_inherit_normal_configuration_or_data_paths(self):
        base = {"LOCALAPPDATA": "private/user/profile", "APPDATA": "private/roaming", "XDG_CONFIG_HOME": "private/config",
                "XDG_DATA_HOME": "private/data", "XDG_CACHE_HOME": "private/cache", "PATH": "preserve-tools"}
        evidence = Path("fresh-capture")
        isolated = isolated_environment(base, evidence)
        for variable in ("LOCALAPPDATA", "APPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"):
            self.assertTrue(Path(isolated[variable]).is_relative_to(evidence / "test-environment"))
            self.assertNotEqual(isolated[variable], base[variable])
        self.assertEqual(isolated["PATH"], base["PATH"])
        self.assertEqual(base["LOCALAPPDATA"], "private/user/profile")

    def test_only_exact_library_test_executable_is_admitted(self):
        wanted = artifact()
        dependency = artifact("dependency.exe")
        dependency["target"]["name"] = "dependency"
        binary = artifact("application.exe")
        binary["target"]["kind"] = ["bin"]
        ordinary_lib = artifact("not-test.exe")
        ordinary_lib["profile"]["test"] = False
        messages = "\n".join(json.dumps(item) for item in [dependency, binary, ordinary_lib, wanted, wanted])
        self.assertEqual(cargo_test_executable(messages), Path(wanted["executable"]))
        for invalid in ("", json.dumps(dependency), json.dumps(wanted) + "\n" + json.dumps(artifact("other.exe"))):
            with self.assertRaises(ValueError):
                cargo_test_executable(invalid)

    def test_existing_evidence_is_never_modified_or_used_for_a_new_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report = directory / "capture.json"
            report.write_text("preserve", encoding="utf-8")
            with patch("measure_gui_coverage.subprocess.check_output") as subprocess_call, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(["--output", str(directory)]), 1)
                subprocess_call.assert_not_called()
            self.assertEqual(report.read_text(), "preserve")

    def test_failed_disk_admission_never_starts_rustc_or_creates_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            evidence = Path(temporary) / "new"
            with patch("measure_gui_coverage.check_space", side_effect=ValueError("disk guard")), patch("measure_gui_coverage.subprocess.check_output") as subprocess_call, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(["--output", str(evidence)]), 1)
                subprocess_call.assert_not_called()
            self.assertFalse(evidence.exists())

    def test_wrong_toolchain_does_not_create_evidence_or_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            evidence = Path(temporary) / "new"
            with patch("measure_gui_coverage.check_space", return_value={}), patch("measure_gui_coverage.subprocess.check_output", return_value="release: 1.94.0\n"), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(["--output", str(evidence)]), 1)
            self.assertFalse(evidence.exists())

    def test_wrapper_override_and_invalid_arguments_are_not_silently_accepted(self):
        with tempfile.TemporaryDirectory() as temporary:
            evidence = Path(temporary) / "new"
            for args in (["--test-threads", "0"], ["--timeout-seconds", "0"]):
                with contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(main(["--output", str(evidence), *args]), 1)
            with patch.dict("measure_gui_coverage.os.environ", {"RUSTC_WORKSPACE_WRAPPER": "existing-wrapper"}), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(["--output", str(evidence)]), 1)
            self.assertFalse(evidence.exists())

    def test_hashes_are_file_content_based_and_tools_must_match_the_sysroot(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            data = directory / "source.rs"
            data.write_bytes(b"abc")
            self.assertEqual(sha256(data), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            with self.assertRaisesRegex(ValueError, "llvm-tools-preview"):
                tool_paths(directory, "missing-host")


if __name__ == "__main__":
    unittest.main()
