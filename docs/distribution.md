# Running a downloaded PeerLume build

These archives contain native **x64** binaries, not an installer. Windows uses
MSVC executables; Linux uses GNU/glibc executables built on Ubuntu 24.04.
They are unsigned development/release-profile builds, not a declaration that
every release gate has passed. No torrent data, credentials or user profile is included.

## Windows

Use Windows 10 or newer on x64. Extract the ZIP into a writable folder and run `peerlume.exe` for the GUI,
or `peerlume-tui.exe` from a terminal for the TUI. Keep the extracted files together.
Windows may warn about unsigned applications. Check the commit and checksum;
this package does not contain a code-signing certificate or an installer.
The build uses the default MSVC C-runtime linkage; if Windows reports a missing
Visual C++ runtime DLL, install Microsoft's current x64 Visual C++ Redistributable.
The archive does not install or bundle a redistributable automatically.

## Linux

Use a GNU/glibc x64 desktop compatible with Ubuntu 24.04 (glibc 2.39). This is
**not** an Alpine/musl, ARM, AppImage, Flatpak, DEB or RPM package. Compatibility
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
