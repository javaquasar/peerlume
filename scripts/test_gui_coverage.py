"""Coverage parser/gate contracts, using LLVM's exported per-file JSON shape."""

import contextlib
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from check_gui_coverage import SCOPE, apply_baseline, check_space, main, measure, read_scope, relative_source


FILES = ["src/appearance.rs", "src/app/torrent_table.rs"]


def fixture():
    return {
        "type": "llvm.coverage.json.export", "version": "2.0.1",
        "data": [{"files": [{"filename": "C:\\repo\\" + name.replace("/", "\\"), "summary": {
            "lines": {"count": 3, "covered": 2, "percent": 66.67},
            "functions": {"count": 4, "covered": 3, "percent": 75},
        }} for name in FILES]}],
    }


def baseline():
    return {"schema": 1, "scope": SCOPE, "totals": {"lines": 66.66, "functions": 75},
            "files": {name: {"lines": 66.66, "functions": 75} for name in FILES}}


class CoverageContracts(unittest.TestCase):
    def test_existing_integration_context_is_measured_separately_not_added_to_new_scope(self):
        document = fixture()
        item = copy.deepcopy(document["data"][0]["files"][0])
        item["filename"] = "C:/repo/src/app.rs"
        document["data"][0]["files"].append(item)
        report = measure(document, FILES, "C:/repo", ["src/app.rs"])
        self.assertEqual(report["totals"]["lines"]["count"], 6)
        self.assertEqual(report["context_files"]["src/app.rs"]["lines"]["count"], 3)
        self.assertNotIn("src/app.rs", report["files"])
        for context in ([FILES[0]], ["../app.rs"], ["src/missing.rs"]):
            with self.assertRaises(ValueError):
                measure(document, FILES, "C:/repo", context)

    def test_every_required_module_must_execute_lines_and_functions_but_context_can_be_uncovered(self):
        for metric in ("lines", "functions"):
            document = fixture()
            document["data"][0]["files"][0]["summary"][metric].update({"covered": 0, "percent": 0})
            with self.assertRaisesRegex(ValueError, "Required GUI source not exercised"):
                measure(document, FILES, "C:/repo")
        document = fixture()
        item = copy.deepcopy(document["data"][0]["files"][0])
        item["filename"] = "C:/repo/src/app.rs"
        for metric in ("lines", "functions"):
            item["summary"][metric].update({"covered": 0, "percent": 0})
        document["data"][0]["files"].append(item)
        report = measure(document, FILES, "C:/repo", ["src/app.rs"])
        self.assertEqual(report["context_files"]["src/app.rs"]["lines"]["covered"], 0)

    def test_real_llvm_shape_rounding_and_weighted_counts(self):
        document = fixture()
        document["data"][0]["files"].append({"filename": "C:/dependency/src/appearance.rs", "summary": {}})
        report = measure(document, FILES, "C:/repo")
        self.assertEqual(report["totals"]["lines"]["count"], 6)
        self.assertEqual(report["totals"]["lines"]["covered"], 4)
        self.assertEqual(report["totals"]["lines"]["uncovered"], 2)
        self.assertEqual(report["baseline_gate"], "not_established")
        self.assertAlmostEqual(report["totals"]["lines"]["percent"], 200 / 3)

    def test_llvm_21_v3_summary_keeps_the_same_required_line_function_checks(self):
        document = fixture()
        document["version"] = "3.0.1"
        for entry in document["data"][0]["files"]:
            entry["summary"]["mcdc"] = {"count": 0, "covered": 0, "notcovered": 0, "percent": 0}
            entry["summary"]["branches"] = {"count": 0, "covered": 0, "notcovered": 0, "percent": 0}
        report = measure(document, FILES, "C:/repo")
        self.assertEqual(report["totals"]["lines"]["covered"], 4)
        self.assertEqual(report["totals"]["functions"]["covered"], 6)
        document["data"][0]["files"][0]["summary"]["functions"].update({"covered": 0, "percent": 0})
        with self.assertRaisesRegex(ValueError, "Required GUI source not exercised"):
            measure(document, FILES, "C:/repo")

    def test_windows_case_separator_and_exact_root(self):
        self.assertEqual(relative_source("c:\\REPO\\src\\appearance.rs", "C:/repo/"), FILES[0])
        self.assertEqual(relative_source("src/appearance.rs", "C:/repo"), FILES[0])
        self.assertIsNone(relative_source("C:/repo-other/src/appearance.rs", "C:/repo"))
        self.assertEqual(relative_source("/repo/src/appearance.rs", "/repo"), FILES[0])
        self.assertIsNone(relative_source("/REPO/src/appearance.rs", "/repo"))
        for filename in ("src/../appearance.rs", "../src/appearance.rs", "src/\x00.rs", None):
            with self.assertRaises(ValueError):
                relative_source(filename, "C:/repo")

    def test_scope_is_nonempty_unique_and_traversal_free(self):
        self.assertEqual(read_scope({"schema": 1, "scope": SCOPE, "files": FILES}), FILES)
        for files in ([], [FILES[0], "SRC/APPEARANCE.RS"], ["../src/a.rs"], ["C:/src/a.rs"], [""]):
            with self.assertRaises(ValueError):
                read_scope({"schema": 1, "scope": SCOPE, "files": files})
        with self.assertRaises(ValueError):
            read_scope({"schema": 1, "scope": "linux-core-tui", "files": FILES})

    def test_missing_duplicate_or_empty_source_never_passes(self):
        document = fixture()
        for changed in ([], None, {}, {"type": "wrong", "version": "2.0.1", "data": []},
                        {**document, "version": "4.0.0"}, {**document, "data": []},
                        {**document, "data": document["data"] * 2},
                        {**document, "data": [{"files": []}]}):
            with self.assertRaises(ValueError):
                measure(changed, FILES, "C:/repo")
        changed = fixture()
        changed["data"][0]["files"].pop()
        with self.assertRaisesRegex(ValueError, "Missing instrumented"):
            measure(changed, FILES, "C:/repo")
        changed = fixture()
        changed["data"][0]["files"].append(copy.deepcopy(changed["data"][0]["files"][0]))
        with self.assertRaisesRegex(ValueError, "Duplicate LLVM"):
            measure(changed, FILES, "C:/repo")

    def test_invalid_counts_and_forged_percentages_are_rejected(self):
        for metric in ("lines", "functions"):
            for field, value in (("count", 0), ("count", True), ("count", 1.5), ("covered", -1),
                                 ("covered", 99), ("percent", "75"), ("percent", 100), ("percent", float("nan"))):
                changed = fixture()
                changed["data"][0]["files"][0]["summary"][metric][field] = value
                with self.assertRaises(ValueError, msg=f"{metric} {field} {value}"):
                    measure(changed, FILES, "C:/repo")
        changed = fixture()
        del changed["data"][0]["files"][0]["summary"]["functions"]
        with self.assertRaises(ValueError):
            measure(changed, FILES, "C:/repo")

    def test_measured_baseline_gates_each_file_and_totals(self):
        report = measure(fixture(), FILES, "C:/repo")
        apply_baseline(report, baseline())
        self.assertEqual(report["baseline_gate"], "passed")
        for section in ("totals", *FILES):
            for metric in ("lines", "functions"):
                changed = baseline()
                minima = changed[section] if section == "totals" else changed["files"][section]
                minima[metric] = 100
                with self.assertRaisesRegex(ValueError, "below measured baseline"):
                    apply_baseline(report, changed)
        for bad in ("wrong", 0, None):
            changed = baseline()
            changed["scope"] = bad
            with self.assertRaises(ValueError):
                apply_baseline(report, changed)
        for bad in ("75", True, -1, 101, float("inf")):
            changed = baseline()
            changed["totals"]["lines"] = bad
            with self.assertRaises(ValueError):
                apply_baseline(report, changed)
        changed = baseline()
        changed["files"].pop(FILES[0])
        with self.assertRaises(ValueError):
            apply_baseline(report, changed)

    def test_disk_guard_fails_before_creating_build_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "not-created" / "coverage"
            with patch("check_gui_coverage.shutil.disk_usage") as usage:
                usage.return_value.free = 1024
                with self.assertRaisesRegex(ValueError, "no build was started"):
                    check_space(destination, 8)
                usage.return_value.free = 9 * 1024**3
                self.assertEqual(check_space(destination, 8)["minimum_bytes"], 8 * 1024**3)
            self.assertFalse(destination.exists())
            for bad in (0, -1, float("nan")):
                with self.assertRaises(ValueError):
                    check_space(destination, bad)

    def test_cli_writes_actual_report_and_fails_closed_without_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            report, scope, output, summary, minimum = [directory / name for name in ("llvm.json", "scope.json", "out.json", "summary.md", "baseline.json")]
            report.write_text(json.dumps(fixture()), encoding="utf-8")
            scope.write_text(json.dumps({"schema": 1, "scope": SCOPE, "files": FILES}), encoding="utf-8")
            minimum.write_text(json.dumps(baseline()), encoding="utf-8")
            args = ["--report", str(report), "--scope", str(scope), "--output", str(output), "--summary", str(summary), "--repo-root", "C:/repo"]
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(main(args), 0)
                self.assertEqual(main(args + ["--baseline", str(minimum)]), 0)
            self.assertEqual(json.loads(output.read_text())["baseline_gate"], "passed")
            self.assertIn("not_established", summary.read_text())
            self.assertIn("passed", summary.read_text())
            report.unlink()
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(main(args), 1)


if __name__ == "__main__":
    unittest.main()
