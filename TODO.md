# swivel — Task List

This file is the single source of truth for work in progress. Read LOOP.md
before you touch it.

## Status keys

| Key | Meaning |
|---|---|
| `[ ]` | Open. Anyone may claim it. |
| `[~]` | Claimed. The branch name follows the title. |
| `[x]` | Done and merged to `main`. |
| `[!]` | Blocked. The reason follows the title. |

## Rules

1. Claim a task by changing `[ ]` to `[~]` and adding your branch name.
2. Commit that claim to `main` before you start work.
3. Do not claim a task whose dependencies are not `[x]`.
4. Mark `[x]` in the same commit that completes the work.
5. Add new tasks at the end of the right milestone. Never renumber.

---

## M0 — Skeleton

- [x] **T-001** Cargo project, layout, `.gitignore`, `rust-toolchain.toml`
  - Accept: the binary is named `swivel`. Accept: edition 2024.
- [x] **T-002** Pin and verify dependencies
  - Accept: `cargo build` succeeds with no system package installed.
  - Accept: libopus compiles from source through `audiopus_sys`.
- [x] **T-003** `config.rs` with every tunable constant in one place
  - Accept: sample rate, frame size, buffer frames, bitrate, jitter bounds,
    `MAX_PEERS`, ALPN, backoff table.
- [x] **T-004** Error type and logging setup
  - Accept: `tracing` to stderr, `SWIVEL_LOG` env filter, no logging in
    real-time callbacks.

## M1 — Identity and storage

- [x] **T-010** SQLite open, path resolution, `0600` mode, `PRAGMA user_version`
  - Accept: `SWIVEL_DB` overrides the path. Needed for two-process tests.
- [x] **T-011** Migration 1: the four tables in ARCHITECTURE.md §8
- [x] **T-012** Identity load or create. Store the iroh `SecretKey`.
  - Accept: the same key survives a restart.
- [x] **T-013** `sv1` ticket encode and decode
  - Accept: round trip test. Accept: a corrupt ticket gives a clear error.
- [x] **T-014** Contact CRUD and slot assignment
  - Accept: a new contact takes the lowest free slot in 1..=9.
  - Accept: contact 10 and beyond gets `slot = NULL`.
  - Accept: slot reassignment swaps rather than fails.
- [x] **T-015** Knock table: record, approve, reject, block

## M2 — Network

- [x] **T-020** Build the iroh `Endpoint` with the tuned transport config
  - Accept: ALPN `swivel/0`. Accept: settings match ARCHITECTURE.md §4.2.
- [x] **T-021** Accept loop with contact authorisation
  - Accept: a known contact is registered. Accept: an unknown id becomes a
    knock and the connection is closed.
  - Accept: a blocked id is dropped with no record.
- [x] **T-022** Per-contact supervisor with backoff and reconnect
- [x] **T-023** Duplicate connection tie-break by smaller dialer `EndpointId`
  - Accept: two processes that dial each other end with exactly one connection.
- [x] **T-024** Control stream: framing, `Control` enum, read and write tasks
- [x] **T-025** Application ping and round trip time measurement
- [x] **T-026** Presence: derive online state, publish to `UiState`
- [x] **T-027** Path type reporting, `DIR` against `RLY`, from `Connection::paths`
- [x] **T-028** Audio datagram header encode and decode
  - Accept: unit test for wrap-around of both counters.
- [x] **T-029** Datagram send path with drop-on-full behaviour
  - Accept: `send_datagram` errors increment a counter, never block.
  - Note: released back to open. It belongs with the encoder thread, T-042.
- [x] **T-030** Datagram receive task, route to the peer slot by `remote_id`

## M3 — Audio

- [x] **T-052** Sample rate conversion for devices that are not 48 kHz
  - Found by measurement, not by design review. See JOURNAL.md.

- [x] **T-040** Device enumeration, selection, and 48 kHz preference
  - Accept: a device without 48 kHz support is reported, not crashed on.
- [x] **T-041** Input stream, downmix to mono, SPSC ring push
  - Accept: no allocation in the callback. Check with a debug allocator hook.
- [x] **T-042** Encoder thread, 480 sample frames, one encode for all members
- [x] **T-043** Fixed peer slot array with preallocated decoders
- [x] **T-044** Output stream, decode, mix, soft limit, upmix
- [x] **T-045** Adaptive jitter buffer per ARCHITECTURE.md §5.4
  - Accept: unit tests for growth, for the delayed shrink, and for reorder.
- [x] **T-046** Packet loss concealment and in-band FEC recovery
- [x] **T-047** Fault tone, mixed into the local output only
- [x] **T-130** Rename the product to `swivel`
- [x] **T-132** Show the key in the panel and copy it with `c`
- [x] **T-133** Let the panel become the key window
  - A borderless `NSPanel` returns `canBecomeKeyWindow == false`, so digits went
    nowhere and the search field could not be clicked into.
  - Note: there is deliberately no tone when the microphone opens or closes.
    Opening a microphone must feel seamless. See DESIGN.md §7.
