# Selective download

The desktop UI owns a `FileSelectionModel` that is independent from rendering.
The torrent parser will replace the current demo rows with paths and sizes from metainfo.

## Engine contract

1. The UI sends selected file indices to the torrent session.
2. The session maps each file byte range to the pieces that intersect it.
3. Pieces required only by unselected files receive the skip priority.
4. Pieces intersecting selected files remain downloadable and hash-verifiable.
5. Boundary pieces are downloaded in full even when some bytes belong to an
   unselected adjacent file. Only requested file ranges need to be persisted.
6. Changing selection recalculates priorities without restarting the torrent.

Planned priorities are `skip`, `normal`, and `high`. Directory rows can later expose
tri-state checkboxes while continuing to produce the same flat selected-index list.

## Current prototype

The application displays representative files, checkboxes, Select all / Select none,
selected file count and size, and the exact selected indices that will be passed to
the engine. Parsing a real `.torrent` and starting transfers are the next integration
steps.
