# Private torrent support

PeerLume must treat the `private=1` flag as a mandatory protocol constraint.

## Required behavior

- Discover peers only through trackers listed in `announce` or `announce-list`.
- Disable DHT, PEX, and LSD for the private torrent even when globally enabled.
- Disconnect existing non-tracker peers if metadata later establishes that the torrent is private.
- Never place the private torrent info hash into DHT or local discovery.
- Treat credentials embedded in tracker URLs as secrets.
- Redact passkeys, query tokens, and authorization data from logs, metrics, errors, and UI.
- Send accurate `uploaded`, `downloaded`, and `left` counters in the default Normal download mode.
- Support `started`, `completed`, and `stopped` tracker events.

## Reporting modes

`accurate` contacts the tracker and sends counters derived from verified payload bytes.

`disabled` makes no announce or scrape requests. For a private torrent this normally
means no peer discovery and therefore no download unless peers are supplied through
an explicitly authorized private deployment mechanism.

Each `.torrent` add dialog also offers two explicit test download modes:
`no_progress` and `upload_ratio_1_5`. Unlike standalone audit scenarios, these
modes keep actual peer transfer running and change outgoing announce metrics.
See the download-mode table in `tracker-audit-scenarios.md`.

## Testability

Tracker communication should be behind a `TrackerClient` trait. Tests may use a mock
implementation or a tracker bound to loopback. The automated download-mode tests
use local trackers and peers, verify downloaded file contents, and inspect actual
HTTP and UDP announce messages independently of the local transfer counters.

Suggested counter rules:

- Count downloaded bytes only after the received block passes protocol validation.
- Count uploaded bytes when payload bytes are successfully written to the peer socket.
- Derive `left` from verified pieces, including selected-file behavior.
- Keep accounting independent from UI speed samples and transport overhead.
