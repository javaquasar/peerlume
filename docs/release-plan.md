# PeerLume release plan

This plan takes PeerLume from the current desktop prototype to a production-oriented
BitTorrent client. Each milestone keeps formatting, strict Clippy, unit, property,
integration, and container tests green. Network features remain disabled in the UI
until deterministic local tests pass.

Tracker passkeys are never stored as plaintext. On Windows, the full Tracker URL is
protected with current-user DPAPI and restored for the matching user after restart.
The ordinary SQLite URL column, logs, errors, and status messages contain only a
structurally redacted URL.

## Milestone 0.2: hostile-input boundary

### 1. Resource and path limits

Use one `ResourceLimits` policy for metainfo, tracker responses, magnet metadata,
blocklists, resume state, file trees, peer messages, and UI imports. Enforce byte,
depth, node, file, tracker, path, content-length, decompression, and rule limits.
Reject Windows aliases, reserved names, alternate streams, duplicate paths,
symlink/junction escapes, and file/directory collisions. Revalidate final paths
immediately before creation.

Done when hostile input fails before unbounded allocation or filesystem mutation.

### 2. Property and fuzz testing

Property-test bencode, tracker responses, cache decoding, path normalization, piece
mapping, peer messages, magnet metadata, and resume migrations. Maintain fuzz targets
and retain discovered failures as a regression corpus.

Done when arbitrary bytes cannot panic and every parser has bounded allocation.

## Milestone 0.3: complete transfer engine

### 3. Multi-peer, multi-piece session

Implement one cancellable task per torrent and bounded tasks per peer. Support
bitfield, have, choke, unchoke, request, piece, cancel, keepalive, timeouts,
reconnect backoff, request pipelining, and endgame duplicates. Publish real progress,
rates, peers, errors, and ETA through bounded event channels.

Done when several deterministic peers jointly serve a complete multi-piece torrent.

### 4. Piece scheduling and selective download

Support rarest-first and sequential strategies, Skip/Low/Normal/High priorities,
first/last-piece priority, availability counts, in-flight ownership, retries, and
endgame replication. Map selected file ranges to pieces and write only selected
segments after the complete piece passes its hash check. Preserve v2 padding.

Done when boundary pieces are verified without writing skipped file ranges.

### 5. Storage, resume, and disk pipeline

Use positioned I/O, bounded disk jobs, free-space checks, optional preallocation,
sparse files, safe temporary names, atomic completion moves, an open-file limit, and
a bounded upload read cache. Checkpoint verified pieces, counters, priorities, and
uncertain pieces in one SQLite transaction. Recheck only uncertain data after restart.

Done when interrupted transfers resume without redownloading trusted pieces.

### 6. Memory and throughput controls

Use a reusable block-buffer pool, bounded request counts and channels, immediate
buffer release after verify/write, batched UI events, and batched SQLite writes.
Keep positioned I/O as the default; add memory mapping only after measurements.

Done when large swarms and 100,000-file torrents stay within configured memory.

## Milestone 0.4: trackers and discovery

### 7. Tracker lifecycle

Implement HTTP/HTTPS and UDP announce, started/completed/stopped, tracker tiers,
failover, interval/min-interval, warnings, failure reasons, scrape, IPv4/IPv6 peers,
transaction validation, and exponential retry. Counters reflect verified payload;
disabling reporting disables tracker traffic.

Done when HTTP and UDP container trackers pass lifecycle and failure tests.

### 8. Magnet metadata and extension protocol

Implement extension handshakes and per-peer IDs, `ut_metadata` request/data/reject,
bounded assembly, duplicate handling, and info-hash verification. Open the normal
file-selection preview after verified metadata arrives.

Done when a magnet-only session retrieves verified metadata from local peers.

### 9. DHT, PEX, and LSD

Implement IPv4/IPv6 DHT and KRPC, PEX, and local discovery. Apply blocklist,
connection limits, and source attribution to every peer. For `private=1`, never
start these providers and evict non-tracker peers if metadata reveals privacy later.

Done when policy tests prove zero discovery traffic for private torrents.

### 10. BitTorrent v2 and hybrid torrents

