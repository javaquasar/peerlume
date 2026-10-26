# PeerLume application icon

The original mark combines a P-like silhouette, connected peer nodes and light.
It was generated with the built-in image generation tool (not the API/CLI) and
has genuine transparency. The approved master is `peerlume-master.png`.

`scripts/generate-icons.ps1` performs deterministic packaging only: it resizes
the master to transparent PNGs at 16, 24, 32, 48, 64, 128, 256 and 512 pixels,
and packs the first seven images into `peerlume.ico`. Run it with PowerShell 7
on Windows to regenerate these assets; normal Cargo builds use the committed
files and do not require an image generation service or image-editing tool.

The native window embeds `peerlume-256.png`. Windows GUI-enabled builds also
embed the ICO through `build.rs`; a resource compiler from the Windows SDK
(MSVC) or MinGW is required. The Windows packaging check verifies every embedded
PNG against the approved artwork without executing the GUI.

## Linux desktop integration

The X11 window receives the same PNG icon. The application ID is `peerlume`,
matching `assets/linux/peerlume.desktop`. To install the launcher for your user,
ensure the `peerlume` executable is on your desktop session's PATH, then run
from the repository root:

```sh
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm644 assets/linux/peerlume.desktop "$data_dir/applications/peerlume.desktop"
for size in 16 24 32 48 64 128 256 512; do
    install -Dm644 "assets/icons/peerlume-$size.png" "$data_dir/icons/hicolor/${size}x${size}/apps/peerlume.png"
done
```

Desktop shell icon caches may need a refresh or a new login. Windows Explorer
may also cache icons for an existing executable path. Neither installing a new
icon nor rebuilding changes the currently running application's window icon;
use the new build at the next normal restart.

## Final generation prompt

Use case: logo-brand. Asset type: production desktop application icon for
PeerLume, a peer-to-peer torrent client. Create ONE original icon, not a sheet
of alternatives or a mockup. A simple bold compact emblem suggesting connected
peers and light, with a subtle P-like silhouette, visually harmonizing with a
desktop application whose existing accent is light blue. Strong memorable
silhouette, balanced negative space, thick forms that remain legible at 16x16
and 32x32. Restrained blue/cyan accents with a dark neutral supporting shape
for contrast on light and dark desktops. Flat vector-like precision rendered
as a polished high-resolution raster icon, minimal detail. Centered square
composition, approximately 10 percent clear margin. Actual transparent
background outside the emblem, preserve alpha. No written words, no labels,
no small lettering, no gradients that rely on a background, no scenery,
no illustrative objects, no drop shadow outside the emblem, no watermark,
no imitation of existing torrent client logos.
