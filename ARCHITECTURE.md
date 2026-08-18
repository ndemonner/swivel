# walkie — Architecture

This document describes how the binary is built. Read DESIGN.md first.

All version numbers and API shapes here were checked against the real crates on
2026-08-18. Do not trust them after a dependency bump. Re-check the source in
`~/.cargo/registry/src/`.

## 1. Technology choices

| Concern | Choice | Version | Reason |
|---|---|---|---|
| Transport | `iroh` | 1.0 | QUIC with hole punching, relay fallback, and unreliable datagrams |
| Codec | `opus` (binds `audiopus_sys`) | 0.3 | Lowest latency codec with in-band FEC. Builds libopus from source. No system dependency. |
| Audio I/O | `cpal` | 0.18 | CoreAudio access with explicit buffer size control |
| Ring buffers | `ringbuf` | 0.5 | Lock-free SPSC for the real-time path |
| Storage | `rusqlite` (`bundled`) | 0.40 | Local state. Bundled SQLite means no system dependency. |
| User interface | `objc2`, `objc2-app-kit` | 0.6 / 0.3 | Direct AppKit access without a framework |
| Hotkeys | `global-hotkey` | 0.8 | Carbon `RegisterEventHotKey`. Needs no Accessibility permission. |
| Async runtime | `tokio` | 1 | Required by iroh |
| Wire format | `postcard` + `serde` | 1 | Compact, no schema file |

### 1.1 Verified facts

These were confirmed by building and running the probe, not by memory.

1. `iroh::endpoint::Connection` exposes `send_datagram(Bytes)`,
   `send_datagram_wait()`, `read_datagram()`, `max_datagram_size()`,
   `datagram_send_buffer_space()`, `rtt(PathId)`, and `remote_id()`.
2. `iroh` 1.0 renamed the old names. `NodeId` is now `EndpointId`. `NodeAddr` is
   now `EndpointAddr`. `discovery` is now `address_lookup`.
3. `impl From<EndpointId> for EndpointAddr` exists. You can dial with only a
   public key. DNS address lookup resolves the rest.
4. `Endpoint::builder(presets::N0)` sets the n0 relays, the DNS address lookup,
   and the TLS crypto provider.
5. `QuicTransportConfig::builder()` exposes `datagram_send_buffer_size`,
   `datagram_receive_buffer_size`, `initial_rtt`, `ack_frequency_config`,
   `keep_alive_interval`, `max_idle_timeout`, and `initial_mtu`.
6. Opus at 48 kHz mono, 10 ms frames, 64 kbps produces **72 byte** packets.
7. `cpal` 0.18 uses `SampleRate = u32` and `device.description()?.name()`.
8. macOS CoreAudio reports a buffer size range of 15 to 4096 frames on the test
   machine. Devices are stereo. Input must be downmixed. Output must be upmixed.

## 2. Process and thread model

There is one process. It has four kinds of thread.

```
 ┌──────────────────────────────────────────────────────────────┐
 │ main thread — AppKit run loop                                │
 │   NSApplication (.accessory), NSStatusItem, NSPanel          │
 │   global-hotkey Carbon handler                               │
 │   reads UI state snapshots, never blocks                     │
 └──────────────────────────────────────────────────────────────┘
              ▲ state snapshot (arc-swap)      ▼ commands (channel)
 ┌──────────────────────────────────────────────────────────────┐
 │ tokio runtime — 2 worker threads                             │
 │   iroh Endpoint, accept loop                                 │
 │   one supervisor task per contact                            │
 │   one control-stream task per connection                     │
 │   one datagram reader task per connection                    │
 │   session manager                                            │
 └──────────────────────────────────────────────────────────────┘
      ▲ encoded packets              ▼ encoded packets
 ┌────────────────────────┐   ┌──────────────────────────────────┐
 │ encoder thread         │   │ CoreAudio output callback (RT)   │
 │   normal priority      │   │   pops, decodes, mixes, writes   │
 └────────────────────────┘   └──────────────────────────────────┘
      ▲ f32 frames                    (no allocation, no locks)
 ┌────────────────────────┐
 │ CoreAudio input        │
 │ callback (RT)          │
 └────────────────────────┘
```

