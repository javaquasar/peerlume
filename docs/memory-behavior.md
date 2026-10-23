# Memory bounds and journal rendering

This change bounds retained UI telemetry and journal data, without reducing peer
transfer concurrency or altering download/reporting modes. It is not a global
process-memory limit and does not claim a measured reduction in working set.

## Live announce journal

The transfer session retains at most **1024 entries** and **8 MiB of accounted
entry data**, whichever limit is reached first. Accounting includes entry structs,
string/vector capacities, peer vectors, cached summaries and reference bookkeeping.
Allocator overhead, spare deque capacity, GUI/GPU allocations and temporary reader
snapshots are not included; the limit is not an RSS/private-bytes guarantee.

The full accepted observation is serialized and flushed to the existing JSONL log
before live-tail eviction. An individual observation larger than the live budget
is omitted from the live tail, with a visible counter, but remains in JSONL when
disk logging succeeds. Disk failures remain visible and exports report incomplete
logging instead of silently claiming a complete history. Oversized-record counts
alone are not proof of successful disk persistence.

Snapshots use `Arc<JournalEntry>`: readers share immutable data rather than copying
every peer address on each repaint. Human-readable summaries are cached once.
The JSONL schema remains unchanged; cache fields are not serialized.

Both frontends show **32-entry pages**, newest first. egui has Newer/Older buttons
and virtualized peer lists inside expanded entries. ratatui uses PgUp/PgDn for pages
and up/down for logical lines; only viewport lines are formatted, even when a reply
contains 100,000 peers. Export still streams the entire disk history, not a page.
Pages are offsets in the moving live tail, not pinned historical snapshots.

## Session-event transport

Periodic file-progress vectors, network counters and check-progress updates use
one latest snapshot of each kind per torrent. A slow frontend cannot accumulate
256 old copies of the same torrent's file-progress vector. Replacing a snapshot
drops the old vector; intermediate telemetry samples are intentionally coalesced.
The actual counters remain cumulative and tracker scheduling is unchanged.

Command acknowledgements, verification results, errors and lifecycle transitions
use a separate bounded FIFO, with backpressure instead of dropped events. Repeated
identical pending engine errors are deduplicated. Sequence stamps preserve causal
order across retained telemetry and controls, so stale progress cannot follow a
newer check result merely because it travelled on another queue.

The command/control capacity remains 256 for the normal application. When the UI
stops polling and fills the control FIFO, the session controller waits for space;
it does not allocate an unbounded acknowledgement backlog. Existing peer tasks
remain separate. Shutdown closes and clears the receiver before joining the
controller, releasing blocked publishers even without a polling frontend.
Removing a torrent also drops its pending telemetry.

## Standalone audit history

egui and ratatui use the same structured audit exchange and bounded history:
**256 live entries / 2 MiB of accounted entry data per history**. Full exchanges,
including peer lists, are appended to `audit-logs/audit-*.jsonl` under the existing
application data directory. The path and any disk error are shown in the history
status. Clearing the live display does not delete historical files.

The egui audit worker has a four-result bounded channel. ratatui no longer builds
one growing string containing every peer from every scenario step; its completion
message contains at most eight compact exchange summaries, with at most eight peers
each. Complete accepted peer lists remain on disk. These audit files are separate
from real transfer-announce logs and must not be interpreted as download traffic.

## HTTP response safety

Both the real tracker transport and the standalone HTTP audit reader reject bodies
larger than **1 MiB**, before parsing bencode. Limits are enforced while reading,
not only against Content-Length; missing length and chunked responses are covered.
Oversized replies are errors, not successful empty-peer replies. All peers from
accepted replies are retained in the full disk observation; no silent truncation
of accepted tracker responses is introduced. Large legitimate tracker replies
will require an explicitly reviewed limit change.

## Regression tests and remaining work

Tests cover shared allocations and cached summaries, page bounds, byte-budget
eviction with complete JSONL export, oversized live entries, disk failures, 100,000
file telemetry with a slow consumer, control ordering/backpressure, removal and
shutdown without a polling UI. A 100,000-peer fixture verifies viewport-only TUI
formatting. HTTP tests cover advertised, unframed, chunked and exact-limit bodies.
Existing concurrent-download, verification, recovery and frontend tests remain.

These are deterministic retention tests, not a multi-hour heap profile. A future
profiling pass should compare release builds on identical 1/10/100-torrent workloads,
record private bytes/working set and allocation peaks, and exercise pause/remove/
recheck cycles. Global peer/cache budgets and lazy loading of inactive torrent
models are separate work; the custom BlockBufferPool is not the active librqbit
transfer allocator.
