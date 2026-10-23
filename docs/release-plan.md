# PeerLume release plan

This plan takes PeerLume from the current desktop prototype to a production-oriented
BitTorrent client. Each milestone keeps formatting, strict Clippy, unit, property,
integration, and container tests green. Network features remain disabled in the UI
until deterministic local tests pass.

Tracker passkeys are never stored as plaintext. On Windows, the full Tracker URL is
protected with current-user DPAPI and restored for the matching user after restart.
The ordinary SQLite URL column, logs, errors, and status messages contain only a
structurally redacted URL.

This is a living plan with two independent tracks: the client milestones below and
the [library publication track](#library-publication-track). A working application
or green application CI does not by itself establish library release readiness.
Add new requirements as they emerge, and mark work complete only with linked
implementation and verification evidence.

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

## Library publication track

Status: planning. Initial readiness review: 2026-10-23, application commit
`63556d9`. The first target is an experimental library alpha, not a stable SDK.
No release date or final crate layout has been committed.

Publish PeerLume as an application-oriented core built on librqbit: torrent
management, reporting policy, verification and recovery, accounting observations,
and integration contracts. Do not describe it as an independent BitTorrent engine
or require a network-engine rewrite as part of library extraction.

### Current baseline

- [x] GUI and TUI rendering are separated from shared client and session logic.
- [x] Headless core builds are supported inside the repository.
- [x] Commands, events, persistence, verification, recovery and journals have tests.
- [x] Windows and Linux application CI and local Docker fixtures are available.
- [ ] A plain external consumer can build the core without repository overrides.
- [ ] A deliberately scoped public API and compatibility policy are established.
- [ ] Runtime, paths, persistence and secrets can be supplied by the embedding host.
- [ ] Package metadata, examples and artifact verification are release-ready.

These checked items describe existing infrastructure, not completion of the
library milestones. The initial external-consumer probe failed: a normal path
dependency with `default-features = false` selected the upstream
`librqbit-tracker-comms`, which lacks PeerLume's reporting and observation APIs.

### LIB 1 Resolve the dependency publication blocker

Priority: release blocker.

- [ ] Decide whether to upstream the required tracker hooks or maintain a
  consistently published fork dependency chain.
- [ ] Ensure PeerLume and librqbit use the same intended tracker implementation;
  registrations must affect the workers that perform real announces.
- [ ] Remove the requirement for consumers to copy PeerLume's root Cargo patch.
- [ ] Add a minimal external-consumer fixture and run it in CI outside the
  application workspace, with no inherited repository patch or Cargo config.
- [ ] Verify reporting policy and real announce observations through that consumer,
  not merely that its imports compile.

Cargo only applies patches from the consuming workspace root; patches declared
by dependencies are ignored. A direct path dependency or a renamed fork alone
must not be assumed to replace librqbit's transitive tracker dependency.
See the [Cargo dependency override rules](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section).

Done when a clean consumer builds and exercises the intended transport using the
distributable dependency graph, without requiring a consumer-side patch.

### LIB 2 Define crate boundaries and a small public API

Proposed structure, subject to a recorded design decision:

- `peerlume-core`: headless domain and session facade over the transfer backend.
- Optional SQLite persistence and platform secret-store adapters, behind features
  or in separate crates.
- An application package containing egui and ratatui frontends and desktop defaults.

- [ ] Decide the workspace and feature layout; keep UI dependencies out of the
  library's default dependency graph.
- [ ] Define a supported `Client` or `Session` facade with builder-based options.
- [ ] Introduce typed torrent identifiers, lifecycle states and library errors;
  avoid string-formatted IDs and string-only error matching as the public contract.
- [ ] Keep internal implementation modules private unless intentionally supported.
- [ ] Use extensible options and non-exhaustive types where appropriate, so adding
  a setting does not force consumers to rebuild large struct or enum literals.
- [ ] Avoid exposing backend-specific types where they would unnecessarily bind
  consumers to the current fork or backend version.
- [ ] Separate supported settings from modeled or planned settings. Return an
  explicit unsupported-capability result instead of implying an option was applied.
- [ ] Keep accurate reporting as the default and clearly distinguish experimental
  audit/reporting policies from ordinary transfer accounting.

Done when one documented public facade supports the common workflow without UI
types, internal module imports, or unsupported configuration assumptions.

### LIB 3 Make embedding and lifecycle behavior explicit

- [ ] Accept paths, journal configuration, persistence and resource limits from
  the embedding host; remove mandatory PeerLume data-directory conventions.
- [ ] Make file creation and persistence opt-in or explicit in builder options.
- [ ] Choose and document runtime ownership: an owned runtime, an existing Tokio
  runtime, or supported implementations of both.
- [ ] Report startup failures through a typed result or readiness handshake.
  Do not silently turn initialization failures into missing optional services.
- [ ] Provide cancellation and explicit shutdown suitable for async callers.
- [ ] Define Drop behavior without requiring a GUI event loop or unsafe assumptions
  about blocking inside a host runtime.
- [ ] Document event ordering, telemetry coalescing, control backpressure, receiver
  closure and the host's responsibility to drain events.
- [ ] Test stalled consumers, startup failure, cancellation, repeated session
  creation and shutdown from supported sync and async contexts.

Done when a standalone CLI or service can own the library's lifecycle and storage
without modifying process-global application settings or relying on UI polling.

### LIB 4 Define portability and secret-storage guarantees

- [ ] Introduce an injectable secret-store contract or equivalent explicit policy.
- [ ] Retain Windows DPAPI as an adapter rather than a universal library assumption.
- [ ] Define supported behavior on Linux and other target platforms before claiming
  that private Tracker URLs survive restart there.
- [ ] Return explicit errors or unsupported-capability results when credentials
  cannot be securely persisted; do not silently restore an unusable redacted URL.
- [ ] Test credential round trips, unavailable stores, user-binding failures,
  redacted exports and absence of secrets in errors and diagnostics.
- [ ] Publish a platform support matrix; do not infer macOS or other platform
  support from successful Windows and Linux application builds.

Current behavior uses current-user DPAPI on Windows. The non-Windows persistence
path does not preserve the full secret URL. This is a portability limitation to
resolve or explicitly constrain, not a completed cross-platform secret store.

Done when supported platforms have tested, documented restart behavior and no
plaintext-secret fallback is introduced implicitly.

### LIB 5 Establish packaging and compatibility policy

- [ ] Choose an owner-approved project license and review obligations for vendored
  dependencies before distributing a standalone library package.
- [ ] Complete package metadata: description, repository, documentation, license,
  intended package contents and minimum supported Rust version.
- [ ] Set and test the MSRV rather than inferring it from the edition or one local
  build; exercise dependency resolution without relying solely on the app lockfile.
- [ ] Define the alpha stability policy, supported features, breaking-change rules,
  persistence/schema compatibility and event-format compatibility.
- [ ] Review the packaged file list for local downloads, credentials, generated
  output and unnecessary application-only resources.
- [ ] Add rustdoc and compiled examples for creation, adding a torrent, observing
  events, changing policy, handling errors and shutting down.
- [ ] Document backend limitations, experimental policies and dependency provenance.

Done when a new user can understand the support contract and build a documented
headless example from the package rather than from the development checkout.

### LIB 6 Verify and release the distributable artifact

- [ ] Run the external-consumer test against the packaged artifact and resolved
  release dependency graph, not just against workspace source paths.
- [ ] Verify minimal core, optional persistence, supported frontend combinations,
  MSRV and current stable Rust on the supported platform matrix.
- [ ] Run formatting, strict Clippy, unit, property, integration, container and
  documentation tests; retain the existing transfer and recovery regressions.
- [ ] Add library lifecycle and API-contract regressions, resource-bound checks,
  multi-session isolation and portability coverage.
- [ ] Inspect `cargo package --list`, perform package verification and a publish
  dry run in a clean environment. Resolve package failures rather than bypassing
  verification with `--no-verify`.
- [ ] Review release notes, supported capabilities, unresolved alpha limitations,
  security/privacy guarantees and license metadata.
- [ ] Obtain explicit approval for the package name, version, registry and actual
  publication. A documentation update or dry run is not publication authorization.

Done when the verified alpha artifact is reproducible, its clean-consumer checks
pass, and the approved publication can proceed without local overrides.

### Library delivery order

1. LIB 1: fix the dependency graph and add a failing-then-passing consumer test.
2. LIB 2: choose package boundaries and the intentionally supported public facade.
3. LIB 3 and LIB 4: define host integration, lifecycle and secret-store behavior.
4. LIB 5: finalize documentation, licensing, MSRV and compatibility expectations.
5. LIB 6: verify the artifact and prepare an explicitly approved alpha release.

### Open library decisions

- Dependency strategy: upstream hooks or a maintained published fork chain.
- Package layout: workspace crates versus optional adapter features.
- Runtime ownership and the sync/async embedding contract.
- Secret-store providers and the initial supported platform matrix.
- License, MSRV, package names and alpha version/stability policy.
- Whether experimental tracker-audit policies require a separate opt-in feature.
- Which real backend capabilities are guaranteed by the first public release.

### Maintaining the library plan

Keep LIB identifiers stable. Add new milestones with new identifiers rather than
renumbering existing ones. Add a date and decision rationale when resolving an
open question. Link completed items to commits, tests or verified artifacts, and
retain unresolved limitations in the release notes. Application CI success alone
does not close an external-consumer or package-publication gate.

The client delivery order and gates below remain a separate application track.

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