### 2.1 Real-time rules

The CoreAudio callbacks are real-time threads. Inside them the code must not:

1. Allocate or free memory.
2. Take a lock that a non-real-time thread can hold.
3. Perform any input or output.
4. Log through a normal logger.

Every buffer, decoder, and slot is allocated at start up. See §5.3.

## 3. Module layout

```
src/
  main.rs           argument parsing, run mode selection
  app.rs            wiring, shared state, shutdown
  config.rs         constants and tunables
  store/
    mod.rs          SQLite open, migrate
    identity.rs     keypair load and create
    contacts.rs     contact CRUD, slot assignment
  net/
    mod.rs          endpoint build, accept loop
    ticket.rs       wt1 ticket encode and decode
    peer.rs         per-contact supervisor and reconnect
    control.rs      control stream protocol
    audio_wire.rs   audio datagram header
  audio/
    mod.rs          device open, stream lifecycle
    capture.rs      input callback, downmix, ring
    encode.rs       encoder thread
    playback.rs     output callback, decode and mix
    jitter.rs       per-peer adaptive jitter buffer
    chirp.rs        local confirmation tones
  session/
    mod.rs          session state machine, member mesh
  ui/
    mod.rs          NSApplication setup
    statusitem.rs   menu bar item and menu
    panel.rs        the floating roster panel
    roster_view.rs  custom drawn roster
    style.rs        colours, fonts, drawing helpers
    hotkey.rs       global hotkey registration and digit capture
  cli/
    mod.rs          key, add, list, rm, slot, doctor
```

## 4. Network layer

### 4.1 Endpoint

```rust
let ep = Endpoint::builder(presets::N0)
    .secret_key(secret_key)
    .alpns(vec![ALPN.to_vec()])
    .transport_config(transport_config())
    .bind()
    .await?;
```

`ALPN` is `b"walkie/0"`.

### 4.2 Transport tuning

```rust
QuicTransportConfig::builder()
    // Drop old audio instead of queuing it. This is the anti-bufferbloat rule.
    .datagram_send_buffer_size(8 * 1024)
    .datagram_receive_buffer_size(Some(64 * 1024))
    // Do not wait for the default 500 ms probe before the first packet.
    .initial_rtt(Duration::from_millis(30))
    .keep_alive_interval(Duration::from_secs(5))
    .max_idle_timeout(Some(Duration::from_secs(20).try_into()?))
    // Audio is small. Do not pay for MTU probing on start.
    .initial_mtu(1200)
    .build()
```

The send buffer is deliberately small. When congestion appears, `send_datagram`
returns an error. The encoder then drops the frame. A dropped frame costs 10 ms
of audio. A queued frame costs unbounded latency for the rest of the session.

### 4.3 Connection lifecycle

One supervisor task runs per contact for the life of the process.

```
 connect ──ok──> register ──> run(control, datagrams) ──err──> backoff ──┐
    ▲                                                                    │
    └────────────────────────────────────────────────────────────────────┘
```

Backoff is 1 s, 2 s, 5 s, 15 s, then 30 s, each with up to 20 percent jitter.

Both sides dial each other. This produces two connections. The tie-break is
deterministic. Keep the connection whose **dialer has the smaller `EndpointId`**
in byte order. Close the other with code `1` and reason `"duplicate"`.

Presence is derived. A registered connection means online. Nothing else.

### 4.4 Control protocol

The control channel is one bidirectional QUIC stream per connection. The dialer
opens it. Messages are `postcard` encoded and prefixed with a `u16` length.

```rust
enum Control {
    Hello { name: String, version: u16 },
    Presence { available: bool, dnd: bool, muted: bool },
    SessionOpen { session: u64, members: Vec<EndpointId> },
    SessionClose { session: u64 },
    TalkState { session: u64, speaking: bool },
    Ping { nonce: u64 },
    Pong { nonce: u64 },
}
```

