# Runtime controls, completion and bounded repair

These controls belong to each torrent. Both egui and ratatui use the same command,
event reducer, persisted configuration and status-card model. They do not change
the standalone tracker-audit scenario editor.

## Showing a file in its folder

In egui, right-click a file in **Files** and choose **Show in folder**, or
right-click a torrent row and choose **Open .torrent location** for its source.
Both actions pass the complete file path and request selection, not merely an
open parent directory. **Open download folder** remains a folder-only action.

Windows uses the Shell item identity and `SHOpenFolderAndSelectItems`, without
Explorer command-line parsing. Linux uses the desktop session bus and
`org.freedesktop.FileManager1.ShowItems` with an encoded file URI (supported by
Nautilus, Dolphin and compatible file managers). A missing session bus, unavailable
interface, rejected request or five-second method timeout is reported as a
selection error; it is not silently reported as success after opening only a
folder. A successful API reply acknowledges the request, not a visual inspection
of the file-manager window.

If a payload file does not exist yet, only the nearest existing directory inside
its configured download root is opened, and the status explicitly says that no
file could be selected. Navigation does not create payload files, change file
selection or send torrent commands. Desktop requests run on a separate worker;
at most one is outstanding, so a slow file manager does not block the GUI or
controller owner.

## Changing an existing download mode

In egui, select the torrent, open **Torrent settings**, select a download mode and
press **Apply download mode**. Freeze mode also requires a positive byte threshold.
In ratatui, press **M** and enter a mode number and optional threshold, for example
`5 1048576`. Numbers are 1 normal, 2 no progress, 3 random upload multiplier, 4 hide downloaded,
and 5 freeze. Existing-torrent numbers always follow this order, independently of
the external add-dialog mode list.

The upload-multiplier mode exposes **Minimum ×** and **Maximum ×** in the add
dialog and Torrent settings. Defaults are **1.5–2.5**. Ratatui asks for the range
when adding; for an existing torrent, **M** accepts `3 1.5 2.5`. Enter equal bounds
(for example `3 1.75 1.75`) for a fixed multiplier. Bounds are positive, ordered,
and support three decimal places. They are saved only after engine confirmation
and restored after restart; old records/commands use the 1.5–2.5 default.

A new multiplier is sampled uniformly from the inclusive range for **every real
announce attempt**, including retries and stopped/completed requests. The sent
upload is `floor(actual_downloaded * multiplier)`, saturating at `u64::MAX`.
Downloaded/left and all local transfer counters stay actual. A smaller sampled
multiplier can decrease reported uploaded bytes even as downloads grow; the mode
does not silently clamp the counter or cache one multiplier for the torrent.
This mode therefore does not guarantee monotonic tracker-upload counters.

CLI/MCP `set_mode` optionally accepts `upload_ratio` with integer `min_milli` and
`max_milli` fields: 1500 means ×1.5 and 2500 means ×2.5. The legacy mode identifier
`upload_ratio_1_5` remains unchanged for compatibility.

Changing the dropdown edits a draft, not the saved configuration. The UI displays:

- **Awaiting engine** after loading a saved torrent, until registration succeeds.
- **Applying**, with the requested mode and last confirmed active mode.
- **Active** only after the engine confirms the policy. Only then is it saved.
- **Failed**, retaining the previous saved/confirmed mode and an error message.

The engine validates the request, briefly pauses a running transport, replaces its
session-scoped policy and resumes it. A paused torrent stays paused. During a disk
check, the change is applied after initialization and before the check resumes
peer traffic. Resuming while a mode request is pending waits for its application.
Unresolved metadata can delay application. Files, file selection and cumulative
actual network counters are retained. If policy application succeeds but resuming
fails, the policy is still reported as active and a separate transfer error appears.

This is an application-policy confirmation, not an acknowledgement from the
tracker. Previously transmitted/in-flight requests cannot be recalled and may
still appear with the old mode in the journal. The Status tab therefore reports
the active policy separately from the mode of the last observed real announce.
When tracker communication is disabled, the policy acknowledgement says so.

