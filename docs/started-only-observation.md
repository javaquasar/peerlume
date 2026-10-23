# Started-only observation mode

## Purpose

`Started-only observation` supports long-running controlled observation of
a local or staging BitTorrent tracker's response to a client that creates
a tracker session but stops reporting its state after the first request.

This mode belongs to the isolated audit runner. It does not change normal
PeerLume transfer-session reporting or forward audit peers to the download
engine.

## Setup

Prefer enabling the mode when adding a `.torrent`:

1. Open the `.torrent` and wait for **Review torrent before adding**.
2. Keep **Download mode = Normal download**, enter the **Tracker URL**, and
   expand **Advanced tracker audit** below it.
3. For an external staging tracker, enter its exact hostname in
   **Allowed staging host**. Loopback and `localhost` require no allowlist.
4. Enable **Started-only observation after adding (staging audit only)**.
5. Set a duration from 60 to 604800 seconds (seven days).
6. Press **Add torrent**.

Selecting the mode automatically clears and disables **Start after adding**.
The torrent is added paused, **Trackers** opens automatically, and the single
audit announce is sent immediately after addition. Manual sending from the
add dialog is disabled in this mode to prevent accidentally sending twice.

The mode can also be enabled for an existing torrent:

1. Select the torrent.
2. Open **Trackers**.
3. Enter the audit **Tracker URL**.
4. For an external staging tracker, enter its exact **Allowed staging host**.
   Loopback and `localhost` require no allowlist.
5. Enable **Started-only observation (staging audit only)**.
6. Set a duration from 60 to 604800 seconds.
7. Press **Start observation**. Leave the normal transfer paused beforehand
   if the tracker must receive only one request.

The enabled flag, duration, staging hostname, announced port, and single
Tracker URL are persisted separately per torrent. On Windows, the exact
URL containing a passkey or other credentials is protected with current-user
DPAPI; SQLite and logs expose only a redacted form. The exact value is
restored after restart and used by the audit runner and normal transfer session.

## First announce

The mode sends exactly one request containing:

```text
event=started
uploaded=0
downloaded=0
left=<full torrent size in bytes>
```

The request also includes the v1 `info_hash`, Peer ID, and configured
listening port. This mode requires a v1 or hybrid torrent.
`left` is the sum of the sizes of **all** torrent files, not just the
selected download subset.

The add dialog contains one editable **Audit Peer ID**. It must contain
exactly 20 ASCII bytes. The entered value is attached to the torrent model,
displayed as **Audit Peer ID** in **Info**, and used by all its audit
announces. The production transfer backend uses its own session-wide Peer
ID, so changing the audit ID does not change ordinary peer connections.
The audit Peer ID is regenerated after restarting the application; set it
again before the next audit run if a particular value is required.

The tracker may return an interval and compact IPv4 peers. PeerLume logs:

- tracker hostname without a credential-bearing URL;
- request time and step number;
- `info_hash`, Peer ID, and port;
- `event`, `uploaded`, `downloaded`, and `left`;
- returned interval;
- peer list or error message.

## Observation period

After the initial response, the audit runner waits for the configured
duration without sending:

- periodic announces;
- announces without an event;
- `completed`;
- `stopped`;
- updated `uploaded`, `downloaded`, or `left` values.

The tracker therefore sees one started session that subsequently stops
updating. Keep PeerLume open throughout the test. When the duration expires,
the status changes to **Started-only observation completed**, without
another tracker request.

## Manual cancellation

**Stop** cancels the waiting period. Cancellation does not send `stopped`,
`completed`, or another final announce. The recorded initial exchange
remains in the log.

If cancellation is requested during an HTTP or UDP request, it takes effect
after the response or network timeout. During a multi-hour wait, cancellation
is checked regularly and requires no additional tracker request.

## Collecting results

PeerLume deliberately sends no requests after the first announce, so later
tracker reactions cannot be retrieved through that announce protocol.
Correlate the PeerLume log with these server-side observations:

- when the peer was registered as a leecher;
- the interval returned to the client;
- when the peer expired from the swarm by TTL;
- changes in the test account's uploaded/downloaded totals and ratio;
- changes in completion count;
- timeout, anomaly, or risk-score events;
- automatic warnings or account/passkey restrictions.

A normal safe response is to credit no transfer volume or completion and
remove the inactive peer after TTL expiry. A missing `completed` or
`stopped` alone is insufficient grounds for a ban: the client may have
crashed or lost connectivity.

## Limitations

- The mode proves neither the presence nor absence of actual payload transfer.
- Receiving an initial peer list is not itself a vulnerability.
- The audit log retains only the initial tracker response; subsequent
  changes must be checked using server-side telemetry.
- Restarting PeerLume ends active observation. Configuration is preserved,
  but an unfinished run does not resume automatically.
- Do not use real users' passkeys, Peer IDs, or production statistics.

## Difference from an ordinary scenario

With the checkbox disabled, the ordered audit-step editor is available.
Each step can specify its own delay, counters, and event.
Started-only mode temporarily disables the editor because its network
behavior is fixed: one `started`, followed by silence until the end of
the observation window.
