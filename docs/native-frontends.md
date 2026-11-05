# Native frontends and the background owner

Target release: 0.3.0. See the [release plan](releases/0.3.0/release-plan.md)
and its recorded validation results before treating a platform as verified.

## Ownership and startup

One daemon owns each profile's transfer engine, controller, writable SQLite
state, command revisions, agent grants and storage budgets. GUI and TUI are
clients of that owner. Two interfaces can use the same profile concurrently.
Closing an interface leaves transfers running; reopening it reconnects to the
existing owner and receives its current state.

Install the complete package. The selected frontend and `peerlume-daemon` must
be beside each other. The local Release launcher builds both executables:

```powershell
./scripts/run-client.ps1
./scripts/run-client.ps1 -Tui
./scripts/run-client.ps1 -ClientArguments @('--profile', 'C:\PeerLume profiles\test')
```

```bash
./scripts/run-client.sh
./scripts/run-client.sh --tui -- --profile '/home/user/PeerLume profiles/test'
```

`-Dev` / `--dev` selects the development build. `-NoBuild` / `--no-build` requires
the matching daemon to have already been built in the selected output directory.
The launcher preserves argument boundaries and the configured Cargo target path.

A frontend first tries to authenticate the existing owner. If there is no owner,
it may start the sibling daemon for that profile. Background startup hides its
window on Windows and detaches terminal input/output and the process session on
Linux. A locked legacy or incompatible owner is reported explicitly; startup
does not kill it, replace it, or start a second engine. Close the older client
normally before moving that profile to the new ownership model.

An existing profile's configuration and credential requirements still apply.
Daemon download directories must be absolute. A locked credential provider or
invalid state blocks startup instead of producing an empty successful session.
A new profile with no state/configuration uses an absolute `PROFILE/downloads`
default. Existing relative configuration is rejected without rewriting it;
existing state without its settings file also needs an explicit repair. The
`--check-config` daemon option validates without creating a profile, lock or engine.

On Linux, keep the absolute profile path short enough for the platform's Unix
socket path limit. Native and restricted control sockets live inside the profile
directory; an overlong path fails endpoint startup and does not fall back to an
embedded engine. This is a compatibility limit, not a verified Linux runtime
result for this candidate.

Restoration admits saved torrents in bounded batches after the native endpoint
is ready. Rows awaiting registration stay busy until the engine returns `Added`
or an error. A large profile does not overflow the startup command queue or
silently lose registrations. Existing torrent roots are pinned before changing
the default download directory. Saving a view changes persisted UI fields only,
preserving other saved preferences and any separate unsaved applied defaults.

## Shutdown and reconnect

Use an explicit owner shutdown when transfers should stop:

```powershell
./peerlume-daemon.exe --profile 'C:\PeerLume profiles\test' --stop
```

```bash
./peerlume-daemon --profile '/home/user/PeerLume profiles/test' --stop
```

This connects to the existing owner before reading configuration and does not
start a new one. It waits at most 95 seconds for profile-lock release, without
killing a process. The owner attempts final transfer detachment, actual-counter
checkpoints and journal barriers before releasing its lock. The CLI confirms the
accepted stop and lock release; it cannot infer every durability result from an
unavailable background exit status. The lifecycle regression independently
reopens SQLite and checks final counters. Journal manifests/status provide their
own completeness evidence. A foreground daemon also accepts Ctrl+C; Linux
service managers can send SIGTERM.

A disconnected interface retains the last known rows and shows the connection
failure. Those rows do not prove that the engine stopped. Queued UI actions mean
"awaiting owner" until the owner accepts them, and owner acceptance still does
not mean that the engine has acknowledged or durably saved the change. Failed
or stale commands are reported without silently replaying mutations. Owner
restart changes the instance identity and invalidates cached file details and
old command revisions.

After its window/terminal closes, a frontend cancels and drains its audit workers
for up to eight seconds, allows up to eight seconds for its bounded native action
queue to receive owner replies, and finishes audit writers under a five-second
deadline. These are sequential bounds, so a failed shutdown path can take up to
21 seconds plus ordinary scheduling overhead. Waiting happens after the visible
interface closes. All error results are retained; a timeout is not a claim of
completed work. An already blocked HTTP/filesystem call can outlive its deadline.

The native action drain waits for admission/replies; engine acknowledgement and
persistence can finish later in the owner. Audit workers record exchanges before
forwarding them to the display, so a closed view cannot silently discard an
accepted exchange. Exiting a frontend does not request owner shutdown.

## Trusted native boundary

Native GUI/TUI traffic uses a versioned, authenticated local channel separate
from the restricted agent/CLI/MCP protocol. On Windows, its named pipe permits
only the current user, rejects remote clients, and requires exclusive first
creation. The authentication descriptor protects its token with the platform
credential mechanism. On Linux, the socket and authentication descriptor are
private to the user. Frames, connections, queued actions and file pages have
explicit bounds.

Names, paths and ordinary native actions are available on this trusted channel.
This does not grant the external agent protocol native deletion or arbitrary
shell authority. Agent access remains disabled by default and keeps its existing
grants, approval and redaction rules. The daemon applies native mutations through
the same authoritative controller and preserves the confirmation required for
data recycling. Recycling first detaches the transfer, saves its stopped intent,
revalidates the confirmed paths, and only then recycles data and removes the row.
Failures retain visible recovery state instead of reporting successful removal.

Snapshot paging pins collection/command identity. Telemetry can advance between
pages; a paged snapshot is not a simultaneous cross-torrent network sample.
Conflicting structural changes require resynchronization. File details are
loaded off the UI thread and checked against owner identity, torrent identity
and the current model revision before being applied. Independent frontend detail
leases are bounded and expire after three seconds.

## Cold files and journals

Inactive models release file paths, tree rows and search buffers. The owner
retains compact numeric state for exact selection and accounting; a frontend
can retain only the summary until details are requested. A deferred empty path
list is never saved as an authoritative empty torrent. Path-required actions
wait for complete, current metadata. Detail pages contain at most 128 paths and
64 KiB of raw path text; full path hydration has a 64 MiB budget and reports
exhaustion explicitly.

Journals keep a bounded live view and use a bounded asynchronous writer. Queued,
written, flushed and durable sequence counters have different meanings. A clean
shutdown or complete export uses a barrier; admission or disk errors invalidate
any claim of a complete history. Default segments rotate at 32 MiB with an ordered
manifest and no automatic deletion of accepted history. The displayed first
JSONL segment is not necessarily the complete run. Use the export action for a
flat, validated history across all retained segments. Missing, altered or
interrupted segments are errors, not successful partial exports.

File-model capacity accounting excludes allocator overhead, engine metadata and
other processes. Process RSS may retain freed allocations. Sampled resource
results and platform limits belong in the release evidence; cold-model capacity
reduction alone is not a proof of an equal whole-process RAM reduction.
