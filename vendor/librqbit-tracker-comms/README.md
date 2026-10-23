This package is a dependency of [rqbit](https://github.com/ikatson/rqbit) torrent client.
It can be used by itself too. See more [at the rqbit Github page](https://github.com/ikatson/rqbit).

## PeerLume local patch

Based on crates.io `librqbit-tracker-comms` 9.0.1, Apache-2.0, from
https://github.com/ikatson/rqbit. PeerLume changes are in `src/reporting.rs` and
the two stats reads in `src/tracker_comms.rs`. They add scoped per-torrent
outgoing metric policies while preserving the original HTTP/UDP peer stream.
The registry is keyed by both info hash and session peer ID and is populated
before the torrent is added, including its first announce.