Freeze snapshots survive pause, recheck and repair. Explicitly applying a mode
(even the same freeze mode) replaces the policy and clears its snapshot. Restarting
the application also creates a new snapshot; the byte threshold itself is saved.

## Forced integrity check and repair

In egui, right-click a torrent name and choose **Force recheck and repair**.
The action targets that row, even when another torrent is selected. It is disabled
while an integrity check is already running.

This is an explicit, one-shot request to read and hash the selected payload,
resume the torrent (including a previously paused torrent), download missing or
corrupt required pieces, and hash the files again before showing **Done**. Valid
pieces are not downloaded again. A missing source `.torrent` file is not required
once the engine has resolved its metadata. The same engine, file selection,
download directory, tracker URLs, reporting mode, freeze snapshot and cumulative
network counters are retained.

The action works even when automatic repair is disabled. It does not change the
saved automatic-repair or post-download-verification settings. The existing repair
attempt limit still applies (3 by default), and the normal stall timeout and
storage-error handling remain in force. A user pause prevents automatic
continuation; resuming an ongoing repair retains its consumed attempts. Plain
**Recheck files** in the Log tab keeps its existing pause/policy semantics.

The shared agent mutation command `{"kind":"recheck_and_repair"}` uses the same
path and requires the management grant. Acceptance is not completion: the
operation waits for a fresh check and actual verification, or reports failure.

## Automatic repair

Automatic repair is **off by default**. Enable it when adding a torrent or in its
settings. The limit defaults to **3 attempts** and is configurable from **1 to 10**.
In ratatui use **F4** in the add dialog, **H** on an existing torrent to toggle, and
**L** to set its limit. Enabling repair also enables the full post-download check;
that check cannot be disabled while repair is enabled. Disabling repair leaves
post-download verification enabled until explicitly changed.

After a completed active download fails a full disk hash check, the engine resumes
the checked torrent, whose piece map now identifies the missing/corrupt pieces.
Only missing required pieces are requested; already valid pieces are retained.
The existing reporting policy remains installed throughout repair. Checked disk
bytes are not counted as newly received network bytes.

Each recovery download consumes one attempt. After completion, a fresh full disk
check is mandatory before **Done**. If no additional verified payload appears for
120 seconds during an active repair, the engine pauses and checks again; another
attempt is allowed only within the remaining budget. User-paused time does not
consume the stall timeout. Read/storage errors stop the check instead of blindly
starting downloads. The UI shows downloading/checking, attempt/limit, success or
exhaustion. Exhaustion leaves the torrent stopped with a verification failure; it
does not delete the payload or retry forever.

A torrent checked while user-paused stays paused even if corruption is detected.
Pausing during a check suppresses automatic continuation. Explicit **Resume** after
a failed check starts a new user-requested recovery episode and budget. Pausing
and resuming an ongoing episode retains its consumed attempts. Changing the mode
does not reset the repair budget. Settings survive restart; an in-flight repair
budget and completion confirmation do not. Restored files are checked afresh.

## Shared Status tab

The Status tab in both interfaces shows the saved mode, engine-confirmed mode,
application phase/error, transfer/completion progress, session network totals,
verified selected payload and repair status. Its last-announce section shows the
actual and sent uploaded/downloaded/left values, event, outcome, timestamp and
mode of the latest real exchange. No observed exchange is clearly distinguished
from an exchange with zero counters. Tracker credentials, userinfo, path and query
are always hidden in this card. In ratatui use up/down to scroll the card.

The existing Log tab retains detailed replies, received peers and JSONL export
for long-running tests. Session network totals are not lifetime account statistics.

## Regression coverage

Tests exercise all 25 mode-to-mode transitions on a paused real engine, invalid
threshold rejection, unchanged payload and pause state, active transport changes
observed by a local HTTP tracker, and retained actual network totals. A local peer
fixture repairs one damaged piece while proving that the healthy piece is not
requested and no-progress metrics remain in force. Other tests cover user pause,
bounded attempts/stall timing, registry session isolation and snapshot replacement,
SQLite migration/round trips, engine acknowledgement before persistence, restored
unconfirmed mode, shared frontend rendering and tracker credential redaction.
All network fixtures are loopback-only; no private production tracker is contacted.
