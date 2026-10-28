# Windows and Linux build packages

[Build packages](https://github.com/javaquasar/peerlume/actions/workflows/builds.yml)
creates two independent native release-profile packages:

| Platform | GitHub runner | Rust target | Download |
| --- | --- | --- | --- |
| Windows x64 | `windows-2025` | `x86_64-pc-windows-msvc` | ZIP |
| Linux x64 | `ubuntu-24.04` | `x86_64-unknown-linux-gnu` | TAR.GZ |

Each includes the GUI, TUI, CLI and MCP adapter, configuration examples,
documentation, build metadata and SHA-256 checksums. Linux also includes the
desktop launcher and application icon. See [downloaded-build instructions](distribution.md)
for requirements and startup; Linux is a GNU/glibc build, not a universal package.

## Trigger and download

The workflow runs on pushes to `main`/`master`, `v*` tags, manually selected refs,
and packaging-related pull requests. To build a branch manually, open
**Actions > Build packages > Run workflow**, choose the branch and confirm.
The workflow must first be present on the default branch for the manual button.

```sh
gh workflow run builds.yml --repo javaquasar/peerlume --ref main
gh run list --repo javaquasar/peerlume --workflow builds.yml
gh run download RUN_ID --repo javaquasar/peerlume --dir downloaded-builds
```

Open a successful run and use its platform-specific **Artifacts** or summary
download links. Each artifact contains the package archive and its adjacent
`.sha256` file. GitHub may wrap them in an additional download ZIP; extract that
outer ZIP first. Artifact names contain the full commit SHA and run attempt (so
reruns do not collide with an earlier immutable artifact), and archive names
contain the package version, platform and first 12 SHA characters. Retention is
30 days, subject to repository/organization policy. Download access follows the
repository's GitHub permissions; no public Release is created.

## Verification and security boundaries

Before uploading either package, its job runs packaging contract tests, application
and vendored tracker library tests with both frontends, and core integration tests.
It builds four named binaries with `cargo build --release --locked --all-features`.
Windows also verifies the embedded multi-resolution application icon.

The job then extracts the **archive**, verifies its exact allowlisted contents,
all internal/external SHA-256 hashes, source SHA, version and x64 PE/ELF headers.
It runs the extracted CLI's version command and initializes the extracted MCP
adapter offline using a disposable profile. Linux checks `ldd` for every binary.
These smoke checks do not launch a second GUI, connect to trackers, enable agent
access or use real torrent data. Native interactive GUI coverage remains in the
separate Reliability workflow. A successful package is not proof that Tests,
Coverage or all long-duration release gates have passed; review those runs too.

Inputs are explicitly allowlisted. The packager does not collect `downloads/`,
profiles, SQLite databases, logs, secret stores or arbitrary extra target files.
Existing archive/checksum paths are never overwritten. Actions are SHA-pinned,
checkout credentials are not persisted, and workflow permissions are read-only.
No signing keys, release token or user secrets are needed. There is no auto-update,
installer, signature, SBOM or automatic GitHub Release publication in this workflow.
Stable Rust may advance between runs; `Cargo.lock` fixes dependencies, while the
exact compiler is recorded. This is not a byte-for-byte reproducible-build claim.

## Local packaging

Python 3.11+ is required. Build on the matching native OS; use PowerShell 7 for
the Windows icon check. For example, on Windows:

```powershell
cargo build --release --locked --all-features --target x86_64-pc-windows-msvc --bin peerlume --bin peerlume-tui --bin peerlume-control --bin peerlume-mcp
./scripts/check-app-icon.ps1 -BinaryPath target/x86_64-pc-windows-msvc/release/peerlume.exe
$commit = git rev-parse HEAD
python scripts/package_build.py --platform windows-x64 --binary-directory target/x86_64-pc-windows-msvc/release --commit $commit --rust-version (rustc --version)
python scripts/verify_build.py --platform windows-x64 --commit $commit
```

On Linux, use target `x86_64-unknown-linux-gnu`, platform `linux-x64`, and its
corresponding `target/<target>/release` directory. Supply the full lowercase Git
SHA. `--output-directory` can select a fresh directory **inside the repository**;
use the same directory with verifier `--directory`. Do not label uncommitted local
code as a clean published commit. The existing `package-windows.ps1` development
ZIP used by Tests is retained independently for backwards compatibility.
