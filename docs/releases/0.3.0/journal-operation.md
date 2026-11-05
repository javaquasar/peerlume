# Asynchronous journal operation

Target: 0.3.0. The announce JSONL schema and meaning of `run_id`, `sequence`,
`actual`, `sent` and tracker replies remain unchanged. Workload exports still
produce one complete, flat `announces.jsonl`; rotation does not replace that
artifact with a partial live tail.

## Admission and evidence states

The observer assigns each sequence in producer order. It shares an immutable
record with a bounded live tail and attempts nonblocking writer admission.
Serialization and steady-state disk writes happen on the writer thread, outside
the live-view mutex. Contending producers can briefly serialize admission, but a
full queue never waits indefinitely for disk I/O on a Tokio worker.

`JournalStatus` distinguishes these watermarks:

- `accepted_sequence`: admission succeeded for this ordered prefix.
- `written_sequence`: serialization reached the buffered writer.
- `flushed_sequence`: that prefix reached the operating system through `flush`.
- `durable_sequence`: a successful `sync_all` barrier covers the prefix.

An OS flush alone is not a power-loss durability guarantee. Explicit flush
barriers, rotation and shutdown synchronize files and publish the manifest.
A timed-out barrier is an error; callers may request a later fence and must
receive its acknowledgement before claiming success. A queued record is not yet
durable. The live tail can contain a rejected observation, so live visibility is
also not proof that a record was accepted onto disk.

Default admission bounds are 2,048 pending records and 8 MiB of accounted pending
record memory. There can additionally be one in-flight admitted record (at most
8 MiB of accounted retained data) and its serialized buffer (at most 32 MiB).
The independent live tail has its own 8 MiB budget. These component
bounds exclude allocator overhead, reader buffers, the OS page cache and other
application state; they are not a process RSS cap. Audit histories in one active
profile share a writer instead of creating a thread for each torrent.

The standalone-audit process registry strongly retains writers until explicit
frontend shutdown, so closing every display clone cannot discard an accepted
queue or hide a final write error. The registry permits at most eight distinct
directory slots, counting successful writers and failed-open records together.
Further directories fail admission explicitly; existing history is retained.
Initial-open failures are also returned by the process shutdown barrier.

Queue or size rejection is sticky and visible: new disk admissions stop, rejected
records are counted, and complete export fails. The already accepted prefix still
drains. Actual serialization/write failure also stops the writer and records an
incomplete history. No retention setting silently drops an accepted prefix to
pretend that complete history remains available.

## Rotation, recovery and export

The default segment target is 32 MiB. A line is never split; a smaller custom
target can be exceeded by one bounded line. A run supports at most 16,384 ordered
segments, with an 8 MiB manifest bound. Reaching a bound is an explicit error.
Default retention deletes no segment.

The manifest records kind, run identity, ordered file names, sequence/byte ranges,
clean shutdown and any incomplete/error state. `path()` identifies the first
segment for display and is not the complete run after rotation. Consumers needing
full history use `export`/the owner export job, which validates ordered segments
and streams one flat JSONL file. Owner export has a 512 MiB bound and reports
oversize as an error. Its disk fence/materialization runs on an export worker,
outside the controller lock.

Reopen accepts only a clean, complete manifest. It rejects unsafe names,
symlinks, missing files, length changes to closed segments, oversized or partial
lines, incompatible run/schema identity and sequence gaps. An active last segment
may grow beyond an earlier fence; closed segments must match their exact length.
Interrupted or incomplete runs remain evidence of failure rather than being
silently repaired into successful history.

The native frontend stores a bounded read-only projection with the owner's
sequence/run identity. It cannot append a local replacement journal or export its
tail as full history. Export is routed to the authoritative owner.

## Shutdown and limitations

Shutdown closes admission, drains the accepted queue, synchronizes the final
segment/manifest and acknowledges completion. The controller keeps profile
ownership through final session counters, checkpoints and journal barriers.
Failure remains visible and must not be described as clean shutdown. The worker
has a bounded completion wait; an underlying filesystem call can outlive that
wait, which is reported instead of joining a stuck writer indefinitely.

Standalone-audit workers persist each exchange independently of the UI receiver;
the receiver adds only a live view, avoiding duplicate disk entries. Frontend exit
cancels and drains those workers before the process audit-writer barrier. Native
queued actions have a separate drain. Current exit budgets are eight seconds for
audit workers, eight for native actions, then five for all audit writers under
one shared deadline: at most 21 seconds of these configured waits. No live-view
mutex is held while the writer barrier waits. Registry acquisition itself also
has a deadline, and closure permanently rejects new process admissions.

A controlled HTTP request can outlast the worker-drain budget. In that case exit
returns an error, still attempts to seal the accepted prefix, and does not claim
a complete audit. Late appends are rejected visibly rather than being silently
treated as durable. A successful barrier covers accepted records; it cannot
prove a reply was captured if its producer never completed within the budget.

Tests inject slow writers, overflow, partial writes, barrier timeout/retry,
concurrent records, rotation, clean reopen, missing/tampered segments and an
interrupted final line. They verify causal evidence and bounds. They do not prove
absence of every filesystem failure or replace a long resource soak.
