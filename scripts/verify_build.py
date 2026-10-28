"""Verify and smoke-test the archive, not executables left in Cargo's target directory."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

from package_build import FLAVORS, DOCUMENTS, TARGETS, check_binary, inside_repository, sha256, package_name


def extract_and_verify(archive, platform, commit, version, destination, flavor="desktop"):
    if flavor not in FLAVORS:
        raise ValueError("Unknown package flavor")
    extension = ".zip" if platform == "windows-x64" else ".tar.gz"
    name = package_name(version, platform, commit, flavor)
    if archive.name != name + extension:
        raise ValueError("Archive name does not match the expected version, platform and commit")
    expected_outer = f"{sha256(archive)}  {archive.name}\n"
    if Path(str(archive) + ".sha256").read_text(encoding="utf-8") != expected_outer:
        raise ValueError("Archive SHA-256 mismatch")
    windows = platform == "windows-x64"
    expected_files = set(DOCUMENTS.values()) | {"build-info.json", "SHA256SUMS"}
    expected_files.update(binary + ".exe" if windows else "bin/" + binary for binary in FLAVORS[flavor])
    if not windows and flavor == "desktop":
        expected_files.update({"share/applications/peerlume.desktop", "share/icons/hicolor/256x256/apps/peerlume.png"})
    expected_members = {name + "/" + file for file in expected_files}
    root = destination / name
    if root.exists():
        raise ValueError("Extraction destination already exists")
    with (zipfile.ZipFile(archive) if windows else tarfile.open(archive)) as bundle:
        members = bundle.infolist() if windows else bundle.getmembers()
        names = [member.filename if windows else member.name for member in members]
        if len(names) != len(expected_members) or set(names) != expected_members:
            raise ValueError("Unexpected, duplicate or missing archive entries")
        total = 0
        for member, member_name in zip(members, names):
            size = member.file_size if windows else member.size
            total += size
            if size > 256 * 1024 * 1024 or total > 768 * 1024 * 1024:
                raise ValueError("Archive exceeds the extraction size budget")
            relative = PurePosixPath(member_name).relative_to(name)
            if not windows:
                mode = 0o755 if relative.parts[0] == "bin" else 0o644
                if not member.isfile() or member.mode != mode:
                    raise ValueError("Linux package contains a non-file or incorrect permissions")
            elif member.is_dir() or (member.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError("Windows package contains a directory or symbolic link")
            target = root.joinpath(*relative.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            with (bundle.open(member) if windows else bundle.extractfile(member)) as source:
                with target.open("xb") as stream:
                    shutil.copyfileobj(source, stream)
            if not windows:
                target.chmod(member.mode)
    checksums = (root / "SHA256SUMS").read_text(encoding="utf-8").splitlines()
    checked = set()
    for line in checksums:
        digest, relative = line.split("  ", 1)
        if relative in checked or relative not in expected_files - {"SHA256SUMS"}:
            raise ValueError("Unexpected or duplicate checksum entry")
        checked.add(relative)
        if sha256(root / relative) != digest:
            raise ValueError(f"Packaged file SHA-256 mismatch: {relative}")
    if checked != expected_files - {"SHA256SUMS"}:
        raise ValueError("Incomplete SHA256SUMS")
    info = json.loads((root / "build-info.json").read_text(encoding="utf-8"))
    for key, value in {"commit": commit, "version": version, "platform": platform,
                       "target": TARGETS[platform], "profile": "release", "features": ["gui", "tui"] if flavor == "desktop" else ["tui"]}.items():
        if info.get(key) != value:
            raise ValueError(f"Build manifest mismatch: {key}")
    for binary in FLAVORS[flavor]:
        check_binary(root / (binary + ".exe" if windows else "bin/" + binary), platform)
    return root


def smoke(root, platform, version, flavor="desktop"):
    windows = platform == "windows-x64"
    folder = root if windows else root / "bin"
    suffix = ".exe" if windows else ""
    # No connection to a running application or user's profile is needed.
    environment = dict(os.environ)
    environment.pop("PEERLUME_CONTROL_TOKEN", None)
    result = subprocess.run([str(folder / ("peerlume-control" + suffix)), "--version"],
                            capture_output=True, text=True, check=True, timeout=15, env=environment)
    if not result.stdout.startswith(f"peerlume-control {version} protocol "):
        raise ValueError("Packaged CLI reported an unexpected version")
    result = subprocess.run([str(folder / ("peerlume-tui" + suffix)), "--version"],
                            capture_output=True, text=True, check=True, timeout=15, env=environment)
    if result.stdout.strip() != f"peerlume-tui {version}":
        raise ValueError("Packaged TUI reported an unexpected version")
    request = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "package-smoke", "version": "1"}}}
    with tempfile.TemporaryDirectory(prefix="peerlume-build-profile-") as profile:
        result = subprocess.run([str(folder / ("peerlume-mcp" + suffix)), "--profile", profile],
                                input=json.dumps(request) + "\n", capture_output=True, text=True,
                                check=True, timeout=15, env=environment)
    reply = json.loads(result.stdout)
    if (reply.get("id") != 1 or reply.get("result", {}).get("serverInfo")
            != {"name": "peerlume", "version": version}):
        raise ValueError("Packaged MCP adapter did not initialize")
    if not windows:
        for binary in FLAVORS[flavor]:
            result = subprocess.run(["ldd", str(folder / binary)], capture_output=True,
                                    text=True, check=True, timeout=15)
            if "not found" in result.stdout + result.stderr:
                raise ValueError(f"Missing Linux runtime library: {binary}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=TARGETS, required=True)
    parser.add_argument("--flavor", choices=FLAVORS, default="desktop")
    parser.add_argument("--commit", required=True)
    parser.add_argument("--directory", default="dist/builds")
    args = parser.parse_args()
    if {"win32": "windows-x64", "linux": "linux-x64"}.get(sys.platform) != args.platform:
        parser.error("Smoke checks must run on the matching native operating system")
    repository = Path(__file__).resolve().parent.parent
    version = tomllib.loads((repository / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    directory = inside_repository(repository, args.directory)
    extension = ".zip" if args.platform == "windows-x64" else ".tar.gz"
    archive = directory / (package_name(version, args.platform, args.commit, args.flavor) + extension)
    with tempfile.TemporaryDirectory(prefix="peerlume-build-verify-") as temporary:
        root = extract_and_verify(archive, args.platform, args.commit, version, Path(temporary), args.flavor)
        smoke(root, args.platform, version, args.flavor)
    print(f"Verified archive, every SHA-256, executable architecture, CLI and MCP: {archive.name}")


if __name__ == "__main__":
    main()
