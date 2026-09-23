# Private torrent support

PeerLume must treat the `private=1` flag as a mandatory protocol constraint.

## Required behavior

- Discover peers only through trackers listed in `announce` or `announce-list`.
- Disable DHT, PEX, and LSD for the private torrent even when globally enabled.
- Disconnect existing non-tracker peers if metadata later establishes that the torrent is private.
- Never place the private torrent info hash into DHT or local discovery.
- Treat credentials embedded in tracker URLs as secrets.
- Redact passkeys, query tokens, and authorization data from logs, metrics, errors, and UI.
- Send accurate `uploaded`, `downloaded`, and `left` counters when announcing.
- Support `started`, `completed`, and `stopped` tracker events.

## Reporting modes

`accurate` contacts the tracker and sends counters derived from verified payload bytes.

`disabled` makes no announce or scrape requests. For a private torrent this normally
means no peer discovery and therefore no download unless peers are supplied through
an explicitly authorized private deployment mechanism.

Arbitrary or falsified counters are not part of the production client.

## Testability

Tracker communication should be behind a `TrackerClient` trait. Tests may use a mock
implementation or a tracker bound to loopback. Synthetic counter values must only be
accepted by test-only code and must never be sent to a non-loopback endpoint.

Suggested counter rules:

- Count downloaded bytes only after the received block passes protocol validation.
- Count uploaded bytes when payload bytes are successfully written to the peer socket.
- Derive `left` from verified pieces, including selected-file behavior.
- Keep accounting independent from UI speed samples and transport overhead.
