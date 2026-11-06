"""Copied X11 package and owner lifecycle contracts; no executable is launched."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("native_ui_e2e", Path(__file__).with_name("native-ui-e2e.py"))
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class NativePackageTests(unittest.TestCase):
    def package(self, root):
        sources = root / "source files"
        sources.mkdir()
        for name in ("peerlume", "peerlume-daemon", "peerlume-control"):
            (sources / name).write_bytes(("synthetic-" + name).encode())
        args = SimpleNamespace(gui=sources / "peerlume", daemon=sources / "peerlume-daemon",
                               cli=sources / "peerlume-control")
        temporary = root / "isolated profile space"
        temporary.mkdir()
        report = root / "report"
        report.mkdir()
        return fixture.NativePackage(args, temporary, report, mock.Mock())

    def test_complete_package_copies_daemon_beside_frontend_without_moving_sources(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = self.package(root)
            self.assertEqual(package.gui.parent, package.daemon.parent)
            self.assertEqual(package.cli.parent, package.daemon.parent)
            for path in (package.gui, package.daemon, package.cli):
                self.assertEqual(path.read_bytes(), (root / "source files" / path.name).read_bytes())
            self.assertEqual(package.stops, 0)
            self.assertFalse(package.owner_may_be_running)

    def test_missing_or_invalid_daemon_rejects_before_partial_package_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gui, cli, daemon = root / "gui", root / "cli", root / "daemon"
            gui.write_bytes(b"gui")
            cli.write_bytes(b"cli")
            destination = root / "installed"
            with self.assertRaises(FileNotFoundError):
                fixture.install_package(gui, daemon, cli, destination)
            self.assertFalse(destination.exists())
            daemon.mkdir()
            with self.assertRaises(ValueError):
                fixture.install_package(gui, daemon, cli, destination)
            self.assertFalse(destination.exists())

    def test_launch_uses_literal_copied_paths_and_explicit_isolated_profile(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            source = package.root / "file with spaces; literal.torrent"
            with mock.patch.object(fixture.subprocess, "Popen") as launch:
                process = package.launch(source, mock.sentinel.log)
            self.assertIs(process, launch.return_value)
            self.assertEqual(launch.call_args.args[0], [str(package.gui), "--profile", str(package.profile),
                                                      "--open", str(source)])
            self.assertNotIn("shell", launch.call_args.kwargs)
            self.assertTrue(package.owner_may_be_running)

    def test_stop_waits_for_only_copied_owner_without_start_or_kill_arguments(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            package.owner_may_be_running = True
            with mock.patch.object(fixture.subprocess, "run", return_value=SimpleNamespace(
                    stdout=b"profile lock released\n", stderr=b"")) as stop:
                package.stop_owner()
            self.assertEqual(stop.call_args.args[0], [str(package.daemon), "--profile", str(package.profile), "--stop"])
            self.assertEqual(stop.call_args.kwargs["timeout"], 100)
            self.assertTrue(stop.call_args.kwargs["check"])
            self.assertFalse(package.owner_may_be_running)
            self.assertEqual(package.stops, 1)
            self.assertEqual((package.report / "owner-stop-1.log").read_bytes(), b"profile lock released\n")

    def test_failed_stop_never_counts_as_lock_release_or_clean_owner(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            package.owner_may_be_running = True
            with mock.patch.object(fixture.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["fixture"])):
                with self.assertRaises(subprocess.CalledProcessError):
                    package.stop_owner()
            self.assertTrue(package.owner_may_be_running)
            self.assertEqual(package.stops, 0)

    def test_successful_frontend_exit_still_requires_explicit_owner_stop(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            package.app = mock.Mock()
            package.app.poll.return_value = 0
            package.owner_may_be_running = True
            with mock.patch.object(package, "stop_owner") as stop:
                self.assertFalse(package.__exit__(None, None, None))
            stop.assert_called_once_with()
            package.app.terminate.assert_not_called()

    def test_failure_cleanup_stops_owner_even_if_screenshot_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            package.app = mock.Mock()
            package.app.poll.return_value = None
            package.desktop.screenshot.side_effect = subprocess.TimeoutExpired("screenshot", 10)
            package.owner_may_be_running = True
            with mock.patch.object(package, "stop_owner") as stop:
                self.assertFalse(package.__exit__(AssertionError, AssertionError("original"), None))
            package.app.terminate.assert_called_once_with()
            package.app.wait.assert_called_once_with(timeout=10)
            stop.assert_called_once_with()

    def test_frontend_termination_is_bounded_and_owner_stop_follows(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            package.app = mock.Mock()
            package.app.poll.return_value = None
            package.app.wait.side_effect = [subprocess.TimeoutExpired("fixture GUI", 10), 0]
            package.owner_may_be_running = True
            with mock.patch.object(package, "stop_owner") as stop:
                package.__exit__(AssertionError, AssertionError("original"), None)
            package.app.kill.assert_called_once_with()
            self.assertEqual(package.app.wait.call_args_list, [mock.call(timeout=10), mock.call(timeout=5)])
            stop.assert_called_once_with()

    def test_cleanup_error_is_reported_without_replacing_primary_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            package = self.package(Path(temporary))
            package.owner_may_be_running = True
            with mock.patch.object(package, "stop_owner", side_effect=subprocess.TimeoutExpired("fixture stop", 100)):
                self.assertFalse(package.__exit__(AssertionError, AssertionError("primary"), None))
            self.assertIn("timed out", (package.report / "owner-cleanup-error.txt").read_text())
            with mock.patch.object(package, "stop_owner", side_effect=OSError("fixture stop failed")):
                with self.assertRaisesRegex(AssertionError, "Fixture owner cleanup failed"):
                    package.__exit__(None, None, None)

    def test_visible_more_action_does_not_scroll(self):
        desktop = fixture.Desktop(Path("unused-fixture"))
        with mock.patch.object(desktop, "click") as click, \
                mock.patch.object(desktop, "locate", return_value=(100, 100)), \
                mock.patch.object(fixture.subprocess, "run") as wheel:
            desktop.more("Pause all")
        self.assertEqual(click.call_args_list, [mock.call("More"), mock.call("Pause all")])
        wheel.assert_not_called()

    def test_more_scrolls_actual_popup_until_hidden_agent_action_is_visible(self):
        desktop = fixture.Desktop(Path("unused-fixture"))
        scrolls = []
        def locate(phrase, **kwargs):
            if phrase == "Agent access":
                return (160, 430) if len(scrolls) >= 2 else None
            return (130, 125) if phrase == "Remove selected" else None
        def scroll(command, **kwargs):
            self.assertEqual(command, ["xdotool", "mousemove", "--sync", "130", "125",
                                       "click", "--repeat", "3", "--delay", "80", "5"])
            self.assertTrue(kwargs["check"])
            self.assertEqual(kwargs["timeout"], 5)
            scrolls.append(command)
        with mock.patch.object(desktop, "click") as click, \
                mock.patch.object(desktop, "locate", side_effect=locate), \
                mock.patch.object(fixture.subprocess, "run", side_effect=scroll), \
                mock.patch.object(fixture.time, "sleep"):
            desktop.more("Agent access")
        self.assertEqual(len(scrolls), 2)
        self.assertEqual(click.call_args_list, [mock.call("More"), mock.call("Agent access")])

    def test_missing_more_action_stops_at_scroll_bound_without_clicking_target(self):
        desktop = fixture.Desktop(Path("unused-fixture"))
        def locate(phrase, **kwargs):
            return (100, 100) if phrase == "Selected visible" else None
        with mock.patch.object(desktop, "click") as click, \
                mock.patch.object(desktop, "locate", side_effect=locate), \
                mock.patch.object(fixture.subprocess, "run") as wheel, \
                mock.patch.object(fixture.time, "sleep"):
            with self.assertRaisesRegex(AssertionError, "not visible after 12 bounded scroll steps"):
                desktop.more("Absent action")
        self.assertEqual(wheel.call_count, 12)
        click.assert_called_once_with("More")

    def test_more_deadline_and_unknown_popup_do_not_send_unbounded_wheel_events(self):
        desktop = fixture.Desktop(Path("unused-fixture"))
        with mock.patch.object(desktop, "click"), \
                mock.patch.object(desktop, "locate", return_value=None), \
                mock.patch.object(fixture.subprocess, "run") as wheel, \
                mock.patch.object(fixture.time, "monotonic", side_effect=[0, 46]):
            with self.assertRaisesRegex(AssertionError, "not visible after 0 bounded scroll steps"):
                desktop.more("Agent access")
        wheel.assert_not_called()
        with mock.patch.object(desktop, "click"), \
                mock.patch.object(desktop, "locate", return_value=None), \
                mock.patch.object(fixture.subprocess, "run") as wheel:
            with self.assertRaisesRegex(AssertionError, "Could not locate the opened More menu"):
                desktop.more("Agent access")
        wheel.assert_not_called()

    def test_access_uses_trusted_enable_control_and_collapses_actual_window(self):
        for initially_collapsed in (False, True):
            with self.subTest(initially_collapsed=initially_collapsed):
                desktop = fixture.Desktop(Path("unused-fixture"))
                collapsed = [initially_collapsed]
                def locate(phrase):
                    return None if phrase == "Enable local IPC" and collapsed[0] else (100, 100)
                def toggle():
                    collapsed[0] = not collapsed[0]
                with mock.patch.object(desktop, "more") as more, \
                        mock.patch.object(desktop, "click") as click, \
                        mock.patch.object(desktop, "locate", side_effect=locate), \
                        mock.patch.object(desktop, "toggle_access_window", side_effect=toggle) as collapse, \
                        mock.patch.object(desktop, "screenshot"):
                    self.assertIsNone(desktop.grant(new=False))
                more.assert_called_once_with("Agent access")
                click.assert_called_once_with("Enable local IPC")
                self.assertEqual(collapse.call_count, 2 if initially_collapsed else 1)
                self.assertTrue(collapsed[0])

    def test_badge_ocr_rejects_partial_or_unrelated_words_and_preserves_trials(self):
        with tempfile.TemporaryDirectory() as temporary:
            desktop = fixture.Desktop(Path(temporary))
            def output(command, **kwargs):
                return SimpleNamespace(stdout=b"Done seeding\n" if command[0] == "tesseract" else b"")
            with mock.patch.object(desktop, "locate", return_value=(200, 100)), \
                    mock.patch.object(fixture.subprocess, "run", side_effect=output) as run:
                self.assertFalse(desktop.done())
            commands = [call.args[0] for call in run.call_args_list if call.args[0][0] == "convert"]
            self.assertEqual(len(commands), 3)
            self.assertTrue(all("140x36+172+118" in command for command in commands))
            self.assertEqual(len(list(Path(temporary).glob("done-ocr-*.txt"))), 3)

    def test_badge_ocr_accepts_only_exact_done_after_palette_fallback(self):
        with tempfile.TemporaryDirectory() as temporary:
            desktop = fixture.Desktop(Path(temporary))
            replies = iter((b"0%\n", b"Done\n"))
            def output(command, **kwargs):
                return SimpleNamespace(stdout=next(replies) if command[0] == "tesseract" else b"")
            with mock.patch.object(desktop, "locate", return_value=(200, 100)), \
                    mock.patch.object(fixture.subprocess, "run", side_effect=output):
                self.assertTrue(desktop.done())
            self.assertEqual((Path(temporary) / "done-ocr-B-45.txt").read_bytes(), b"Done\n")


if __name__ == "__main__":
    unittest.main()