`Ping` runs every 2 seconds. It gives an application-level round trip time. This
is more honest than the QUIC estimate because it includes scheduling delay.

### 4.5 Audio datagrams

Audio never uses a stream. Streams retransmit and block on order. Audio uses
`Connection::send_datagram`.

Header, 8 bytes, little endian:

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | version 4 bits, type 4 bits. Type 1 is audio. |
| 1 | 1 | flags. Bit 0 marks the start of a talkspurt. |
| 2 | 2 | sequence number, wrapping `u16` |
| 4 | 4 | timestamp in samples at 48 kHz, wrapping `u32` |

The Opus payload follows. A typical packet is 8 + 72 = 80 bytes. At 100 packets
per second this is 64 kbit/s of payload per peer, per direction.

## 5. Audio pipeline

### 5.1 Format

Everything between the codec and the wire runs at 48 kHz. Devices often do not.
A Bluetooth headset commonly runs at 44.1 kHz, and that is the default for most
people, not an edge case.

A device at another rate is converted in `audio/resample.rs`, inside the
callback, with a Catmull-Rom cubic. `rubato` would give better stopband
rejection, but this converter has three properties that matter more:

1. It allocates nothing, so it obeys §2.1.
2. It costs a handful of multiplies per sample.
3. It works one sample at a time, so it adds no block of latency.

A device already at 48 kHz gets a passthrough and pays nothing. `walkie doctor`
reports the rate either way.

**This was a real defect, not a hypothetical.** Before the converter existed,
playback on 44.1 kHz headphones ran at 44100/48000 of the correct speed. The
symptom was subtle: audio worked, sounded almost right, and the `played` counter
fell behind `sent` by a steady 8.2 percent. Nothing errored. See `JOURNAL.md`.

Audio is mono on the wire. The capture path downmixes. The playback path copies
mono to every output channel.

### 5.2 Capture and encode

1. CoreAudio calls the input callback with `N` interleaved `f32` frames.
2. The callback downmixes to mono and pushes into an SPSC ring.
3. The callback does nothing else.
4. The encoder thread pops exactly 480 samples, which is 10 ms.
5. The encoder thread encodes once into a preallocated buffer.
6. The encoder thread sends the same bytes to every member of the session.

One encode serves all members. Encoding once for `n` peers is the reason the
encoder is a separate thread and not part of the network task.

Encoder settings:

```rust
Encoder::new(48000, Channels::Mono, Application::Audio)
enc.set_bitrate(Bitrate::Bits(64_000))
enc.set_inband_fec(true)
enc.set_packet_loss_perc(10)
enc.set_complexity(8)
```

`Application::Audio` is chosen over `Voip`. `Voip` applies speech shaping. The
product goal is presence, not intelligibility at low bitrate.

### 5.3 Playback, decode, and mix

Decode and mix happen **inside** the output callback. This removes a thread hop
and one buffer of latency. An Opus decode of a 10 ms frame costs about 50 µs.

To keep this real-time safe there is a fixed array of `MAX_PEERS = 8` slots.
Every slot is allocated at start up and holds:

1. One `opus::Decoder`.
2. One SPSC consumer of encoded packets, with fixed-size preallocated packet
   slots.
3. One jitter buffer state.
4. One `AtomicBool` for active.

Adding a peer sets a flag. It never allocates. Removing a peer clears the flag
and drains the queue from the network side.

The callback:

1. For each active slot, decide which packet is due, using §5.4.
2. Decode it, or run concealment if it is missing.
3. Sum into the mix buffer with per-peer gain.
4. Apply a soft limiter.
5. Write mono to every output channel.

### 5.4 Jitter buffer

Each peer has an independent adaptive buffer.

