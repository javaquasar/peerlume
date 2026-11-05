#!/usr/bin/env python3
"""Build the PeerLume frontend and its background owner; Release is the default."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def launch_plan(arguments: list[str], root: Path) -> dict:
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog="Use -- before application arguments that resemble launcher options.",
    )
    parser.add_argument("--dev", "--development", action="store_true", help="use the development build")
    parser.add_argument("--tui", action="store_true", help="run the terminal frontend")
    parser.add_argument("--no-build", action="store_true", help="run the selected existing build")
    parser.add_argument("--print-command", action="store_true", help="print JSON without building or launching")
    separator = arguments.index("--") if "--" in arguments else len(arguments)
    options, forwarded = parser.parse_known_args(arguments[:separator])
    if separator < len(arguments):
        forwarded.extend(arguments[separator + 1 :])
    profile = "debug" if options.dev else "release"
    binary_name = "peerlume-tui" if options.tui else "peerlume"
    cargo = ["cargo", "build", "--locked", "--bin", binary_name, "--bin", "peerlume-daemon"]
    cargo.extend(["--no-default-features", "--features", "tui"] if options.tui else ["--all-features"])
    if not options.dev:
        cargo.append("--release")
    target_root = Path(os.environ.get("CARGO_TARGET_DIR") or root / "target")
    if not target_root.is_absolute():
        target_root = root / target_root
    target = os.environ.get("CARGO_BUILD_TARGET")
    if target:
        # Cargo uses the JSON target's file stem as the output subdirectory.
        target_root /= Path(target).stem if target.endswith(".json") else target
    binary = target_root / profile / (binary_name + (".exe" if os.name == "nt" else ""))
    daemon = binary.with_name("peerlume-daemon" + (".exe" if os.name == "nt" else ""))
    return {
        "profile": profile,
        "frontend": "tui" if options.tui else "gui",
        "repository": str(root),
        "build_command": [] if options.no_build else cargo,
        "client_command": [str(binary), *forwarded],
        "daemon_executable": str(daemon),
        "print_only": options.print_command,
    }


def main(arguments: list[str] | None = None, *, root: Path | None = None) -> int:
    root = root or Path(__file__).resolve().parent.parent
    plan = launch_plan(sys.argv[1:] if arguments is None else arguments, root)
    if plan["print_only"]:
        # Escaped JSON also works in legacy Windows console encodings.
        print(json.dumps(plan, ensure_ascii=True, indent=2))
        return 0
    try:
        if plan["build_command"]:
            result = subprocess.run(plan["build_command"], cwd=root, check=False)
            if result.returncode:
                return result.returncode
        if not Path(plan["client_command"][0]).is_file():
            print("The selected PeerLume executable does not exist; build it first.", file=sys.stderr)
            return 1
        if not Path(plan["daemon_executable"]).is_file():
            print("The matching peerlume-daemon executable is missing beside the frontend; build the complete application first.", file=sys.stderr)
            return 1
        # The frontend attaches to the profile owner or starts its sibling daemon.
        # No shell expansion or rewriting of an existing profile is performed here.
        return subprocess.run(plan["client_command"], cwd=root, check=False).returncode
    except OSError as error:
        print(f"Cannot build or launch PeerLume: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
