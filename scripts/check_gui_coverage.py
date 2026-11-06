"""Validate real LLVM coverage exports for the required GUI source scope.

No baseline is inferred from test counts or zero denominators. This is separate
from the existing Linux core/TUI gate and never changes that baseline.
"""

from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path, PurePosixPath
import shutil
import sys


SCOPE = "windows-gui-0.4.0"


def source_path(value: str) -> str:
    if not isinstance(value, str) or not value or "\x00" in value:
        raise ValueError("Invalid coverage source path")
    normalized = value.replace("\\", "/")
    parts = PurePosixPath(normalized).parts
    if ".." in parts or normalized.startswith("/") or ":" in normalized:
        raise ValueError("Scoped source paths must be relative without traversal")
    return str(PurePosixPath(normalized))


def relative_source(filename: str, repo_root: str) -> str | None:
    if not isinstance(filename, str) or not filename or "\x00" in filename:
        raise ValueError("Invalid LLVM filename")
    filename = filename.replace("\\", "/")
    root = repo_root.replace("\\", "/").rstrip("/")
    windows = len(root) > 1 and root[1] == ":"
    compared_file = filename.casefold() if windows else filename
    compared_root = root.casefold() if windows else root
    if compared_file.startswith(compared_root + "/"):
        filename = filename[len(root) + 1 :]
    elif filename.startswith("/") or (len(filename) > 1 and filename[1] == ":"):
        return None
    result = source_path(filename)
    return result.casefold() if windows else result


def read_scope(document: dict) -> list[str]:
    if not isinstance(document, dict) or document.get("schema") != 1 or document.get("scope") != SCOPE:
        raise ValueError("Invalid Windows GUI coverage scope or schema")
    files = document.get("files")
    if not isinstance(files, list) or not files:
        raise ValueError("Coverage scope must require source files")
    normalized = [source_path(name) for name in files]
    if len(set(name.casefold() for name in normalized)) != len(normalized):
        raise ValueError("Duplicate scoped source path")
    return normalized


def read_metric(summary: dict, name: str) -> dict:
    metric = summary.get(name)
    if not isinstance(metric, dict):
        raise ValueError(f"Missing LLVM {name} metric")
    count, covered, reported = (metric.get(field) for field in ("count", "covered", "percent"))
    if type(count) is not int or type(covered) is not int or count <= 0 or not 0 <= covered <= count:
        raise ValueError(f"Invalid or empty LLVM {name} counts")
    if type(reported) not in (int, float) or not math.isfinite(reported):
        raise ValueError(f"Invalid LLVM {name} percentage")
    percent = 100.0 * covered / count
    # LLVM exports percentages rounded to two decimals, not exact fractions.
    if abs(percent - reported) > 0.011:
        raise ValueError(f"Inconsistent LLVM {name} percentage")
    return {"count": count, "covered": covered, "uncovered": count - covered, "percent": percent}


def measure(document: dict, required: list[str], repo_root: str, context: list[str] | None = None) -> dict:
    if not isinstance(document, dict) or document.get("type") != "llvm.coverage.json.export":
        raise ValueError("Expected a real LLVM JSON coverage export")
    version = document.get("version")
    if not isinstance(version, str) or version.split(".")[0] not in ("2", "3"):
        raise ValueError("Unsupported LLVM JSON coverage schema")
    data = document.get("data")
    if not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict):
        raise ValueError("Expected exactly one merged LLVM coverage dataset")
    files = data[0].get("files")
    if not isinstance(files, list) or not files:
        raise ValueError("LLVM coverage file list is empty")
    windows = len(repo_root) > 1 and repo_root[1] == ":"
    context = [source_path(name) for name in context or []]
    all_files = required + context
    if len(set(name.casefold() for name in all_files)) != len(all_files):
        raise ValueError("Context source scope must be unique and separate from new modules")
    keys = {name.casefold() if windows else name: name for name in all_files}
    measured = {}
    for entry in files:
        if not isinstance(entry, dict):
            raise ValueError("Invalid LLVM file record")
        relative = relative_source(entry.get("filename"), repo_root)
        if relative not in keys:
            continue
        name = keys[relative]
        if name in measured:
            raise ValueError(f"Duplicate LLVM source record: {name}")
        summary = entry.get("summary")
        if not isinstance(summary, dict):
            raise ValueError(f"Missing per-file LLVM summary: {name}")
        measured[name] = {metric: read_metric(summary, metric) for metric in ("lines", "functions")}
    missing = sorted(set(all_files) - measured.keys())
    if missing:
        raise ValueError("Missing instrumented GUI sources: " + ", ".join(missing))
    for name in required:
        if any(measured[name][metric]["covered"] <= 0 for metric in ("lines", "functions")):
            raise ValueError(f"Required GUI source not exercised: {name}")
    totals = {}
    for metric in ("lines", "functions"):
        count = sum(measured[name][metric]["count"] for name in required)
        covered = sum(measured[name][metric]["covered"] for name in required)
        totals[metric] = {"count": count, "covered": covered, "uncovered": count - covered, "percent": 100.0 * covered / count}
    return {"schema": 1, "scope": SCOPE, "files": {name: measured[name] for name in required},
            "context_files": {name: measured[name] for name in context},
            "totals": totals, "baseline_gate": "not_established"}