1. Track the arrival time of each sequence number.
2. Compute the jitter estimate as a running p95 of the inter-arrival deviation.
3. Set the target depth to `clamp(ceil(jitter / 10 ms) + 1, 1, 6)` frames.
4. Grow the depth immediately when a late packet arrives.
5. Shrink the depth only after 5 seconds of stability. Shrink by one frame.
6. Shrink by dropping a frame during a silence gap, never mid-word.

Missing packets are handled in this order:

1. If the next packet is present and carries FEC, decode the lost frame from it.
   Call `decode` with `fec = true`.
2. Otherwise call `decode` with an empty slice for packet loss concealment.
3. After 5 consecutive lost frames, fade to silence.

### 5.5 Echo

Version 1 assumes headphones. There is no acoustic echo canceller.

If the output device is a speaker, `walkie doctor` prints a warning. A future
version may add `webrtc-audio-processing`. That is tracked in TODO.md. It is not
in scope for version 1 because AEC3 adds buffering and build complexity.

## 6. Session model

### 6.1 State

```rust
struct Session {
    id: u64,
    members: BTreeSet<EndpointId>,
    started: Instant,
    last_voice: Instant,
}
```

There is at most one local session at a time. Version 1 does not support two
independent conversations.

### 6.2 Opening

The microphone opens here and nowhere else. Two paths reach it: the user presses
a digit, or a contact sends `SessionOpen`. Opening the panel does not open the
microphone.

1. The user presses a digit for slot `n`.
2. The session manager resolves slot `n` to an `EndpointId`.
3. If there is no session, it creates one with `id = random u64`.
4. It adds the contact to `members`.
5. It sends `SessionOpen { id, members }` to every member, including the ones
   already in the session.
6. It marks the peer slot active in the audio engine.
7. It plays the open chirp, if this is the first member.

Step 5 is what makes the conversation a mesh. Every member learns the full list
and opens its own microphone to every other member.

### 6.3 Joining as a member

On `SessionOpen`:

1. If do-not-disturb is on, reply with `SessionClose` and stop.
2. If the sender is not an approved contact, ignore it.
3. For each member that is not you, mark the peer slot active. If there is no
   warm connection to that member, dial it now.
4. Play the open chirp, if you were not already in a session.

### 6.4 Limits

`MAX_PEERS` is 8. A mesh sends `n-1` streams up and receives `n-1` down. At 8
members this is 7 × 64 kbit/s = 448 kbit/s up. That is the practical limit for a
home connection.

### 6.5 Closing

`⌃⌥⌘Esc` sends `SessionClose` to every member and clears local state. A member
that receives `SessionClose` removes the sender. When the member set is empty
the session ends and the close chirp plays.

The 10 minute idle timer calls the same path.

## 7. Latency budget

Measured components, at the default settings, on the test machine.

| Stage | Cost |
|---|---|
| Input device buffer, 256 frames | 5.3 ms |
| Opus frame accumulation | 10.0 ms |
| Opus encode, complexity 8 | 0.3 ms |
| Network, one way | RTT / 2 |
| Jitter buffer, target 2 frames | 20.0 ms |
| Opus decode | 0.1 ms |
| Output device buffer, 256 frames | 5.3 ms |
| **Fixed total** | **41 ms** |

Add the one-way network time. A LAN adds about 1 ms. A same-city path adds about
8 ms. A US coast-to-coast direct path adds about 35 ms.

A relayed path adds the trip to the relay and back. `walkie` shows `RLY` in the
roster when this happens, because it is the single biggest latency risk.

### 7.1 Reducing the budget

The following are configurable. Each trades safety for latency.

1. `buffer_frames = 128` saves 5.3 ms. It risks dropouts under CPU load.
2. `frame_ms = 5` saves 5 ms. It raises the packet rate to 200 per second and
   lowers codec efficiency.
3. `jitter_min = 1` saves 10 ms. It only works on a stable LAN.

`walkie doctor --tune` measures the link and suggests values.

## 8. Storage

SQLite lives at `~/Library/Application Support/dev.motor.walkie/walkie.db`. The
file mode is `0600`.

