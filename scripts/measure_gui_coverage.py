"""Instrument only the PeerLume library, reusing normal dependency artifacts.

This local alternative to cargo-llvm-cov uses the same rustc/LLVM coverage format.
It never cleans target, launches a user client, or instruments dependencies.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from check_gui_coverage import check_space, measure, read_scope


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def test_executable(messages: str) -> Path:
    candidates = set()
    for line in messages.splitlines():
        if not line.strip():
            continue
        item = json.loads(line)
        if (item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "peerlume"
                and item.get("target", {}).get("kind") == ["lib"] and item.get("profile", {}).get("test") is True
                and item.get("executable")):
            candidates.add(item["executable"])
    if len(candidates) != 1:
        raise ValueError(f"Expected exactly one Cargo library test executable, found {len(candidates)}")
    return Path(candidates.pop())


def tool_paths(sysroot: Path, host: str) -> tuple[Path, Path]:
    extension = ".exe" if os.name == "nt" else ""
    directory = sysroot / "lib" / "rustlib" / host / "bin"
    tools = tuple(directory / (name + extension) for name in ("llvm-profdata", "llvm-cov"))
    if not all(path.is_file() for path in tools):
        raise ValueError("Matching llvm-tools-preview is not installed for this Rust toolchain")
    return tools


def isolated_environment(base: dict[str, str], evidence: Path) -> dict[str, str]:
    environment = base.copy()
    for variable, directory in (("LOCALAPPDATA", "localappdata"), ("APPDATA", "appdata"),
                                ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                                ("XDG_CACHE_HOME", "cache")):
        environment[variable] = str(evidence / "test-environment" / directory)
    return environment


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path, help="New evidence directory; existing directories are rejected")
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--toolchain", default="stable")
    parser.add_argument("--minimum-free-gib", type=float, default=2.0)
    parser.add_argument("--test-threads", type=int, default=2)
    parser.add_argument("--timeout-seconds", type=int, default=1800)
    args = parser.parse_args(argv)
    evidence = args.output.resolve()
    repo = args.repo_root.resolve()
    created = False
    manifest = {"schema": 1, "scope": "windows-gui-0.4.0", "completed": False,
                "instrumented_crate": "peerlume", "instrumented_dependencies": False,
                "real_desktop_window": False, "commands": []}
    try:
        if evidence.exists():
            raise ValueError("Evidence directory already exists; choose a new path")
        if args.test_threads <= 0 or args.timeout_seconds <= 0:
            raise ValueError("Test threads and timeout must be positive")
        if os.environ.get("RUSTC_WORKSPACE_WRAPPER"):
            raise ValueError("An existing RUSTC_WORKSPACE_WRAPPER must be explicitly cleared before this capture")
        manifest["disk_preflight"] = check_space(repo / "target", args.minimum_free_gib)
        version = subprocess.check_output(["rustc", f"+{args.toolchain}", "-Vv"], text=True, timeout=30)
        if "release: 1.93.0\n" not in version:
            raise ValueError("This capture requires the pinned Rust 1.93.0 toolchain")
        sysroot = Path(subprocess.check_output(["rustc", f"+{args.toolchain}", "--print", "sysroot"], text=True, timeout=30).strip())
        host = next((line.removeprefix("host: ") for line in version.splitlines() if line.startswith("host: ")), "")
        profdata, llvm_cov = tool_paths(sysroot, host)
        manifest["rustc_version"] = version.splitlines()
        manifest["llvm_cov_version"] = subprocess.check_output([str(llvm_cov), "--version"], text=True, timeout=30).splitlines()
        scope = json.loads((repo / "config/windows-gui-coverage-scope.json").read_text(encoding="utf-8"))
        required = read_scope(scope)
        context = scope.get("context_files", [])
        evidence.mkdir(parents=True)
        created = True
        (evidence / "profiles").mkdir()
        manifest["source_sha256"] = {name: sha256(repo / name) for name in required + context}
        wrapper_source = repo / "scripts/gui_coverage_wrapper.rs"
        wrapper = repo / ".tools/gui-coverage/gui-coverage-wrapper.exe" if os.name == "nt" else repo / ".tools/gui-coverage/gui-coverage-wrapper"
        wrapper.parent.mkdir(parents=True, exist_ok=True)

        def run(command, name, env=None):
            manifest["commands"].append({"argv": [str(value) for value in command], "stdout": name + ".stdout.log", "stderr": name + ".stderr.log"})
            start = time.monotonic()
            with (evidence / (name + ".stdout.log")).open("wb") as stdout, (evidence / (name + ".stderr.log")).open("wb") as stderr:
                completed = subprocess.run([str(value) for value in command], cwd=repo, env=env,
                                           stdout=stdout, stderr=stderr, timeout=args.timeout_seconds)
            manifest["commands"][-1].update({"exit_code": completed.returncode, "wall_seconds": time.monotonic() - start})
            if completed.returncode:
                raise ValueError(f"{name} failed with exit {completed.returncode}; inspect captured logs")

        run(["rustc", f"+{args.toolchain}", "--edition=2024", wrapper_source, "-o", wrapper], "build-wrapper")
        wrapper_tests = evidence / ("wrapper-tests.exe" if os.name == "nt" else "wrapper-tests")
        run(["rustc", f"+{args.toolchain}", "--edition=2024", "--test", wrapper_source, "-o", wrapper_tests], "build-wrapper-tests")
        run([wrapper_tests], "wrapper-tests")
        environment = isolated_environment(dict(os.environ), evidence)
        for variable in ("LOCALAPPDATA", "APPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"):
            Path(environment[variable]).mkdir(parents=True, exist_ok=True)
        manifest["isolated_test_environment"] = {variable: str(Path(environment[variable]).relative_to(evidence)) for variable in
                                                  ("LOCALAPPDATA", "APPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME")}
        environment.update({"RUSTC_WORKSPACE_WRAPPER": str(wrapper), "CARGO_INCREMENTAL": "0", "CARGO_BUILD_JOBS": "2",
                            "CARGO_TARGET_DIR": str(repo / "target"), "LLVM_PROFILE_FILE": str(evidence / "profiles/gui-%p-%m.profraw")})
        run(["cargo", f"+{args.toolchain}", "test", "--locked", "--all-features", "--lib", "-p", "peerlume", "--no-run", "--message-format=json-render-diagnostics"], "build-instrumented-library", environment)
        executable = test_executable((evidence / "build-instrumented-library.stdout.log").read_text(encoding="utf-8"))
        if not executable.is_file():
            raise ValueError("Cargo's reported library test executable is missing")
        manifest["test_executable_sha256"] = sha256(executable)
        manifest["wrapper_source_sha256"] = sha256(wrapper_source)
        run([executable, f"--test-threads={args.test_threads}"], "instrumented-library-tests", environment)
        profiles = sorted((evidence / "profiles").glob("*.profraw"))
        if not profiles:
            raise ValueError("Instrumented tests produced no LLVM profiles")
        manifest["profile_files"] = [{"file": path.name, "bytes": path.stat().st_size, "sha256": sha256(path)} for path in profiles]
        merged = evidence / "gui.profdata"
        run([profdata, "merge", "-sparse", *profiles, "-o", merged], "merge-profiles")
        # Summary-only still includes the measured line/function counts per file.
        # Full LCOV below retains uncovered line locations without enormous JSON.
        run([llvm_cov, "export", executable, f"-instr-profile={merged}", "-summary-only"], "llvm-json")
        llvm_json = evidence / "llvm.json"
        (evidence / "llvm-json.stdout.log").replace(llvm_json)
        manifest["commands"][-1]["stdout"] = "llvm.json"
        run([llvm_cov, "export", executable, f"-instr-profile={merged}", "-format=lcov"], "llvm-lcov")
        (evidence / "llvm-lcov.stdout.log").replace(evidence / "lcov.info")
        manifest["commands"][-1]["stdout"] = "lcov.info"
        report = measure(json.loads(llvm_json.read_text(encoding="utf-8")), required, str(repo), context)
        (evidence / "scoped-summary.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        if manifest["source_sha256"] != {name: sha256(repo / name) for name in required + context}:
            raise ValueError("Required GUI source changed during instrumentation")
        manifest["source_unchanged"] = True
        manifest["completed"] = True
        print(json.dumps({"completed": True, "evidence": str(evidence), "totals": report["totals"]}))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        manifest["error"] = str(error)
        print(f"GUI coverage capture failed: {error}", file=sys.stderr)
        return 1
    finally:
        if created:
            (evidence / "capture.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(main())
