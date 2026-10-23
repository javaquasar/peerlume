# BitTorrent tracker accounting audit scenarios

## Purpose

This document describes controlled tests that help tracker operators detect
clients reporting inconsistent `uploaded`, `downloaded`, and `left` values,
violating the announce lifecycle, or sharing credentials.

These tests are intended for a local or staging tracker controlled by the test
operator. PeerLume defaults to normal downloading with accurate reporting.
For testing, explicitly select one of the download-time metric transformation
modes described below when adding a torrent.

The basic HTTP announce fields are defined in
[BEP 3](https://www.bittorrent.org/beps/bep_0003.html); the binary UDP announce
format is defined in [BEP 15](https://www.bittorrent.org/beps/bep_0015.html).

## Important limitation

A tracker sees only information reported by the client. The protocol provides
no cryptographic proof of the bytes actually transferred. A client can report
internally consistent but false values. Consequently, a tracker can identify
contradictions and anomalies, but cannot prove the exact transfer volume from
announces alone.

The recommended response is to accumulate a risk score, observe further, and
allow manual review. An immediate ban based on one heuristic creates a high
risk of false positives.

## Safe test environment

1. Use a dedicated passkey and test torrent.
2. Restrict the audit harness to `localhost`, a private laboratory network, or
   an explicitly configured staging hostname.
3. Record a test-run identifier in a separate field or log.
4. Keep test events separate from production user statistics.
5. Limit the number of announces and the maximum test duration.
6. Remove or label test statistics afterward.
7. Do not use real users' passkeys, IP addresses, or Peer IDs.

## Running scenarios in the desktop UI

When adding a `.torrent`, the isolated audit panel appears below the torrent
information. It can send one controlled announce before the torrent is added.

After adding a torrent, select it and open **Trackers**. Each torrent has its
own ordered scenario. Every step specifies:

- a delay relative to the preceding step;
- `uploaded`, `downloaded`, and `left`;
- an announce event: none, `started`, `completed`, or `stopped`.

Send an individual step or run the entire scenario, with cancellation available
during waits. For each executed step, the log records request fields
(`info_hash`, Peer ID, port, event, and all three counters) and the tracker
response (interval, compact IPv4 peer addresses, or an error).

Scenario steps, the allowed staging hostname, port, and single Tracker URL are
saved separately for each torrent. On Windows, the exact URL is protected with
current-user DPAPI; ordinary SQLite fields and logs contain only a redacted
representation without the passkey. After restart, the protected URL is restored
and used by both the audit runner and normal transfer session. Only loopback
addresses are allowed by default; an external tracker requires an exact match
with **Allowed staging host**.

### Long-running started-only observation

See [started-only-observation.md](started-only-observation.md) for the full mode
description, lifecycle, and result collection procedure.

For a multi-hour test, enable **Started-only observation** in the selected
torrent's audit profile and choose a duration from 60 seconds to seven days.
Selecting this option in the add dialog disables normal automatic startup,
adds the torrent paused, and sends the single audit announce automatically
after **Add torrent** is pressed. It sends exactly:

```text
event=started
uploaded=0
downloaded=0
left=<full torrent size in bytes>
```

After the response, the application remains in observation until the deadline
or **Stop**. The audit runner sends no periodic, `completed`, or `stopped`
announces during that time. Stopping observation also sends no additional
tracker request. The exact outgoing fields and initial response, including
interval and peers, remain in the `sent -> received` log.

This mode belongs to the isolated staging audit and does not change normal
transfer-session reporting. Audit peers are not forwarded to the download
engine. The mode configuration and single Tracker URL are saved separately
for each torrent. On Windows, credential-bearing URLs are stored only as
DPAPI-protected values and restored after restart.

Because the client deliberately sends nothing after the first announce, it
cannot retrieve subsequent tracker reactions through the same protocol.
For multi-hour statistics, correlate the PeerLume log with server logs, peer
TTL expiry, the test account/passkey state, and internal anomaly/risk-score
events. Keep PeerLume running until observation finishes or is stopped manually.

## Scenarios that expose tracker weaknesses

The most informative checks require the tracker to compare an announce with
previous session state, rather than simply accept the submitted counters.

| Scenario | Sequence | Potential weakness |
| --- | --- | --- |
| False upload | `started: uploaded=0`, then `uploaded=10 GiB` a few seconds later | The tracker credits the entire claim without checking plausible transfer speed |
| Completion without downloading | `started: downloaded=0,left=size`, then `completed: downloaded=0,left=0` | Download volume or completion is credited without consistent growth in `downloaded` |
| `completed` with `left>0` | Send `completed` while keeping a nonzero `left` | The completion count increases or the torrent is marked complete |
| Repeated `completed` | Send the same `completed` multiple times | Every repetition changes accounting again |
| Counter rollback | Decrease `uploaded` or `downloaded`, or increase `left` | The tracker accepts and stores a rollback without flagging an anomaly |
| Inconsistent progress | Decrease `left` while keeping `downloaded` unchanged | No contradiction is recorded while leecher/completion state continues changing |
| Duplicate announce | Repeat a step with identical values | Duplicate delivery credits volume or completion repeatedly |
| Implausible jump | Increase a counter by tens of GiB in one or two seconds | The claim is accepted without rate checks, risk scoring, or further review |
| Repeated `started` | Send multiple `started` events with different baselines | A new `started` can erase history or credit volume again |
| Announce after `stopped` | Send `started`, then `stopped`, then an ordinary announce without a new `started` | A closed session continues without appropriate lifecycle checks |
| Events without an active session | Send `stopped` or `completed` as the first request | The tracker creates a full session or credits statistics |
| Peers issued to an invalid session | Send deliberately contradictory values | A weakness only if tracker policy requires blocking such sessions |

### Minimal combined test

For a quick check of several error classes, create this scenario:

1. `started`: `uploaded=0`, `downloaded=0`, `left=1000000000`.
2. After two seconds: no event, `uploaded=20000000000`, `downloaded=0`,
   `left=1000000000`.
3. After two seconds: no event, `uploaded=100`, `downloaded=0`,
   `left=500000000`.
4. `completed`: `uploaded=100`, `downloaded=0`, `left=500000000`.
5. Repeat step 4 two more times.
6. Send `stopped`, then an ordinary announce without a new `started`.

This combines implausible upload, counter rollback, inconsistent
`downloaded`/`left`, invalid and repeated `completed`, and continuation
after `stopped`.

### Confirmation criteria

A weakness is indicated if the tracker:

- credits claimed upload without checking history and elapsed time;
- reduces previously stored maximum counters;
- counts one completion multiple times;
- accepts `completed` with nonzero `left`;
- lets unrestricted `started` events reset baselines;
- records neither contradictions nor an increased risk score;
- continues a closed session without a new `started`.

A `200 OK` response and returned peers alone do not establish an accounting
bug: a tracker may deliberately separate peer discovery from statistics
validation. Correlate PeerLume's exchange log with tracker server logs and
statistics for the dedicated test account/passkey.

## Normal session model

The tracker should group events using at least:

- account or passkey;
- `info_hash`;
- Peer ID;
- IP address and address family;
- listening port;
- transport: HTTP, HTTPS, or UDP;
- first and most recent announce times.

Expected properties of a normal session include:

- nondecreasing `uploaded` and `downloaded`;
- nonincreasing `left`;
- `left` between zero and the torrent size;
- `completed` only when `left == 0`;
- repeated `started` creates a new session or explicitly resets the old one;
- `stopped` closes an existing session;
- transfer rates calculated between announces remain physically plausible;
- HTTP and UDP events for the same session do not contradict each other.

## Initial state without data transfer

To report that a tracker session has started but no bytes have been downloaded
or uploaded yet, the first announce should contain:

```text
event=started
uploaded=0
downloaded=0
left=<full torrent size in bytes>
```

For a torrent of 1 GiB:

```text
event=started
uploaded=0
downloaded=0
left=1073741824
```

If there is still no progress at the next announce, send the same counters
without an event:

```text
event=None
uploaded=0
downloaded=0
left=1073741824
```

`left=0` claims that the client already has all content and should be treated
as a seeder. It does not mean that downloading has not started. `started`
marks the start of a tracker session, not proof of payload transfer.
Do not send `completed` until downloading has brought `left` to zero.

For a clean full download, initial `left` equals the entire torrent size.
Selective-download interpretations vary between clients, so select all files
for this baseline test. Like other announce fields, these values are client
claims and do not prove that no peer transfer occurred.

Client restarts, file rechecks, selection changes, and lost local state can
legitimately alter the observed pattern. Account for these cases before
applying sanctions.

## Scenario 1: honest baseline session

### Sequence

1. Send `started` with known initial counters.
2. Send several periodic announces with increasing `downloaded` and a
   consistent decrease in `left`.
3. Send `completed` when `left == 0`.
4. Close the session with `stopped`.

### Expected result

- No increase in risk score.
- One continuous session is retained.
- Credited volume matches control measurements.
- Completion is counted once.
- Duplicate delivery does not double statistics.

Use this scenario as the control group for every subsequent test.

## Scenario 2: persistent `downloaded=0`

### Sequence

The client remains a leecher, announces regularly, and receives peers, but
keeps `downloaded=0` in every message.

### Signals

- A long active session with unchanged counters.
- Outgoing peer connections, if suitable server-side telemetry is available.
- A later `completed` without intermediate growth in `downloaded`.
- A mismatch with earlier sessions belonging to the same account.

### False-positive safeguards

Zero transfer is legitimate when a peer cannot find sources, is choked, or
stops immediately after `started`. The signal becomes stronger only when
combined with decreasing `left`, `completed`, or sustained activity.

## Scenario 3: decreasing `left` without increasing `downloaded`

### Sequence

Reduce `left` in each announce while leaving `downloaded` unchanged.

### Signals

- Changes in `left` and `downloaded` are inconsistent.
- Observed progress exceeds the increase in downloaded bytes.
- The anomaly repeats over multiple intervals.

### Expected result

The tracker increases the risk score after the first contradiction and raises
it further for repeated occurrences. Selective downloads may interpret `left`
differently; allow a tolerance rather than insisting on exact byte equality.

## Scenario 4: counter rollback

### Sequence

After several ordinary announces, send a smaller `downloaded` or `uploaded`
value, or a larger `left`.

### Signals

- Monotonicity is violated within one identified session.
- No new `started` event.
- Unchanged Peer ID, IP address, and listening port.

### Expected result

The tracker does not subtract already credited volume. It flags the session
as inconsistent and preserves the previous maximum accepted counter. A new
explicit `started` may establish another baseline, but frequent resets are
themselves a separate signal.

## Scenario 5: invalid `completed`

### Variants

- `completed` when `left > 0`.
- `completed` without a previously observed `started`.
- Multiple `completed` events for one session.
- `completed` followed by nonzero `left`.

### Expected result

The tracker avoids duplicate increases to the global completion count,
records the inconsistent event, and increases risk. Treat a missing
`started` more cautiously: the tracker may have restarted or lost some
short-lived state.

## Scenario 6: physically implausible counter jump

### Sequence

Between closely spaced announces, increase `downloaded` or `uploaded`
by an amount requiring a rate above the configured laboratory limit.

### Calculation

`observed_rate = counter_delta / elapsed_seconds`

Thresholds must account for delayed delivery, coalesced events, fast local
networks, seedboxes, and restored state. Do not apply one universal threshold
to all users.

### Expected result

Assign low or medium weight to one event. High weight requires repetition
and additional contradictions.

## Scenario 7: frequent Peer ID changes

### Sequence

Keep the account, `info_hash`, IP address, and port unchanged while sending
announces with new Peer IDs.

### Signals

- Many Peer IDs in a short period.
- Overlapping active sessions.
- Every new identifier starts with a favorable baseline.
- Identical behavior signatures across identifiers.

### False-positive safeguards

Peer IDs can legitimately change after reinstallation, switching clients,
or privacy-setting changes. Evaluate frequency and concurrency, not merely
the fact that an ID changed.

## Scenario 8: shared passkey

### Sequence

Simultaneously announce under one account from multiple test IP addresses,
Peer IDs, and ports, exceeding the configured limit.

### Signals

- Geographically or topologically incompatible simultaneous sessions.
- Too many active IP addresses, autonomous systems, or devices.
- Many peers for the same `info_hash`/account pair.
- An abrupt IP change without closing the preceding session.

### Expected result

Apply a dedicated account-sharing policy. Allow for NAT, VPNs, dual-stack
connectivity, and dynamic addresses; do not automatically treat them as
violations.

## Scenario 9: lifecycle ordering violations

### Variants

- `stopped` without an active session.
- A periodic announce after `stopped` without a new `started`.
- Repeated `started` events.
- Disappearance without `stopped`.
- `completed` after the session timeout expires.

### Expected result

The tracker tolerates UDP packet loss and client crashes. Missing `stopped`
is not a violation. Repeated sequences used to reset a baseline increase risk.

## Scenario 10: HTTP/UDP desynchronization

### Sequence

Alternate HTTP and UDP announces for one logical session while supplying
inconsistent counters, events, or listening ports.

### Signals

- Matching account, `info_hash`, and Peer ID have different baselines.
- One transport reports completion while the other remains a leecher.
- Counters roll back when switching transports.
- A UDP transaction ID or action does not match the request.

### Expected result

Merge transport events only when session keys match sufficiently.
IPv4/IPv6 or HTTP/UDP transitions alone must not be treated as violations.

## Additional robustness checks

The audit harness should also exercise malformed and repeated packets:

- missing required parameters;
- overflows and negative textual values;
- excessively long Peer IDs, query strings, and failure reasons;
- unknown events;
- incorrect UDP actions and transaction IDs;
- retries following a network timeout;
- reordered and duplicated UDP packets;
- rate-limit violations;
- requests from blocked IP addresses.

These checks test parser and transport robustness, not proof of false accounting.

## Example risk score

Tune weights using statistics from your own tracker. An initial model might be:

| Signal | Points |
| --- | ---: |
| One counter rollback | 25 |
| Repeated rollback in the same session | 40 |
| `completed` with `left > 0` | 35 |
| Decreasing `left` without increasing `downloaded` | 20 |
| Repeated inconsistent changes | 35 |
| Implausible transfer speed | 10 |
| Frequent Peer ID changes | 15 |
| Exceeding the simultaneous-IP limit | 30 |
| HTTP/UDP desynchronization | 20 |
| Rate-limit violation | 10 |

Example responses:

- `0-29`: retain observation.
- `30-59`: increase logging and shorten the reassessment interval.
- `60-89`: temporarily restrict the session and refer it for review.
- `90+`: temporary ban with an appeal/review path.

Scores should decay over time. An indefinite lifetime sum creates false bans
from old failures.

## What to log

For an investigation, retain:

- timestamp and transport;
- account ID or an irreversible internal passkey identifier;
- `info_hash`;
- Peer ID;
- IP address, address family, and port;
- event;
- counters before and after normalization;
- elapsed time between events;
- triggered rules and score increments;
- tracker response status;
- audit-run identifier.

Never log the passkey itself or the full credential-bearing URL.

## Acceptance criteria

A tracker passes the audit if:

1. The baseline session receives no suspicious score.
2. Duplicate announces do not double statistics.
3. Counter rollback cannot reduce previously credited values.
4. Invalid `completed` events do not increase the completion count.
5. A missing `stopped` does not automatically cause a ban.
6. Every anomalous scenario leaves an explainable rule record.
7. Neither logs nor diagnostics contain passkeys.
8. HTTP and UDP use the same counter-normalization model.
9. Rate limits do not interfere with ordinary sessions.
10. Operators can reverse automated decisions and restore accounts.

## Manual audit client

The `tracker-audit` utility sends one controlled announce per invocation.
It accepts explicit `uploaded`, `downloaded`, and `left` values, uses
`--peer-id` to retain a stable Peer ID across invocations, and returns
IPv4 peers as JSON. Reproduce scenarios 1-6 independently of active downloads
and their selected reporting modes.

In the desktop UI, the same fields are available under **Advanced tracker
audit** in the add-torrent dialog. **Send audit announce** performs the request
in the background and displays the interval and peer list. These manually
specified values do not configure the ordinary download session.

Only loopback is allowed by default. For staging, supply the exact hostname
using `--allow-host`. Prefer `PEERLUME_AUDIT_TRACKER_URL` for a URL containing
a passkey so it does not appear in process arguments. Every invocation sends
exactly one announce.

## Further automation plan

Future work for `tracker-audit` includes named multi-step scenarios, a fixed
random seed, a final JSON report, and a nonzero exit code when expected
invariants fail. Staging hosts still require an explicit allowlist and a
dedicated test passkey.

Recommended automation order:

1. Baseline HTTP session.
2. Counter scenarios 2-6.
3. Lifecycle and Peer ID changes.
4. Account sharing.
5. UDP and HTTP/UDP consistency.
6. Malformed input, timeouts, retries, and rate limits.
7. Regression runs after every tracker-accounting change.

# External tracker behavior profiles

## Download mode when adding a torrent

**Download mode** is directly below the torrent information. Every new dialog
defaults to **Normal download**. In the TUI, entering a path opens a mode-number
prompt with `1` selected and choices `1` through `5`. `Enter` confirms
addition; `Esc` cancels. Transfer does not start before confirmation. The mode
appears in **Info**, is saved separately per torrent, and is restored after
restart. Older SQLite records migrate to `normal`.

| Mode | Downloading | Announced metrics |
| --- | --- | --- |
| `normal` | Ordinary peer transfer | Actual `uploaded`, `downloaded`, and `left` |
| `no_progress` | Ordinary peer transfer | `uploaded=0`, `downloaded=0`, `left=full torrent size`; completion is not reported as `completed` |
| `upload_ratio_1_5` | Ordinary peer transfer | Actual `downloaded` and `left`; `uploaded=floor(downloaded * 3 / 2)` |
| `hide_downloaded` | Ordinary peer transfer | Only `downloaded=0`; actual `uploaded` and `left` are retained |
| `freeze_after` | Ordinary peer transfer | Actual metrics before the threshold, then one frozen snapshot of all three counters |

`no_progress` continues periodic announces and receives updated peer lists.
It differs from **Started-only observation**, which sends one separate audit
request, leaves the real transfer paused, and then observes silently.
`upload_ratio_1_5` recalculates its ratio at every announce rather than using
a constant counter. Odd byte counts round down; arithmetic saturates at
the maximum `u64` value.

In every mode, the GUI and TUI display actual local counters. Transformations
occur only at the HTTP/HTTPS and UDP announce transport boundary. Tracker
responses still feed peers to the transfer engine. Policies are scoped to the
info hash and session Peer ID; another torrent or session does not inherit
them. Global `privacy.tracker_reporting = "disabled"` takes precedence and
disables all tracker communication.

Mode availability and ordering are defined at the top of the external file:

```toml
version = 1
download_modes = ["normal", "no_progress", "upload_ratio_1_5", "hide_downloaded", "freeze_after"]
```

If an older file omits this field, all five modes are available. The exact
legacy default list containing the first three modes is extended in memory
with the two new modes; the user's file is not overwritten.
`normal` must remain first. Test modes may be removed from custom lists.
The transfer backend supports these test modes for v1/hybrid `.torrent`
files with a known nonzero total size. Magnets currently start in normal
mode until metadata is resolved.

The separate audit scenarios described below remain available under
**Advanced tracker audit**. Their `profiles` and `steps` do not replace
the **Download mode** selection.

## Real announce journal and JSONL

The selected torrent's **Log** tab in egui and ratatui shows requests from the
real download backend, not estimates synthesized from UI progress and not
the separate **Advanced tracker audit** requests. A record appears after a
response, error, or cancellation; HTTP retries produce separate records.
A UDP record covers connection-ID acquisition and one announce; connecting
alone is not counted as a transmitted announce.

Every application run automatically creates
`%LOCALAPPDATA%/PeerLume/announce-logs/announces-<run-id>.jsonl`.
Each JSONL record contains:

- `schema_version`, `run_id`, a monotonic `sequence`, Unix start time in
  milliseconds, elapsed request time, info hash, the actual Peer ID in hex,
  port, transport, and a safe tracker identifier;
- `mode` and the actual `event` (`null` means the event argument is absent);
- `actual.uploaded/downloaded/left` before transformation and `sent` afterward;
- `verified_bytes`, `full_size`, and `selected_bytes` when available;
- `request_transmitted`: `true` after an HTTP response or a successful UDP
  announce send; `false` if the UDP announce has not been sent, for example
  because connecting failed; `null` if an HTTP error/cancellation cannot
  reliably establish whether the request was transmitted;
- `outcome`, HTTP status, interval/minimum interval, seeders/leechers,
  a warning or failure reason, and **all parsed peer addresses** in `reply.peers`.

For a failed/cancelled attempt, `sent` describes the prepared request.
Assess delivery together with `request_transmitted`, `outcome`, and server
logs. Responses are recorded before forwarding peers to the engine, so a slow
peer consumer does not delay their appearance in the journal. Tracker paths,
userinfo, and queries are not recorded because they may contain passkeys.
Known URL secrets reflected in error/warning messages are also redacted.
Peer IP addresses, info hashes, and Peer IDs remain in the log; consider their
confidentiality before sharing reports. Raw HTTP headers/bodies and
BitTorrent peer-wire packets are not saved.

For a multi-hour test, leave the application running. Records are appended
and flushed after every write. Memory retains only the most recent 1024
records **across the whole session**, filtered by the selected torrent in
the UI. Full history remains on disk. In egui, use **Export torrent JSONL**
or **Export session JSONL**; in ratatui's **Log** tab, press `e` or `E`
and enter a new file path. Export reads the complete saved history for the
current run, not just the visible tail, and never overwrites existing files.
Older run files remain available in `announce-logs`.

File-open/write errors are shown in **Log**. Such a journal is incomplete,
and export reports the error. Files are not automatically rotated or deleted:
reserve sufficient disk space beforehand. `flush` is not `fsync`; an abrupt
shutdown may lose the latest data, and a process killed during an unfinished
attempt may leave no record of that attempt. The journal records engine
events literally and does not synthesize missing `completed`/`stopped`.
The current HTTP backend starts with `started` and then sends announces
without an event; UDP chooses events from its state and `sent.left`.
Inspect the journal instead of inferring events from UI completion.

### Partial downloads and file rechecks

File selection is not a separate reporting mode: it works with all five
modes. Select files during addition or in **Files**. Selection changes reach
the real engine and are persisted. **Recheck files** in **Log** (ratatui:
`v`) stops the torrent instance and re-adds it with the same selection,
directory, and mode, without deleting data. Initialization verifies the files
against their hashes again. A previously paused torrent stays paused after
verification; otherwise it continues. Re-adding may trigger a new `started`.

`actual.downloaded` is network payload received from peers during the current
application session, including repeated/rejected blocks, not file size on
disk. `verified_bytes` is checked existing data; initial scanning is not
treated as if all scanned bytes were already verified. `actual.left` is
based on the **entire torrent**, not merely selected files. Recheck and
pause/resume retain cumulative network counters; restarting the application
begins a new session. The UI progress table still shows selected-file readiness.

Control test: place one correct file from a two-file torrent in the output
directory beforehand, select it, and recheck. Expect `downloaded=0`,
nonzero verified bytes, and `left` equal to the missing portion of the full
torrent. Tracker statistics should not turn verification into newly
downloaded volume. To test actual resuming, select the missing file and
compare network `downloaded` increments with tracker logs. Completing the
selected subset does not mean possessing the entire torrent.

### Hiding downloaded only

Select `hide_downloaded` when adding a torrent. Only `downloaded` changes:
it is always zero. `uploaded` remains actual, and `left` decreases according
to verified data. Peer transfer and periodic announces continue.
Compare server-side upload/download accounting with JSONL `actual` and
`sent`. A discrepancy alone does not establish a vulnerability; assessment
depends on provider accounting rules, existing data, and server-side correlation.

### Freezing metrics after a volume threshold

Select `freeze_after` and set **Freeze after received bytes** before adding.
The default is 1 GiB (1073741824 bytes); allowed values are
1 through 9223372036854775807. Mode and threshold are saved per torrent in
SQLite schema 6. Actual metrics are sent until the threshold is reached.
The **first announce** observing network `downloaded >= threshold` latches
all three `uploaded/downloaded/left` values. This is an announce-boundary
snapshot, not a snapshot at the exact threshold byte. Subsequent announces
reuse it unchanged, while peer transfer and peer-list updates continue.
Journal `actual` values continue changing. If the frozen `left>0`,
completion must not be inferred from actual progress instead of the sent value.

A threshold above attainable volume may never trigger. A low threshold
combined with infrequent announces may latch an already completed download.
Choose data size and threshold so transfer continues after the first matching
announce. Pause/resume retains the snapshot; recheck and application restart
reset it. Recheck retains network totals, so an already-exceeded threshold
may immediately latch another snapshot. Observe whether the tracker notices
static `sent` values alongside increasing `actual`, and how it handles
new sessions.

The shared catalog is `%LOCALAPPDATA%/PeerLume/tracker-behaviors.toml`. It is
created once from `config/tracker-behaviors.example.toml` when egui starts or TUI
first loads profiles. To select another file, set the environment variable before
starting either frontend:

```powershell
$env:PEERLUME_TRACKER_BEHAVIORS = "C:\tests\tracker-behaviors.toml"
```

The explicit file must exist. Invalid files report an error; they are never
silently replaced with defaults. Existing files are never overwritten by loading.

In egui, select **Load behavior profile** in the add-torrent dialog or the
selected torrent's **Trackers** tab. **Reload profiles** rereads the file. A
multi-step profile selected while adding is attached in full; after adding,
**Run scenario** executes it. A selected `started_only` profile keeps the existing
add-dialog behavior: normal transfer starts paused and observation starts after
**Add torrent**.

In ratatui, open **Trackers**, press `b` to reload the file, and enter a profile
ID. Press `A` to run all steps, or `x` to cancel delays/observation and prevent
subsequent steps. An in-flight network request finishes or reaches its timeout
before cancellation takes effect. Exiting also cancels outstanding waits.

Loading/applying a catalog entry sends no announce. Each torrent stores its own
resolved copy in SQLite. Restarting restores that copy without requiring the
external catalog. Editing/reloading the catalog affects future applications of a
profile; reapply it explicitly to update an existing torrent. A running scenario
keeps its initial copy. Scenarios are not automatically resumed after a restart.

Example combination:

```toml
version = 1

[[profiles]]
id = "session-test"
label = "Started, periodic updates, stopped"
mode = "scenario"
allowed_host = "tracker.staging.example"
# port = 51413  # omit to use the current announce/listen port

[[profiles.steps]]
event = "started"
uploaded = 0
downloaded = 0
left = "full_size"

[[profiles.steps]]
event = "none"
delay_seconds = 1800
repeat = 4
uploaded = 0
downloaded = 0
left = "full_size"

[[profiles.steps]]
event = "stopped"
delay_seconds = 1800
left = "full_size"
```

Supported modes:

| Mode | Behavior |
| --- | --- |
| `manual` | One non-repeating step, available for explicit audit requests. |
| `started_only` | One immediate `started` with zero uploaded/downloaded and full size in `left`, followed by silence for `observation_seconds` (60–604800). No explicit steps are accepted. |
| `scenario` | Ordered explicit steps. Each step can repeat with the same metrics. |

Each step accepts `event = "none"`, `"started"`, `"completed"`, or `"stopped"`.
`none` omits the event query parameter. `delay_seconds` defaults to zero and is
relative to the preceding request; it is applied before **each** repetition.
`repeat` defaults to 1. Uploaded/downloaded default to zero; `left` is required.
Metrics can be byte counts, `"full_size"` (all torrent files), or
`"selected_size"` (selected files). Size values are resolved when applying the
profile, not continuously during the download. `started_only` always uses the
full size even when only a subset of files is selected.

Unknown fields, duplicate IDs, invalid events/modes, zero ports, excessive values,
and missing required fields reject the catalog. Limits are 128 profiles, 1024
expanded steps per profile, seven days of total scheduled delays/observation,
and a 1 MiB file. Metric values must fit SQLite's signed integer range.
`observation_seconds` is valid only for `started_only`.

Tracker URL and passkey remain in each torrent's existing secure storage. The
catalog contains only the staging `allowed_host`, optionally an announce port,
and scenario definitions. Empty `allowed_host` permits loopback only; the
existing exact staging-host check still applies to every audit request.

These profiles configure the isolated audit runner. The real transfer session's
reporting is still configured in `settings.toml` as
`[privacy] tracker_reporting = "accurate"` or `"disabled"`, with restart required.
For tests requiring a tracker to see only scenario traffic, pause the normal
transfer before starting the scenario.
