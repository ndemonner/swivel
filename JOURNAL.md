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

## 2026-08-18 — M3 audio engine

- Did: T-040..T-047, T-029, T-102. Capture, encode, jitter buffer, decode, mix,
  limiter, fault tone, and `walkie doctor`.
- Measured on this machine: Opus encode is **0.05 ms** per 10 ms frame at
  complexity 8, against a 10 ms budget. The packet is 81 bytes. The fixed
  latency total is **41.1 ms**, which matches ARCHITECTURE.md §7 exactly.
- Decision: the Opus encode runs **inside** the input callback. libopus
  allocates its state once at creation, so it obeys the real-time rules, and
  encoding there removes a thread hand-off worth hundreds of microseconds. The
  callback must not send, so encoded frames go to a lock-free queue and a normal
  thread does the network work.
- Decision: the **speaker stays open for the life of the process, and the
  microphone opens on demand**. Two reasons. An intercom must be reachable at
  any moment, so the output can never close. And if the input ran all the time,
  the macOS microphone indicator would be permanently lit and would stop
  telling the user anything.
- Decision: **no tone when the microphone opens or closes.** The user asked for
  it to be seamless. The menu bar state and mute carry that job. Only a device
  fault makes a sound, because that is the case the interface cannot cover.
- Surprise: a `tanh` limiter with a ceiling of 1.0 reaches exactly 1.0 in `f32`
  for a loud input, which still clips once a device adds gain. The ceiling is
  now 0.98.
- Surprise: CoreAudio reports "External Headphones" as `DeviceType::Unknown`.
  Treating Unknown as a loudspeaker made `doctor` warn about echo on a correct
  setup. A warning that fires on the right answer teaches users to ignore
  warnings, so only a real `Speaker` is reported now.
- Surprise: the first `doctor` run captured 0 frames. That was the macOS
  permission prompt, not a bug. The second run captured 40 frames in 400 ms,
  which is exactly the expected 100 frames per second.
- Learned: `ringbuf` 0.5 has no `push_overwrite` on a split producer. The
  hand-off queue therefore drops the newest packet when full. That is
  acceptable because the queue holds 320 ms and the callback empties it every
  few milliseconds, so it can only fill when the device has already stopped.
  The jitter buffer, not this queue, decides what is too late.
- Added: T-050 and T-051 for choosing devices. The user asked for it. System
  defaults work today.
- Next: M4 sessions, so two instances can actually talk.

## 2026-08-18 — M4 sessions. Two instances talk.

- Did: T-060..T-064. Session state, mesh fan-out, teardown, idle timer, mute,
  do-not-disturb, and the speaking indicator.
- **Verified end to end.** Two processes on one machine, one presses `2`, and
  both microphones open with no answering step. Counters after ten seconds:
  `encoded 829, sent 829, refused 0, played 828, concealed 0, late 0,
  overrun 0`, in both directions. The single frame between sent and played is
  the jitter buffer holding one frame, which is exactly right.
- Learned: `App::start` used to take an `AudioSink`. It now builds the engine
  itself, because the engine is both the sink and the thing that needs the
  transmitter. A machine with no usable audio still gets a working roster,
  since seeing who is online is useful on its own.
- Surprise: calling `driver::supervise` from `on_session_open` made rustc fail
  with a type cycle. A supervisor runs a connection, a connection carries a
  `SessionOpen`, and that dials a new member who needs a supervisor, so the
  opaque future type referred to itself. Boxing did not help, because the
  compiler still had to prove `Send` on the inner type. The fix is a channel:
  the control path asks for a supervisor and one task starts it. That is better
  layering as well as a working build.
- Learned: `Connection::send_datagram` is synchronous, so the audio sender
  thread needs no runtime. The member connections are published through
  `ArcSwap`, so reading them is a pointer swap with no lock.
- Added: a `played` counter. Every other counter reports a failure, and absence
  of failure is not proof that audio arrived. `played` is the positive signal.
- Testing note: driving `walkie tui` through a fifo needs a writer held open,
  for example `sleep 900 > /tmp/wa.in &`. Without it the first `echo` closes
  the pipe, standard input reaches end of file, and the instance shuts down.
- Next: M5, the menu bar and the panel.
