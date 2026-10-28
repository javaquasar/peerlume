# PeerLume runtime extension

Baseline: crates.io `librqbit` 9.0.1, Apache-2.0, upstream
<https://github.com/ikatson/rqbit>. All original notices and source are retained.

Local changes expose runtime per-torrent rate limits and sequential ordering,
with shared settings surviving pause/resume. A session download admission cap
orders active unfinished torrents by priority then engine ID; already issued
requests can finish. Disabled cap preserves upstream admission. Sequential mode
sorts queued pieces by index; normal mode and streaming priorities are unchanged.
Small rate limits allow one 16 KiB peer chunk as a burst without changing their
per-second quota. Peer capacity remains configured through AddTorrentOptions;
PeerLume re-adds only the affected torrent when changing it.

Regression coverage lives in `src/piece_tracker.rs`, `src/limits.rs` and the root
crate's transfer/session/controller/TUI tests. This is not a new network soak.