```sql
CREATE TABLE identity (
  id          INTEGER PRIMARY KEY CHECK (id = 1),
  secret_key  BLOB    NOT NULL,
  name        TEXT    NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE contacts (
  endpoint_id TEXT    PRIMARY KEY,
  slot        INTEGER UNIQUE,
  name        TEXT    NOT NULL,
  auto_open   INTEGER NOT NULL DEFAULT 1,
  added_at    INTEGER NOT NULL,
  last_seen   INTEGER
);

CREATE TABLE knocks (
  endpoint_id TEXT    PRIMARY KEY,
  claimed     TEXT,
  first_seen  INTEGER NOT NULL,
  blocked     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

Migrations use `PRAGMA user_version`. Each migration is a numbered function in
`store/mod.rs`.

## 9. macOS integration

### 9.1 One binary, no bundle

The product is a single executable. It is not an `.app`. Two problems follow.

**Problem 1. The microphone permission needs an `Info.plist` key.**

Solution: embed a plist in the binary through the linker. `build.rs` emits:

```
cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,<abs path>/Info.plist
```

The plist sets `NSMicrophoneUsageDescription` and `LSUIElement`.

**Problem 2. TCC identifies an unsigned binary by path and it re-prompts.**

Solution: ad-hoc sign after build. `scripts/build-release.sh` runs
`codesign -s - --force <binary>`. The user grants the microphone once per build.
A stable self-signed certificate would remove the repeat prompt. That is tracked
in TODO.md.

### 9.2 AppKit without a bundle

`NSApplication` works from a plain executable. Set the activation policy to
`NSApplicationActivationPolicyAccessory`. This gives a menu bar item with no
Dock icon.

All AppKit calls must happen on the main thread. The tokio runtime therefore
starts on a spawned thread and the AppKit run loop owns `main`.

### 9.3 UI state transfer

The user interface never reads network or audio state directly.

1. The core publishes an immutable `UiState` snapshot through `arc-swap`.
2. A `CFRunLoopTimer` at 10 Hz reads the snapshot and marks views dirty.
3. Real-time transitions, such as a session opening, also post a
   `performSelectorOnMainThread` wake so they are not delayed by the timer.

The user interface sends commands the other way through an unbounded channel.

### 9.4 Hotkeys and digit capture

`global-hotkey` registers `⌃⌥⌘T`, `⌃⌥⌘Esc`, and `⌃⌥⌘M` through Carbon. Carbon
hotkeys need no Accessibility permission and they consume the keystroke.

Digits are **not** global hotkeys. When `⌃⌥⌘T` fires, the panel is shown and
made the key window. Digits arrive as ordinary `keyDown:` events on the panel's
view. This avoids registering ten global hotkeys and avoids stealing digits from
other applications.

## 10. Testing

1. **Unit.** Ticket round trip, slot assignment, jitter buffer decisions, wire
   header encode and decode. These run in CI without hardware.
2. **Loopback latency.** `walkie doctor --loopback` plays a click, records it,
   and measures the round trip through the real device stack.
3. **Two-process local.** `WALKIE_DB=/tmp/a.db walkie` and a second instance
   with a different database. They connect over the loopback path. This is the
   main integration test.
4. **Network shaping.** Use `dnctl` and `pfctl` to add delay and loss. Confirm
   the jitter buffer adapts and does not grow without bound.

## 11. Risks

| Risk | Impact | Response |
|---|---|---|
| Hot microphone surprises a user | Trust loss | Chirps, menu bar state, mute, DND |
| Relay path used instead of direct | Latency doubles | Show `RLY`, log path events |
| Echo without headphones | Unusable on speakers | Warn in doctor, AEC later |
| `audiopus_sys` fails to build on a friend's machine | No install | Ship a prebuilt binary, not source |
| Jitter buffer grows and never shrinks | Latency creep | Explicit shrink rule, §5.4 |
| AppKit call from a non-main thread | Crash | All UI behind a main-thread queue |
