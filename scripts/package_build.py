"""Package only explicitly approved build outputs; never copy a user profile."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import tarfile
import tempfile
import tomllib
import zipfile


BINARIES = ("peerlume", "peerlume-tui", "peerlume-control", "peerlume-mcp")
TARGETS = {
    "windows-x64": "x86_64-pc-windows-msvc",
    "linux-x64": "x86_64-unknown-linux-gnu",
}
DOCUMENTS = {
    "docs/distribution.md": "README.md",
    "docs/agent-control.md": "docs/agent-control.md",
    "docs/runtime-controls.md": "docs/runtime-controls.md",
    "config/peerlume.example.toml": "config/peerlume.example.toml",
    "config/tracker-behaviors.example.toml": "config/tracker-behaviors.example.toml",
}


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require_file(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"Missing or symbolic-link package input: {path}")


def check_binary(path, platform):
    require_file(path)
    with path.open("rb") as stream:
        header = stream.read(64)
        if platform == "windows-x64":
            if len(header) != 64 or header[:2] != b"MZ":
                raise ValueError(f"Not a Windows executable: {path}")
            stream.seek(struct.unpack_from("<I", header, 60)[0])
            if stream.read(6) != b"PE\0\0\x64\x86":
                raise ValueError(f"Not an x64 Windows executable: {path}")
        elif (len(header) != 64 or header[:6] != b"\x7fELF\x02\x01"
              or struct.unpack_from("<H", header, 18)[0] != 62):
            raise ValueError(f"Not an x64 little-endian Linux ELF executable: {path}")


def inside_repository(root, value):
    path = (root / value).resolve()
    if path == root or not path.is_relative_to(root):
        raise ValueError("Build inputs and output directory must stay below the repository root")
    return path


def package(root, binary_directory, output_directory, platform, commit, rust_version):
    root = Path(root).resolve()
    if platform not in TARGETS or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Expected a supported platform and full lowercase Git commit SHA")
    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?", version):
        raise ValueError("Unsafe or invalid package version")
    binaries = inside_repository(root, binary_directory)
    output = inside_repository(root, output_directory)
    windows = platform == "windows-x64"
    name = f"peerlume-{version}-{platform}-{commit[:12]}"
    archive = output / (name + (".zip" if windows else ".tar.gz"))
    sidecar = Path(str(archive) + ".sha256")
    if archive.exists() or sidecar.exists():
        raise ValueError("Package already exists; choose a new output directory")
    files = []
    for binary in BINARIES:
        filename = binary + (".exe" if windows else "")
        source = binaries / filename
        check_binary(source, platform)
        files.append((source, filename if windows else f"bin/{filename}", True))
    documents = dict(DOCUMENTS)
    if not windows:
        documents.update({
            "assets/linux/peerlume.desktop": "share/applications/peerlume.desktop",
            "assets/icons/peerlume-256.png": "share/icons/hicolor/256x256/apps/peerlume.png",
        })
    for source, destination in documents.items():
        source = root / source
        require_file(source)
        files.append((source, destination, False))
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".peerlume-package-", dir=output) as temporary:
        stage = Path(temporary) / name
        stage.mkdir()
        for source, destination, executable in files:
            target = stage / destination
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            target.chmod(0o755 if executable else 0o644)
        manifest = {
            "application": "PeerLume", "version": version, "commit": commit,
            "platform": platform, "target": TARGETS[platform], "profile": "release",
            "features": ["gui", "tui"], "rustc": rust_version,
            "built_at_utc": datetime.now(timezone.utc).isoformat(),
            "linux_baseline": None if windows else "Ubuntu 24.04 / glibc 2.39; X11 desktop",
            "signed": False,
        }
        (stage / "build-info.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
        members = sorted(path for path in stage.rglob("*") if path.is_file())
        (stage / "SHA256SUMS").write_text(
            "".join(f"{sha256(path)}  {path.relative_to(stage).as_posix()}\n" for path in members),
            encoding="utf-8", newline="\n",
        )
        members = sorted(path for path in stage.rglob("*") if path.is_file())
        with archive.open("xb") as stream:
            if windows:
                with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
                    for path in members:
                        bundle.write(path, path.relative_to(stage.parent).as_posix())
            else:
                with tarfile.open(fileobj=stream, mode="w:gz") as bundle:
                    for path in members:
                        info = bundle.gettarinfo(str(path), path.relative_to(stage.parent).as_posix())
                        info.uid = info.gid = 0
                        info.uname = info.gname = "root"
                        info.mode = 0o755 if path.relative_to(stage).parts[0] == "bin" else 0o644
                        with path.open("rb") as payload:
                            bundle.addfile(info, payload)
    with sidecar.open("x", encoding="utf-8", newline="\n") as stream:
        stream.write(f"{sha256(archive)}  {archive.name}\n")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=TARGETS, required=True)
    parser.add_argument("--binary-directory", required=True)
    parser.add_argument("--output-directory", default="dist/builds")
    parser.add_argument("--commit", required=True)
    parser.add_argument("--rust-version", required=True)
    args = parser.parse_args()
    try:
        print(package(Path(__file__).resolve().parent.parent, args.binary_directory,
                      args.output_directory, args.platform, args.commit, args.rust_version))
    except (ValueError, OSError, KeyError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
