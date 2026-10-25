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

## Realistic swarm and fault-recovery extension

The separate `Realistic swarm soak` workflow retains the baseline experiment for
comparison. Its one engine/controller/IPC owner manages three fixed private
torrents: two initial parallel downloads and a pre-existing control seed. The
`large` profile downloads a 2 GiB + 256 KiB single file and a 2 GiB, three-file
torrent. File boundaries intentionally cross piece boundaries. The `smoke`
profile uses the same code with approximately 16 MiB downloads. Both profiles
keep only one 16 KiB deterministic template per torrent, a piece-sized hashing
buffer and bounded connection pools; fixture memory is not proportional to the
download size. All traffic remains on loopback.

Two complementary peers must contribute to each initial download. A barrier
requires both torrents to request data before the fixture releases any response,
proving overlapping downloads rather than sequential completion. Tracker replies
rotate between two groups of four source endpoints, with redundant complementary
piece sets so a deliberate disconnection leaves a healthy alternative. Each cycle reconnects validating
receivers, exercises the large file tail, disconnects old source generations,
truncates a real peer frame and repairs a damaged piece in each torrent. The first
multi-file repair damages bytes on both sides of a file boundary within one piece.
Independent streaming byte comparison verifies every file after download/repair;
healthy pieces must not be fetched again. The control seed retains one permanent
TCP connection while this happens.

Cycles rotate HTTP 503, dropped tracker replies and silent-response timeouts.
Each injected failure must appear in the real announce journal and be followed by
a successful reply before continuing. HTTP announces now have a ten-second
request/body deadline, matching the bounded UDP request timeout. Additional
HTTP/UDP regressions cover lost completion acknowledgements, timeout recovery,
and truthful shutdown failures. A lost acknowledgement may make the tracker
receive the same `completed` request twice; only a successful reply settles the
client's lifecycle state. The journal must not invent tracker acceptance.

Reliability CI runs the 180-second smoke profile; Coverage instruments 120 seconds
and the same source. Each run requires at least three complete cycles, including
all three tracker faults, even if setup/repair extends beyond the requested
minimum duration. Before a four-hour run, execute a short `large` gate:

```powershell
gh workflow run realistic-swarm-soak.yml --repo javaquasar/peerlume --ref main -f seconds=180 -f profile=large
gh workflow run realistic-swarm-soak.yml --repo javaquasar/peerlume --ref main -f seconds=14400 -f profile=large
```

Do not launch the second command until CI and the first run's complete evidence
have passed review. A long run is never silently replaced after failure. The
workflow requires 8 GiB free disk for the large profile and permits five hours
including compilation and bounded final-cycle completion.

The `realistic-swarm-soak` artifact contains `resources.jsonl` and `announces.jsonl`.
The final result requires one unchanged engine, three fixed identities, concurrent
verified initial downloads, positive validated uploads/refetches, peer churn and
truncation evidence, all fault/recovery types, exactly one successful completion
per newly downloaded torrent, no completion for the control seed, and final real
successful stopped replies. Only the explicitly recorded injected failures may
be non-successful journal entries. Actual counters remain cumulative and accurate.
Source-written bytes can exceed client-received bytes when connections/frames
are interrupted; compare network totals against the independently verified payload
floor, not against every byte written by a fixture socket. Existing RSS/thread/
handle growth and SQLite/journal/report budgets remain unchanged. Neither this
loopback test nor its four-hour duration closes the 24-hour release gate or proves
the absence of every memory leak.