Support SHA-256 Merkle verification, piece layers, file roots, padding files, v2
identifiers, and hybrid swarm selection. Reject inconsistent hybrid metadata.

Done when v1, v2, and hybrid fixtures produce identical selected output.

### 11. Network policy and reachability

Add SOCKS5 and HTTP tracker proxies, optional peer proxying, interface binding,
IPv6, listening ports, UPnP/NAT-PMP/PCP, encryption policy, connection and rate
limits, and deterministic token buckets. Check blocklists before connecting and
evict newly blocked peers after updates.

Done when bind, leak, rate, port mapping, and live blocklist tests pass.

## Milestone 0.5: persistence, security, and desktop workflow

### 12. SQLite state model

Use WAL, foreign keys, transactional migrations and checkpoints. Persist identity,
source metadata, redacted trackers, file selection and priority, overrides, queue,
counters, verified pieces, blocklist metadata, labels, schedules, and UI state.
Never persist tracker passkeys in plaintext; protect the exact URL with an
OS-user-bound secret mechanism and keep the ordinary tracker column redacted.

Done when migration, rollback, cascade, concurrent read, and crash recovery pass.

### 13. Safe filesystem behavior

Check free space before start and file extension. Prevent symlink and junction
traversal, ensure canonical parents stay under the destination, limit open handles,
distinguish list removal from data deletion, and move completed data atomically.

Done when adversarial paths and interrupted moves cannot escape the destination.

### 14. Complete add and file-selection UI

Support picker, drag-and-drop queue, folder import, clipboard magnets, watch folders,
Windows file/protocol association, and sequential previews. Provide a hierarchical
tree, tri-state folders, search, sorting, priorities, and shared-piece details.

Done when 100,000-file imports and previews remain responsive.

### 15. Torrent management UI

Add categories, labels, destinations, drag queue ordering, status filters, context
menus, per-torrent limits, schedules, notifications, completed-file moves, and
sequential mode. Peers, Trackers, Pieces, Files, and Log show real engine data.

Done when the full workflow operates without blocking the UI thread.

### 16. Diagnostics and privacy

Use structured redacted logs with bounded rotation, event history, database health,
tracker timing, blocklist freshness, peer source, request latency, disk queue depth,
and buffer usage. Diagnostics exports exclude paths and credentials by default.

Done when automated secret scans find no passkey in persistent output.

## Milestone 0.6: release verification

### 17. Integration, interoperability, and soak testing

Expand testcontainers fixtures with HTTP/UDP trackers and slow, choking, corrupt,
disconnecting, and wrong-info-hash peers. Cover multi-piece, multi-file, private,
resume, v1/v2/hybrid, padding, proxy, blocklist, and low-disk cases. Test against
Transmission and a libtorrent client. Run Windows/Linux CI and a 24-hour soak.

Done when one documented command runs the deterministic release suite.

The tracker accounting threat model, anomaly matrix, false-positive controls, and
acceptance criteria are specified in `tracker-audit-scenarios.md`.

## Delivery order

1. Scheduler, selected-range storage map, checkpoints, and rate limiter.
2. Multi-peer v1 transfer using HTTP trackers.
3. UDP trackers, tiers, resume recovery, and real UI metrics.
4. Extension protocol and magnet metadata.
5. DHT/PEX/LSD with private-torrent enforcement.
6. v2/hybrid and network reachability.
7. Desktop completion, diagnostics, interoperability, and soak gates.

## Release gates

1. No parser panic or allocation outside configured limits.
2. No plaintext tracker passkey in persistent state, logs, errors, or diagnostics.
3. Private torrents emit no DHT, PEX, or LSD traffic.
4. Every completed piece is verified before trusted resume state.
5. SQLite migrations and checkpoints are transactional.
6. Blocked peers are rejected before handshake and evicted after updates.
7. UI commands are cancellable and perform no blocking network or disk work.
8. Memory, connections, requests, buffers, and queues have explicit limits.
9. Strict Clippy, formatting, unit, property, integration, and container tests pass.
10. Windows/Linux interoperability and the 24-hour soak pass before release.
