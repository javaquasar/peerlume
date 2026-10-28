# Running a downloaded PeerLume build

The workflow produces portable archives and separate native installers for **x64**. Windows uses
MSVC executables; Linux uses GNU/glibc executables built on Ubuntu 24.04.
They are unsigned development/release-profile builds, not a declaration that
every release gate has passed. No torrent data, credentials or user profile is included.

## Windows

Use Windows 10 or newer on x64. Extract the ZIP into a writable folder and run `peerlume.exe` for the GUI,
or `peerlume-tui.exe` from a terminal for the TUI. Keep the extracted files together.
Windows may warn about unsigned applications. Check the commit and checksum;
the builds and installers are not code-signed.
The build uses the default MSVC C-runtime linkage; if Windows reports a missing
Visual C++ runtime DLL, install Microsoft's current x64 Visual C++ Redistributable.
The archive does not install or bundle a redistributable automatically.

## Linux

Use a GNU/glibc x64 desktop compatible with Ubuntu 24.04 (glibc 2.39). This is
**not** an Alpine/musl, ARM, AppImage, Flatpak or RPM package. Compatibility
with older distributions is not guaranteed. An X11 session (or XWayland), OpenGL,
EGL and XKB/X11 runtime libraries are needed for the GUI. Private tracker credentials
use the desktop Secret Service (for example GNOME Keyring); DBus and a running,
unlocked keyring are needed for persistent credentials.

On Ubuntu 24.04, install runtime dependencies if they are not already provided:

```sh
sudo apt-get install libgl1 libegl1 libx11-6 libxi6 libxrandr2 libxcb1 libxkbcommon0 libxkbcommon-x11-0 xdg-desktop-portal xdg-desktop-portal-gtk gnome-keyring
```

Extract the TAR.GZ and run from its directory:

```sh
./bin/peerlume
# Terminal frontend instead:
./bin/peerlume-tui
```

The archive preserves executable permissions. Optional launcher and icon files
are under `share/`. To install them manually, put the extracted `bin` directory
on your desktop session's PATH and copy `share/applications/peerlume.desktop` and
`share/icons/hicolor/256x256/apps/peerlume.png` to the corresponding user data
directories. No system files or file associations are changed automatically.

## Configuration and adapters

The same workflow also produces separate `tui` artifacts for both operating
systems, built with `--no-default-features --features tui`. They omit GUI binaries
and desktop assets. Linux headless packages need no display server; Secret
Service requirements for persistent private credentials still apply. See
[terminal controls and server setup](tui.md).

Both frontends use the normal per-user application profile. "Archive build"
does not mean profile data is saved next to the executable. Before changing an
existing installation, close the old client; do not run two engines on one profile.
To use a separate profile, pass `--profile` with an absolute directory path.

`config/` contains **examples**, not a copy of your settings. The CLI and MCP
adapters are `peerlume-control` and `peerlume-mcp` (`.exe` on Windows).
See `docs/agent-control.md` and `docs/runtime-controls.md` in the extracted package
for opt-in access and commands. Adapters never enable agent access
automatically.

`build-info.json` identifies the version, source commit, Rust compiler, target,
features and build time. `SHA256SUMS` covers every other packaged file.
The adjacent `.sha256` file verifies the complete archive before extraction.
Checksums detect corruption; they are not a publisher signature.

Linux archive checksum verification:

```sh
sha256sum -c peerlume-*.tar.gz.sha256
```

On Windows, compare `(Get-FileHash ./peerlume-....zip -Algorithm SHA256).Hash`
with the first field of the matching `.zip.sha256` file.

## Native installers

The `peerlume-native-*` artifacts contain a Windows `*-setup.exe` or a Linux
`.deb`, plus its SHA-256 sidecar. Both are built from the verified archive.
Desktop archives contain GUI, TUI, CLI, MCP and daemon; headless archives omit GUI.

Windows Setup uses Inno Setup 6 in per-user mode (no administrator prompt).
Desktop and headless flavors use separate installation directories. A desktop
shortcut is optional. Setup does not launch the app, force-close an existing
client, install a service or enable autostart. Close the old client before an
upgrade. Uninstall removes program files, never the profile or downloaded data.

The amd64 Debian package targets Ubuntu 24.04 or compatible newer GNU/glibc
systems. Install with `sudo apt install ./peerlume-....deb` so dependencies are
resolved. Desktop and headless packages conflict intentionally because their
shared executables occupy the same `/usr/bin` paths. The package installs a
**user** systemd unit, but has no installation scripts and does not enable or
start it automatically. Test with `peerlume-daemon --check-config` before use.
The package workflow also installs, replaces and uninstalls the native package:
Windows uses a unique installation directory on its disposable runner; Linux uses
a disposable Ubuntu 24.04 container. Synthetic profile/payload sentinels must
survive each stage, and installed console binaries must answer offline version
requests. The tests do not launch the user GUI or enable a service.
They also abruptly terminate the installed daemon on a new, empty synthetic
profile, restart that same profile and check committed SQLite state, integrity,
profile-lock recovery, absence of a false clean-resume marker and payload
preservation. No production tracker or real swarm is used.

These are same-version replacement tests, not proof of every historical schema
upgrade. Windows signing, the real Linux service login/keyring environment and
other distribution compatibility remain separate gates.

The repository pins Rust 1.93.0 for local and CI builds. The separately instrumented
fuzz campaign keeps its explicitly pinned nightly toolchain. The Tests workflow
also exercises genuine ENOSPC on a disposable 2 MiB tmpfs and storage regressions
for access denial, cancellation and interrupted move recovery.
