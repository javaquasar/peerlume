"""Archive contract tests; synthetic executable headers are never run."""
import hashlib
import io
import json
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
import zipfile

import package_build as packaging
import verify_build as verifier


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="peerlume-package-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text('[package]\nversion = "0.1.0"\n', encoding="utf-8")
        for relative in [*packaging.DOCUMENTS, "assets/linux/peerlume.desktop", "assets/icons/peerlume-256.png"]:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fixture documentation or icon")
        (self.root / "profile").mkdir()
        (self.root / "profile/secrets.sqlite").write_bytes(b"must not be shipped")
        self.binaries = self.root / "target/release"
        self.binaries.mkdir(parents=True)

    def fixtures(self, platform):
        header = bytearray(128)
        if platform == "windows-x64":
            header[:2] = b"MZ"
            struct.pack_into("<I", header, 60, 64)
            header[64:70] = b"PE\0\0\x64\x86"
        else:
            header[:6] = b"\x7fELF\x02\x01"
            struct.pack_into("<H", header, 18, 62)
        for name in packaging.BINARIES:
            (self.binaries / (name + (".exe" if platform == "windows-x64" else ""))).write_bytes(header)
        (self.binaries / "private-debug-log.txt").write_bytes(b"must not be shipped")

    def build(self, platform="windows-x64", **overrides):
        args = dict(root=self.root, binary_directory="target/release", output_directory="dist/builds",
                    platform=platform, commit="a" * 40, rust_version="rustc fixture")
        args.update(overrides)
        return packaging.package(**args)

    def verify(self, platform):
        self.fixtures(platform)
        archive = self.build(platform)
        outer = Path(str(archive) + ".sha256").read_text(encoding="utf-8")
        self.assertEqual(outer, f"{packaging.sha256(archive)}  {archive.name}\n")
        if platform == "windows-x64":
            with zipfile.ZipFile(archive) as bundle:
                payloads = {name: bundle.read(name) for name in bundle.namelist()}
        else:
            with tarfile.open(archive) as bundle:
                payloads = {member.name: bundle.extractfile(member).read() for member in bundle.getmembers()}
                for member in bundle.getmembers():
                    self.assertTrue(member.isfile())
                    self.assertEqual(member.mode, 0o755 if "/bin/" in member.name else 0o644)
        roots = {Path(name).parts[0] for name in payloads}
        self.assertEqual(len(roots), 1)
        prefix = roots.pop() + "/"
        payloads = {name.removeprefix(prefix): data for name, data in payloads.items()}
        self.assertFalse(any("secret" in name or "private-debug" in name for name in payloads))
        manifest = json.loads(payloads["build-info.json"])
        self.assertEqual(manifest["commit"], "a" * 40)
        self.assertEqual(manifest["target"], packaging.TARGETS[platform])
        self.assertEqual(manifest["profile"], "release")
        entries = payloads["SHA256SUMS"].decode().splitlines()
        self.assertEqual(len(entries), len(payloads) - 1)
        for line in entries:
            digest, name = line.split("  ", 1)
            self.assertEqual(digest, hashlib.sha256(payloads[name]).hexdigest())
        self.assertEqual(len(list((self.root / "dist/builds").iterdir())), 2)
        destination = self.root / "extracted"
        destination.mkdir(exist_ok=True)
        extracted = verifier.extract_and_verify(archive, platform, "a" * 40, "0.1.0", destination)
        self.assertEqual(json.loads((extracted / "build-info.json").read_text())["platform"], platform)
        return archive

    def test_windows_zip_has_all_four_executables_and_verified_checksums(self):
        self.verify("windows-x64")

    def test_linux_tar_preserves_executable_modes_and_desktop_assets(self):
        archive = self.verify("linux-x64")
        with tarfile.open(archive) as bundle:
            names = bundle.getnames()
        self.assertTrue(any(name.endswith("share/applications/peerlume.desktop") for name in names))
        for binary in packaging.BINARIES:
            self.assertTrue(any(name.endswith("bin/" + binary) for name in names))

    def test_missing_and_wrong_architecture_binaries_fail_before_packaging(self):
        with self.assertRaises(ValueError):
            self.build()
        self.fixtures("windows-x64")
        path = self.binaries / "peerlume-tui.exe"
        value = bytearray(path.read_bytes())
        value[68:70] = b"\x4c\x01"  # x86 instead of x64
        path.write_bytes(value)
        with self.assertRaises(ValueError):
            self.build()
        self.assertFalse((self.root / "dist").exists())

    def test_headless_packages_need_no_gui_binary_or_desktop_assets(self):
        for platform in packaging.TARGETS:
            with self.subTest(platform=platform):
                self.fixtures(platform)
                (self.binaries / ("peerlume.exe" if platform == "windows-x64" else "peerlume")).unlink()
                archive = self.build(platform, flavor="tui", output_directory="dist/" + platform)
                destination = self.root / ("extracted-" + platform)
                extracted = verifier.extract_and_verify(archive, platform, "a" * 40, "0.1.0", destination, "tui")
                manifest = json.loads((extracted / "build-info.json").read_text())
                self.assertEqual(manifest["features"], ["tui"])
                names = [path.relative_to(extracted).as_posix() for path in extracted.rglob("*") if path.is_file()]
                self.assertFalse(any(name.startswith("share/") or name in ("peerlume.exe", "bin/peerlume") for name in names))
                with self.assertRaises(ValueError):
                    verifier.extract_and_verify(archive, platform, "a" * 40, "0.1.0", self.root / "wrong-flavor")

    def test_unknown_flavor_is_rejected(self):
        with self.assertRaises(ValueError):
            self.build(flavor="typo")

    def test_existing_package_is_never_overwritten(self):
        archive = self.verify("windows-x64")
        original = archive.read_bytes()
        with self.assertRaises(ValueError):
            self.build()
        self.assertEqual(archive.read_bytes(), original)

    def test_unsafe_output_and_invalid_metadata_are_rejected(self):
        for overrides in [dict(output_directory="../outside"), dict(output_directory="."),
                          dict(binary_directory="../outside"), dict(commit="main"),
                          dict(platform="linux-arm64")]:
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.build(**overrides)
        (self.root / "Cargo.toml").write_text('[package]\nversion = "../unsafe"\n', encoding="utf-8")
        with self.assertRaises(ValueError):
            self.build()

    def rewrite_zip(self, archive, mutate):
        with zipfile.ZipFile(archive) as bundle:
            payloads = {name: bundle.read(name) for name in bundle.namelist()}
        mutate(payloads)
        with zipfile.ZipFile(archive, "w") as bundle:
            for name, payload in payloads.items():
                bundle.writestr(name, payload)
        Path(str(archive) + ".sha256").write_text(
            f"{packaging.sha256(archive)}  {archive.name}\n", encoding="utf-8")

    def verify_modified(self, archive):
        return verifier.extract_and_verify(archive, "windows-x64", "a" * 40, "0.1.0", self.root / "modified")

    def test_verifier_rejects_outer_corruption_and_wrong_commit(self):
        self.fixtures("windows-x64")
        archive = self.build()
        with self.assertRaisesRegex(ValueError, "Archive name"):
            verifier.extract_and_verify(archive, "windows-x64", "b" * 40, "0.1.0", self.root / "wrong")
        with archive.open("ab") as stream:
            stream.write(b"corruption")
        with self.assertRaisesRegex(ValueError, "Archive SHA-256"):
            self.verify_modified(archive)

    def test_verifier_rejects_unlisted_files_and_path_traversal_before_extraction(self):
        self.fixtures("windows-x64")
        archive = self.build()
        self.rewrite_zip(archive, lambda payloads: payloads.update({"../escape.txt": b"not allowed"}))
        with self.assertRaisesRegex(ValueError, "archive entries"):
            self.verify_modified(archive)
        self.assertFalse((self.root / "modified").exists())

    def test_verifier_checks_inner_hashes_even_when_outer_checksum_is_updated(self):
        self.fixtures("windows-x64")
        archive = self.build()
        def corrupt(payloads):
            key = next(name for name in payloads if name.endswith("/peerlume.exe"))
            payloads[key] += b"changed binary"
        self.rewrite_zip(archive, corrupt)
        with self.assertRaisesRegex(ValueError, "Packaged file SHA-256"):
            self.verify_modified(archive)

    def test_verifier_checks_manifest_even_when_all_hashes_match(self):
        self.fixtures("windows-x64")
        archive = self.build()
        def corrupt(payloads):
            key = next(name for name in payloads if name.endswith("/build-info.json"))
            info = json.loads(payloads[key])
            info["commit"] = "b" * 40
            payloads[key] = json.dumps(info).encode()
            prefix = key.removesuffix("build-info.json")
            payloads[prefix + "SHA256SUMS"] = "".join(
                f"{hashlib.sha256(data).hexdigest()}  {name.removeprefix(prefix)}\n"
                for name, data in sorted(payloads.items()) if not name.endswith("/SHA256SUMS")
            ).encode()
        self.rewrite_zip(archive, corrupt)
        with self.assertRaisesRegex(ValueError, "manifest mismatch: commit"):
            self.verify_modified(archive)

    def test_verifier_rejects_linux_links_and_lost_execute_permissions(self):
        self.fixtures("linux-x64")
        for kind in ["link", "permissions"]:
            with self.subTest(kind=kind):
                archive = self.build("linux-x64", output_directory=f"dist/{kind}")
                with tarfile.open(archive) as bundle:
                    members = [(member, bundle.extractfile(member).read()) for member in bundle.getmembers()]
                with tarfile.open(archive, "w:gz") as bundle:
                    for member, payload in members:
                        if member.name.endswith("/bin/peerlume"):
                            if kind == "link":
                                member.type = tarfile.SYMTYPE
                                member.linkname = "../../outside"
                                member.size = 0
                            else:
                                member.mode = 0o644
                        bundle.addfile(member, io.BytesIO(payload))
                Path(str(archive) + ".sha256").write_text(
                    f"{packaging.sha256(archive)}  {archive.name}\n", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "non-file or incorrect permissions"):
                    verifier.extract_and_verify(archive, "linux-x64", "a" * 40, "0.1.0", self.root / kind)


if __name__ == "__main__":
    unittest.main()
