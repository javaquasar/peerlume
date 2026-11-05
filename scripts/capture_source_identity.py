#!/usr/bin/env python3
"""Hash the complete local source candidate without claiming a new Git commit."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--base-commit", required=True)
    parser.add_argument("--candidate-label", required=True)
    parser.add_argument("--report", type=Path, required=True)
    options = parser.parse_args()
    if not re.fullmatch(r"[0-9a-fA-F]{40}", options.base_commit):
        parser.error("base-commit must be a complete40-digit Git SHA")
    if options.report.exists():
        parser.error("report path must be new")
    repository = options.repository.resolve(strict=True)
    command = ["git", "-c", f"safe.directory={repository.as_posix()}", "-c", "core.excludesFile=",
               "-c", "core.autocrlf=false", "-C", str(repository)]
    if subprocess.check_output(command + ["cat-file", "-t", options.base_commit], timeout=10).strip() != b"commit":
        parser.error("base-commit does not identify a local commit")
    head = subprocess.check_output(command + ["rev-parse", "HEAD"], timeout=10).decode().strip()
    status = subprocess.check_output(command + ["status", "--porcelain", "--untracked-files=all"], timeout=10)
    selected = set()
    for directory in ("src", "examples", "tests", "vendor"):
        selected.update((repository / directory).rglob("*.rs"))
    selected.update((repository / "vendor").rglob("Cargo.toml"))
    for name in ("Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain.toml", "rust-toolchain", ".cargo/config", ".cargo/config.toml"):
        if (path := repository / name).is_file():
            selected.add(path)
    selected.update(path for path in (repository / "assets").rglob("*") if path.is_file())
    selected.update(path for path in (repository / "scripts").rglob("*")
                    if path.is_file() and path.suffix in (".py", ".ps1", ".sh"))
    files = {}
    for path in sorted(selected, key=lambda item: item.relative_to(repository).as_posix()):
        if path.is_symlink() or not path.resolve().is_relative_to(repository):
            raise ValueError("Source input is not a regular contained file: " + path.relative_to(repository).as_posix())
        contents = path.read_bytes()
        files[path.relative_to(repository).as_posix()] = {"sha256": hashlib.sha256(contents).hexdigest(), "bytes": len(contents)}
    canonical = json.dumps(files, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    report = {"schema_version": 1, "kind": "source_candidate_identity", "base_commit": options.base_commit.lower(),
              "git_head_at_capture": head, "candidate_label": options.candidate_label,
              "candidate_is_a_new_commit": False, "worktree_has_changes": bool(status),
              "source_file_count": len(files), "source_bytes": sum(item["bytes"] for item in files.values()),
              "file_set_sha256": hashlib.sha256(canonical).hexdigest(),
              "scope": ["src/**/*.rs", "examples/**/*.rs", "tests/**/*.rs", "vendor/**/*.rs", "vendor/**/Cargo.toml",
                        "Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain*", ".cargo/config*", "assets/**/*",
                        "scripts/**/*.{py,ps1,sh}"],
              "excluded": ["target", "docs", ".git", "user profiles", "build outputs"],
              "identity_limits": "Source bytes observed at capture, not a claim that a nonexistent commit exists; bind to recorded build commands and executable SHA-256 values",
              "files": files}
    with options.report.open("x", encoding="utf-8") as output:
        json.dump(report, output, indent=2, ensure_ascii=False)
        output.write("\n")
    print(json.dumps({"source_file_count": len(files), "file_set_sha256": report["file_set_sha256"]}))


if __name__ == "__main__":
    main()
