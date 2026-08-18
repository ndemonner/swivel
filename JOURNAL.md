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