- [ ] **T-048** Input and output level meters for the roster
- [x] **T-049** Device change handling. Rebuild streams when the default device
  changes.
- [x] **T-050** Choose the input and output device, rather than always using the
  system default
  - Accept: `swivel devices` lists them. `swivel devices --in <n> --out <n>`
    sets them.
  - Accept: the choice is stored in the `settings` table and survives a restart.
  - Accept: a stored device that has gone away falls back to the system default
    and says so, rather than failing to start.
- [x] **T-051** Device submenu in the menu bar, with a tick against the device in
  use
  - Accept: changing a device rebuilds the stream without dropping the session.

## M4 — Sessions

- [x] **T-060** Session state machine and the member mesh
- [x] **T-061** `SessionOpen` fan-out and dial-on-demand for unknown members
- [x] **T-062** `SessionClose`, empty-set teardown, and the 10 minute idle timer
- [x] **T-063** Mute, do-not-disturb, and per-contact `auto_open`
- [x] **T-064** `TalkState` speaking indicator with a voice activity gate

## M5 — User interface

- [x] **T-070** `NSApplication` accessory mode from a plain binary
  - Accept: no Dock icon. Accept: it starts in under 500 ms.
- [x] **T-071** `NSStatusItem` with the five icon states from DESIGN.md §6.1
- [x] **T-072** Right-click menu: mute, DND, devices, copy key, quit
- [x] **T-073** `style.rs`: colour tokens, monospace font, 2 px border helper,
  stipple shadow helper, notched section label helper
- [x] **T-074** The floating `NSPanel`, positioned under the status item
- [x] **T-075** Custom roster view: slot box, name, presence dot, RTT, path type
- [x] **T-076** Live row inversion and the live session summary
- [x] **T-077** Search and add field with `sv1` paste detection
- [x] **T-078** Knock approval row with `a` and `x` keys
- [x] **T-079** `arc-swap` `UiState` snapshot and the 10 Hz redraw timer
- [ ] **T-080** Main-thread wake for immediate transitions
  - Note: the 10 Hz timer covers it for now. A session opening can lag by up
    to 100 ms in the roster, which nobody has noticed yet.

## M6 — Hotkeys

- [x] **T-090** Register `⌃⌥⌘T`, `⌃⌥⌘Esc`, `⌃⌥⌘M` through `global-hotkey`
- [x] **T-091** Panel key window handling and digit capture in `keyDown:`
- [x] **T-092** Digit toggles a member in and out of the session
- [x] **T-093** `Esc` hides the panel and leaves the session live

## M7 — Command line

- [x] **T-100** `swivel key`, with `--copy`
- [x] **T-101** `swivel add`, `list`, `rm`, `slot`, `approve`, `block`
- [x] **T-102** `swivel doctor`: devices, sample rates, permission, relay, RTT
- [ ] **T-103** `swivel doctor --loopback` mouth-to-ear measurement
- [ ] **T-104** `swivel doctor --tune` suggested settings
- [x] **T-105** `swivel tui` headless mode for two-process tests

## M8 — Packaging

- [x] **T-110** `build.rs` that embeds `Info.plist` through the linker
  - Accept: `otool -s __TEXT __info_plist` shows the plist in the binary.
- [ ] **T-111** `scripts/build-release.sh` with ad-hoc `codesign`
- [ ] **T-112** Release profile: `lto = "fat"`, `codegen-units = 1`, `panic` stays
  `unwind` because the audio callbacks must not abort the process
- [ ] **T-113** README with a two-minute quick start
- [ ] **T-114** Universal binary for Apple Silicon and Intel

## M9 — Verification

- [ ] **T-127** Quit from the keyboard, for when the menu bar is unreachable
  - Note: a menu bar manager can hide the icon, and then the only way out is
    `pkill swivel`.

- [ ] **T-120** Two-process integration test script
- [ ] **T-121** Latency measurement harness and a recorded baseline
- [ ] **T-122** Network shaping test with `dnctl` for delay and loss
- [ ] **T-123** Real-time safety check. Assert no allocation in the callbacks.
- [ ] **T-124** Twelve-hour soak test. Watch for latency creep and leaks.
- [ ] **T-125** Debug allocator hook that fails a test on allocation in a
  real-time callback
- [ ] **T-126** Remove the crate-level `allow(dead_code)` from `main.rs`
- [ ] **T-131** Remove the identity migration from the former name `walkie`
  - It lives in `store::adopt_former_name`. Drop it once nobody is on the old
    name.

## Backlog — not scheduled

- [ ] **B-001** Acoustic echo cancellation with `webrtc-audio-processing`
- [ ] **B-002** A stable self-signed certificate to stop repeat TCC prompts
- [ ] **B-003** More than nine slots with a two-digit entry mode
- [ ] **B-004** Opus stereo or a raw PCM mode for a LAN
- [ ] **B-005** Linux and Windows user interfaces
- [ ] **B-006** A per-contact volume trim
- [ ] **B-007** Two independent simultaneous sessions
- [ ] **B-008** A push-to-talk key for people who want the microphone closed
