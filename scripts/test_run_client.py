"""Launcher contract tests; no real client or compiler is started."""

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import run_client


class LauncherContract(unittest.TestCase):
    def setUp(self):
        self.root = Path("synthetic repository with spaces").absolute()
        self.environment = mock.patch.dict(os.environ, {}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_release_default_and_tui_development_are_explicit(self):
        release = run_client.launch_plan([], self.root)
        self.assertEqual(release["profile"], "release")
        self.assertIn("--release", release["build_command"])
        self.assertIn("--all-features", release["build_command"])
        self.assertEqual(release["build_command"].count("--bin"), 2)
        self.assertIn("peerlume-daemon", release["build_command"])
        self.assertEqual(Path(release["daemon_executable"]).parent, Path(release["client_command"][0]).parent)
        development = run_client.launch_plan(["--development", "--tui"], self.root)
        self.assertEqual(development["profile"], "debug")
        self.assertNotIn("--release", development["build_command"])
        self.assertEqual(development["build_command"][-3:], ["--no-default-features", "--features", "tui"])

    def test_application_arguments_are_forwarded_without_shell_parsing(self):
        arguments = ["--profile", "directory with spaces / quotes ' & $()", "--open", "Unicode ☃ & quoted.torrent", "", "--dev"]
        plan = run_client.launch_plan(["--no-build", "--", *arguments], self.root)
        self.assertEqual(plan["build_command"], [])
        self.assertEqual(plan["profile"], "release")
        self.assertEqual(plan["client_command"][1:], arguments)
        direct = run_client.launch_plan(["--open", "file with spaces.torrent"], self.root)
        self.assertEqual(direct["client_command"][1:], ["--open", "file with spaces.torrent"])

    def test_cargo_target_environment_and_print_only_do_not_launch(self):
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": "build outputs", "CARGO_BUILD_TARGET": "x86_64-example-target"}):
            plan = run_client.launch_plan([], self.root)
        self.assertEqual(Path(plan["client_command"][0]).parent, self.root / "build outputs" / "x86_64-example-target" / "release")
        output = io.StringIO()
        with mock.patch.object(subprocess, "run") as execute, contextlib.redirect_stdout(output):
            self.assertEqual(run_client.main(["--print-command", "--", "--help"], root=self.root), 0)
        execute.assert_not_called()
        self.assertEqual(json.loads(output.getvalue())["client_command"][1:], ["--help"])

    def test_build_failure_prevents_launch_and_client_exit_is_propagated(self):
        with mock.patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 7)) as execute:
            self.assertEqual(run_client.main([], root=self.root), 7)
        self.assertEqual(execute.call_count, 1)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            plan = run_client.launch_plan(["--no-build"], root)
            executable = Path(plan["client_command"][0])
            executable.parent.mkdir(parents=True)
            executable.touch()
            Path(plan["daemon_executable"]).touch()
            with mock.patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 9)) as execute:
                self.assertEqual(run_client.main(["--no-build", "--", "--profile", "safe profile"], root=root), 9)
            self.assertEqual(execute.call_args.args[0], [str(executable), "--profile", "safe profile"])
            self.assertEqual(execute.call_args.kwargs, {"cwd": root, "check": False})

    def test_incomplete_existing_package_is_reported_before_starting_a_frontend(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            plan = run_client.launch_plan(["--no-build", "--tui"], root)
            executable = Path(plan["client_command"][0])
            executable.parent.mkdir(parents=True)
            executable.touch()
            output = io.StringIO()
            with mock.patch.object(subprocess, "run") as execute, contextlib.redirect_stderr(output):
                self.assertEqual(run_client.main(["--no-build", "--tui"], root=root), 1)
            execute.assert_not_called()
            self.assertIn("peerlume-daemon", output.getvalue())


if __name__ == "__main__":
    unittest.main()