def apply_baseline(report: dict, baseline: dict) -> None:
    if not isinstance(baseline, dict) or baseline.get("schema") != 1 or baseline.get("scope") != SCOPE:
        raise ValueError("Invalid GUI baseline scope or schema")
    files = baseline.get("files")
    if not isinstance(files, dict) or set(files) != set(report["files"]):
        raise ValueError("GUI baseline must cover the exact required source scope")
    for source, minima in [("totals", baseline.get("totals")), *files.items()]:
        if not isinstance(minima, dict):
            raise ValueError("Missing GUI baseline thresholds")
        measured = report["totals"] if source == "totals" else report["files"][source]
        for metric in ("lines", "functions"):
            minimum = minima.get(metric)
            if type(minimum) not in (int, float) or not math.isfinite(minimum) or not 0 <= minimum <= 100:
                raise ValueError(f"Invalid {source} {metric} baseline")
            if measured[metric]["percent"] + 0.000001 < minimum:
                raise ValueError(f"{source} {metric} {measured[metric]['percent']:.6f}% is below measured baseline {minimum}%")
    report["baseline_gate"] = "passed"


def check_space(path: Path, minimum_gib: float) -> dict:
    if not math.isfinite(minimum_gib) or minimum_gib <= 0:
        raise ValueError("Minimum free disk space must be positive")
    ancestor = path.resolve()
    while not ancestor.exists():
        if ancestor == ancestor.parent:
            raise ValueError("Cannot locate coverage build volume")
        ancestor = ancestor.parent
    free = shutil.disk_usage(ancestor).free
    required = math.ceil(minimum_gib * 1024**3)
    if free < required:
        raise ValueError(f"Coverage disk guard: {free} bytes free, {required} required; no build was started")
    return {"free_bytes": free, "minimum_bytes": required}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=Path("coverage/windows-gui/llvm.json"))
    parser.add_argument("--scope", type=Path, default=Path("config/windows-gui-coverage-scope.json"))
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--repo-root", default=os.getcwd())
    parser.add_argument("--output", type=Path)
    parser.add_argument("--summary", type=Path)
    parser.add_argument("--preflight", type=Path, metavar="BUILD_DIRECTORY")
    parser.add_argument("--minimum-free-gib", type=float, default=8.0)
    args = parser.parse_args(argv)
    try:
        if args.preflight:
            print(json.dumps(check_space(args.preflight, args.minimum_free_gib), sort_keys=True))
            return 0
        scope = json.loads(args.scope.read_text(encoding="utf-8"))
        required = read_scope(scope)
        report = measure(json.loads(args.report.read_text(encoding="utf-8")), required, args.repo_root, scope.get("context_files"))
        if args.baseline:
            apply_baseline(report, json.loads(args.baseline.read_text(encoding="utf-8")))
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        rows = ["Windows GUI source coverage (instrumented egui tests)", "", "| Source | Lines | Functions |", "| --- | ---: | ---: |"]
        for name, metrics in [*report["files"].items(), ("TOTAL", report["totals"])]:
            cells = [f"{metrics[key]['covered']}/{metrics[key]['count']} ({metrics[key]['percent']:.2f}%)" for key in ("lines", "functions")]
            rows.append(f"| {name} | {cells[0]} | {cells[1]} |")
        rows += ["", f"Numeric baseline gate: {report['baseline_gate']}. Counts are actual LLVM line/function coverage, not branch or platform certification."]
        if report["context_files"]:
            rows += ["", "Existing integration context (excluded from new-module totals):", "", "| Source | Lines | Functions |", "| --- | ---: | ---: |"]
            for name, metrics in report["context_files"].items():
                cells = [f"{metrics[key]['covered']}/{metrics[key]['count']} ({metrics[key]['percent']:.2f}%)" for key in ("lines", "functions")]
                rows.append(f"| {name} | {cells[0]} | {cells[1]} |")
        summary = "\n".join(rows) + "\n"
        print(summary, end="")
        if args.summary:
            with args.summary.open("a", encoding="utf-8") as stream:
                stream.write(summary)
        return 0
    except (ValueError, OSError, TypeError, KeyError) as error:
        print(f"GUI coverage verification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
