"""Copy candidate aliases to a new disposable package; never launch a process."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sys
import uuid


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare(gui: Path, daemon: Path, parent: Path) -> Path:
    gui, daemon, parent = gui.resolve(), daemon.resolve(), parent.resolve()
    if gui.stem != "ui-candidate" or daemon.stem != "ui-candidate-daemon":
        raise ValueError("Use the two candidate alias artifacts, not a live application's binaries")
    if not gui.is_file() or not daemon.is_file() or not parent.is_dir():
        raise ValueError("Both alias artifacts and the evidence parent directory must exist")
    if gui.suffix != daemon.suffix or gui.suffix not in ("", ".exe"):
        raise ValueError("Candidate artifact platform suffixes must match")
    expected = b"MZ" if gui.suffix == ".exe" else b"\x7fELF"
    for path in (gui, daemon):
        with path.open("rb") as stream:
            if not stream.read(4).startswith(expected):
                raise ValueError("Candidate alias does not have the expected executable header")
    directory = parent / ("ui-candidate-" + uuid.uuid4().hex)
    directory.mkdir()
    extension = gui.suffix
    artifacts = {}
    for source, name in ((gui, "peerlume"), (daemon, "peerlume-daemon")):
        destination = directory / (name + extension)
        shutil.copy2(source, destination)
        if digest(source) != digest(destination):
            raise ValueError("Candidate artifact changed while copying")
        artifacts[destination.name] = {"source_alias": source.name, "bytes": destination.stat().st_size, "sha256": digest(destination)}
    profile = directory / "profile"
    manifest = {
        "schema": 1, "directory": str(directory), "profile": str(profile), "process_started": False,
        "artifacts": artifacts,
        "prepare_fixture_env": {"PEERLUME_UI_CANDIDATE_PROFILE": str(profile)},
        "prepare_fixture_argv": ["cargo", "+stable", "test", "--locked", "--all-features", "--test", "ui_candidate_fixture", "--", "--ignored", "--exact", "prepare_native_ui_candidate_profile", "--nocapture"],
        "launch_argv": [str(directory / ("peerlume" + extension)), "--profile", str(profile)],
        "stop_disposable_owner_argv": [str(directory / ("peerlume-daemon" + extension)), "--profile", str(profile), "--stop"],
        "note": "Run fixture preparation explicitly before launch. Only this new profile is intended for native UI QA; no existing owner was contacted.",
    }
    (directory / "candidate.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return directory


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", type=Path, required=True)
    parser.add_argument("--daemon", type=Path, required=True)
    parser.add_argument("--output-parent", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        print(json.dumps({"directory": str(prepare(args.gui, args.daemon, args.output_parent)), "process_started": False}))
        return 0
    except (OSError, ValueError) as error:
        print(f"Candidate preparation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
