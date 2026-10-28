"""Fail if the normal/build dependency graph of the TUI includes a GUI toolkit."""
import subprocess


def main():
    result = subprocess.run([
        "cargo", "tree", "--locked", "--no-default-features", "--features", "tui",
        "--edges", "normal,build", "--prefix", "none", "--format", "{p}",
    ], capture_output=True, text=True, check=True, timeout=120)
    packages = {line.split()[0] for line in result.stdout.splitlines() if line.strip()}
    forbidden = packages & {"eframe", "egui", "egui-winit", "winit", "rfd", "glutin", "glutin-winit", "x11-dl", "wayland-client"}
    if forbidden:
        raise SystemExit("Unexpected GUI dependencies: " + ", ".join(sorted(forbidden)))
    print("TUI normal/build graph has no GUI toolkit dependencies")


if __name__ == "__main__":
    main()
