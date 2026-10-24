# Continuous session soak

This experiment keeps one real PeerLume transfer engine, one controller/IPC owner,
one listener and two fixed torrents alive throughout the requested workload.
All tracker and peer traffic uses synthetic private torrents on loopback. It
does not contact production trackers or use personal credentials.

## Workload

The primary torrent starts without payload files and downloads 1 MiB from the
fixture peer. Every 32 KiB piece is hashed by the real backend. The control
torrent starts with 64 KiB of verified data and seeds continuously over one TCP
connection. That connection is opened exactly once: a disconnect fails the test
instead of reconnecting and hiding an engine restart.

Each cycle downloads the primary torrent from the application into a validating
fixture receiver, corrupts one or two rotating pieces on disk, and requests an
ordinary recheck over authenticated IPC. Automatic repair must enter Downloading,
Checking and Succeeded, restore the exact original payload, and fetch only the
damaged pieces. The control torrent must continue uploading during the repair.
Cycles last at least ten seconds; the permanent receiver remains active between
cycles. The same content hashes, engine object, listener, session peer identity
and selected reporting mode must survive every cycle. Actual network counters
must account for the bytes verified by the fixture peers.

PeerLume's existing recheck implementation stops and re-adds the affected torrent
inside the same engine session. This test permits that torrent-state transition;
it does not permit recreating the engine or replacing the permanent control
connection. It is not a claim of one unbroken connection to every peer, constant
bandwidth, or in-place hashing without a torrent-state restart.

## GitHub execution

Reliability CI runs 90-second smoke experiments in both `normal` and `no_progress`
reporting modes. The Coverage workflow instruments 60-second experiments in both
modes in addition to ordinary regressions. The no-progress variant must preserve
zero sent upload/download counters, full-size `left`, and no `completed` announce
while actual download, upload and repair still take place.

Both HTTP and UDP tracker workers use the same lifecycle rules. A new full
download reports `completed` once per tracker; pre-existing complete files,
partial selections and subsequent repair/recheck generations do not create a
new completion. Pause, removal, mode replacement, recheck and orderly application
shutdown flush real `stopped` requests before the engine cancels its peer stream.
An earned completion is flushed first if it has not yet been acknowledged.
Reporting modes still transform these requests; `no_progress` and a frozen
nonzero `left` suppress `completed`.

Lifecycle requests do not wait for the tracker's regular announce interval or
for peer-stream backpressure. Stop flushing is best-effort: concurrent trackers
share a five-second deadline, with at most two seconds per final request.
Unreachable trackers can therefore leave failed/cancelled attempts rather than
successful `stopped` records; abrupt process termination cannot guarantee delivery.
Journal records always describe actual attempts and responses, not synthetic
UI-generated completion or stop entries.

Start one four-hour run of the manual Continuous session soak workflow:

```powershell
gh workflow run continuous-session-soak.yml --repo javaquasar/peerlume --ref main -f seconds=14400 -f mode=normal
```

The runner timeout is five hours, including compilation. New pushes do not cancel
an experiment already in progress. A replacement experiment must be dispatched
explicitly; failed runs are never silently combined to make up four hours.

## Evidence and acceptance

The `continuous-session-soak` artifact contains `resources.jsonl` and a complete
`announces.jsonl` export. The resource report includes:

- one-second RSS, thread, handle, SQLite-file and announce-journal measurements;
- independently verified wire download and upload counters, source connection
  counts, the permanent connection count and the current workload phase;
- initial-download evidence and per-cycle damaged-piece indices, fetched bytes,
  repair transitions, retained identities, reporting mode and controller snapshots;
- a final result only after successful workload completion, fixture shutdown,
  engine shutdown and journal export, including completion/stop counts.

The complete exported journal must contain exactly one successful main-torrent
`completed` in normal mode (zero in no-progress mode), no completion for the
pre-existing control seed, and a final successful `stopped` for each torrent.
The check reads the full export rather than the bounded live UI journal.

After three warmup cycles, idle and sampled-peak growth budgets are 192 MiB RSS,
32 threads and 64 handles. SQLite files are bounded to 32 MiB; journal and resource
evidence are each bounded to 128 MiB. These are failure thresholds, not targets or
proof that no smaller leak exists. One-second sampling can miss shorter peaks.
IPC requests must finish within five seconds.

Acceptance requires a successful GitHub job and exactly one final result with
`completed=true`, `workload=continuous_session`, the full requested duration,
positive download/upload/repair evidence, `session_starts=1`, unchanged engine
identity, `permanent_seeding_connections=1` and `clean_shutdown=true`. A partial
JSONL file or a passed smoke run does not establish a completed four-hour run.
This experiment does not close the separate 24-hour release gate.
