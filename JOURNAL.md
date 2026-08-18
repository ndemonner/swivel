# JOURNAL

Newest entries at the bottom. Keep entries short. Record surprises.

## 2026-08-18 — Project start

- Did: wrote `DESIGN.md`, `ARCHITECTURE.md`, `LOOP.md`, `TODO.md`, `CLAUDE.md`.
- Did: verified every risky dependency by building and running a probe.
- Learned: `iroh` 1.0 renamed `NodeId` to `EndpointId` and `NodeAddr` to
  `EndpointAddr`. The `discovery` module is now `address_lookup`. Code written
  from memory against iroh 0.x will not compile.
- Learned: `iroh::endpoint::Connection` has real QUIC datagram support. The
  audio path does not need a stream.
- Learned: `audiopus_sys` builds libopus from source with cmake. A friend's
  machine needs no `brew install opus`.
- Learned: Opus at 48 kHz mono, 10 ms, 64 kbps produces 72 byte packets. The
  audio path is 64 kbit/s per peer per direction.
- Surprise: `cpal` 0.18 changed its API. `SampleRate` is now a plain `u32`.
  Device names come from `device.description()?.name()`.
- Surprise: CoreAudio on the test machine allows buffers down to 15 frames.
  The 256 frame default is conservative and can be lowered.
- Next: M0 and M1. Build the skeleton, then identity and storage.

## 2026-08-18 — M0, M1, M2, and the contact commands

- Did: T-001..T-004 skeleton, T-010..T-015 storage, T-020..T-030 network,
  T-100, T-101 contact commands, T-105 headless mode.
- Did: took the command line ahead of its milestone. The store was untestable
  by hand without it, and `walkie tui` is the only way to test two peers.
- Learned: two instances connect, hole punch, and settle on a direct path with
  0 ms round trip on loopback. iroh's relay fallback works and the roster
  reports `RLY` until the direct path takes over. That transition is visible
  and takes about 2 seconds.
- Surprise: `EndpointId::from_bytes` accepts most random byte patterns. Only
  some fail to decompress to a curve point. `[0xff; 32]` parses, `[2; 32]` does
  not. A test that needs an invalid key must use a probed value.
- Surprise: both sides dialling produced connection churn, not just one
  discarded connection. The side whose connection lost the tie-break redialled
  immediately and raced the replacement that was still arriving. A 300 ms
  settle delay before redialling removed it. See `config::RECONNECT_SETTLE`.
- Learned: refusing a knock before reading `Hello` leaves the user staring at a
  hex string with no way to decide. The accept path now reads one message from
  an unapproved endpoint, takes the claimed name, and then closes. The name is
  stripped of control characters and capped at 40 bytes.
- Learned: a refused caller retries on a backoff, so the knock notice must be
  printed once. `App::knock_is_known` guards it.
- Learned: `iroh::Watcher::get` needs `&mut self` and the trait must be in
  scope. `home_relay_status()` is the honest source for "am I online".
- Learned: `Connection::send_datagram` is not async. The encoder thread can
  call it directly and skip the tokio scheduler entirely. This removes a hop
  from the hot path. `Peer::connection` exists for that.
- Next: M3 audio. T-029 was released back to open because the drop-on-full
  behaviour belongs with the encoder thread in T-042.
