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

## M10 — Reported defects (highest priority)

Reported from use on 2026-08-19. Work these before any other milestone. They
are ordered by how quickly they can be fixed, easiest first.

Two reports from the same list are already covered:

- "People in a multi-person session hear some members and not others" was
  T-135, merged in `0b5357b`. T-136 is the remainder of it: a redundant `arm`
  still rebuilds the audio path and interrupts everyone.
- T-136 stays where it is, in M4.

- [x] **T-140** Cache the build in CI
  - A release build starts from an empty `target/` every time. It compiles
    every dependency twice, once per architecture, and it builds the vendored
    Opus source with CMake as well. A release takes far longer than it should.
  - `release.yml` runs on a tag. A run can only restore a cache written by its
    own ref or by the default branch, and no workflow runs on the default
    branch today, so a cache step in `release.yml` alone would never hit.
  - Fix: add a workflow on `main` that builds, tests, and writes the cache.
    Add the restore to `release.yml`. The `main` workflow also catches a
    toolchain fault before a release, which is what the CMake 4 failure in
    T-139 cost a whole release cycle.
  - Accept: a second release with no dependency change reuses the cache and is
    measurably faster than the first.
    - Not checked yet. It can only be seen when a release runs, and `ci.yml`
      must run on `main` once first, because a tag reads the default branch's
      cache. The first release after this change is still slow.
  - Accept: a push to `main` runs clippy and the tests.

- [ ] **T-141** Tick the audio device in use
  - The device submenu lists every device and marks none of them, so there is
    no way to see which one is in use. "Cancel echo" carries a tick already,
    which makes the missing ones look like a fault.
  - `build_devices_menu` never calls `setState`. It also does not know the
    current choice, because `UiState` does not carry the device names.
  - Fix: carry the input and output device names in `UiState`, and tick the
    matching item. Tick "Use the system defaults" when no device is stored.
  - Accept: the menu ticks exactly one microphone and one speaker.
  - Accept: choosing another device moves the tick.

- [ ] **T-142** The panel shows the wrong people after it is opened again
  - Two faults with one symptom. Opening and closing the panel shows a
    different set of contacts each time.
  - `RosterView::draw` groups `state.peers` and ignores the filter, but
    `content_height` measures the filtered set. A panel sized for one row then
    draws nine, and the rows past the height are dropped by the `break`.
  - The search field keeps its text when the panel is dismissed by a click
    elsewhere, so the stale filter applies on the next open.
  - `Panel::show` also sizes the panel from the previous snapshot, because
    `set_state` runs after `show`.
  - Fix: draw the filtered set, clear the field when the panel hides, and
    publish the state before the panel measures itself.
  - Accept: a roster of nine contacts shows nine rows on every open.
  - Accept: a search, then a dismiss, then an open, shows every contact.

- [ ] **T-143** Paste into the search field with ⌘V
  - ⌘V does nothing in the search field, so a key must be typed by hand. A key
    is 63 characters. This makes adding a contact through the panel unusable.
  - An accessory application has no menu bar, so no Edit menu carries the
    standard key equivalents, and `NSTextView` never receives `paste:`.
  - Fix: handle the editing key equivalents on the panel and send them to the
    first responder.
  - Accept: ⌘V, ⌘C, ⌘X, ⌘A, and ⌘Z work in the search field.
  - Accept: a pasted `sv1` key is added with Return, as a typed one is.

- [ ] **T-144** Click a slot to open or close a conversation
  - A digit works. A click does nothing but move focus. Every row looks like a
    control and behaves like a label.
  - `RosterView::mouseDown:` only takes first responder. The drawing code
    computes row positions and throws them away.
  - Fix: record the rectangle of each drawn row, and act on a click inside one.
    A click on a row is the same action as its digit.
  - Accept: a click on a row starts a conversation with that contact.
  - Accept: a click on a live row takes that contact out of the conversation.
  - Accept: a contact with no slot can still be reached by a click.

- [ ] **T-145** Leave a conversation from the panel
  - The only ways out are the `0` key, ⌃⌥⌘⎋, and a menu item. Nothing in the
    panel says so, so quitting looks like the only way to leave.
  - Fix: draw a visible control in the live session summary that ends the
    session, and name the key beside it.
  - Accept: the panel shows how to leave whenever a session is live.
  - Accept: leaving a three-person session leaves the other two talking.

- [ ] **T-146** Run detached by default
  - `./swivel` holds the terminal. The program is a menu bar application, so
    the terminal it started from is of no further use.
  - Fix: the plain `swivel` command starts the application detached and
    returns. Add `--foreground` for the current behaviour, which the logs and
    the two-process tests need.
  - Care: the log output must go somewhere a user can find. Care: a second
    instance must not start silently beside the first.
  - Accept: `./swivel` returns to the prompt with the icon in the menu bar.
  - Accept: `swivel --foreground` behaves as `swivel` does today.
  - Accept: `SWIVEL_LOG=debug ./swivel` still records its log.

- [ ] **T-147** Ask before a stranger joins a conversation
  - A member can add somebody this machine does not know. Today that arrives
    as a silent `SessionOpen` from a stranger, which `on_session_open` drops
    with a debug line. The user never learns that somebody tried to talk.
  - A known contact set to knock is no better: the notice is written to
    `UiState::fault`, and the next `refresh_ui` sets `fault: None`, so it is
    erased within two seconds.
  - Fix: hold the request as a real record, the way a knock is held, and show
    it in the panel with accept and refuse. Give `fault` a lifetime, or drop
    it for a field that survives a refresh.
  - Accept: adding a stranger to a conversation raises a request on their
    machine that waits for an answer.
  - Accept: the request survives a refresh and stays until it is answered.

- [ ] **T-148** Audio stops for everyone after a reconnect
  - After a disconnect and a reconnect, nobody can be heard until the process
    is restarted.
  - Suspected cause: `Command::Arm` drops the speaker, then calls
    `start_talking`. The voice unit path moves the queue consumers into the
    mixer, so a failure there loses them and returns an error. The arm handler
    only logs it. The machine is then left with no speaker at all, which is
    exactly "audio stopped for everyone". A voice unit is most likely to
    refuse to start moments after the previous one was torn down, which is
    what a reconnect does.
  - T-136 is the other half: a redundant `arm` should not rebuild the path at
    all, so the failure window should not be entered.
  - Fix: the audio path must always end with an open speaker. A failed
    conversation start falls back to listening, and it says so.
  - Accept: a voice unit that refuses to start leaves the speaker open.
  - Accept: two machines that disconnect and reconnect five times still hear
    each other, with `played` rising on both.

- [ ] **T-149** The received audio is quiet and rough
  - The far end is too quiet and does not sound clean.
  - Places to measure before changing anything: `PEER_GAIN` at 0.8 with no
    make-up gain; the voice unit runs with automatic gain control off; the
    microphone is downmixed by an average, which loses 6 dB on a device that
    carries the voice in one channel of two; the Catmull-Rom converter runs on
    any device that is not at 48 kHz.
  - Fix: measure first. Record a known tone through the whole path and report
    the level at each stage. Change one thing at a time.
  - Accept: a measured end-to-end level within 3 dB of the input.
  - Accept: the numbers and the decision are written in ARCHITECTURE.md §5.

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
- [x] **T-134** Echo cancellation with Apple's Voice Processing unit
  - Accept: a conversation on a loudspeaker does not send the far end their own
    voice back.
  - Accept: the unit is built when a conversation starts and destroyed when one
    ends, so the microphone is never held open.
  - Accept: a machine where the unit will not start still gets a conversation.
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
- [x] **T-135** Engine-owned session membership for the receive slots
  - Bug: the hub of a three-person session hears only the last member added.
    `add_member` arms on every press, `arm` installs a fresh empty `SlotTable`,
    and only the new member is re-activated. The other members' datagrams are
    dropped in `deliver` because they no longer own a slot.
  - Fix: the engine keeps the desired member set and repopulates every fresh
    table from it inside `swap_slots`, so no `arm`/`disarm`/`set_devices`
    caller carries a re-activation duty.
  - Accept: with three instances meshed, `played` rises on all three machines
    for both remote peers.
  - Accept: the manual re-activation loops in `set_device` and
    `on_session_open` are gone, not duplicated.
- [ ] **T-136** A redundant `arm` must not rebuild the audio path
  - Every `arm` while already talking tears down and rebuilds the whole path,
    so each membership change gives every member an audible hiccup and a
    burst of queue overruns while the new table waits for the audio thread.
  - With T-135 the engine repopulates every fresh table itself, so the old
    reason for the caller-side swap is gone. An `arm` that changes nothing
    should do nothing.
  - Accept: adding a third member does not interrupt audio from the first.
  - Accept: the `overrun` counter stays at zero across a digit press while
    talking.
  - Care: `arm` is also the path that applies a changed echo cancellation
    setting. "Changes nothing" must compare that too. See ARCHITECTURE.md §5.

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
- [x] **T-111** `scripts/build-release.sh` with ad-hoc `codesign`
  - Shipped alongside T-114. The script existed and signed ad-hoc; only the
    checkbox was stale.
- [ ] **T-112** Release profile: `lto = "fat"`, `codegen-units = 1`, `panic` stays
  `unwind` because the audio callbacks must not abort the process
- [ ] **T-113** README with a two-minute quick start
- [x] **T-114** Universal binary for Apple Silicon and Intel
  - The release script writes `./swivel` in the repository root, joined with
    `lipo` and signed afterwards, because `lipo` discards signatures.
- [x] **T-137** `swivel update` over GitHub releases
  - The repo (ndemonner/swivel) is being made public. Releases are tagged
    `v<semver>`, built locally by `scripts/release.sh`, and uploaded as
    release assets with a detached ed25519 signature.
  - `swivel update` reads the version from the `releases/latest` redirect,
    compares it with the built-in version, downloads the binary and its
    signature, verifies against a public key compiled into the binary, and
    atomically replaces its own executable. `--check` only reports.
  - The signature is not optional. This program opens microphones; a
    compromised update channel is remote microphone access, so GitHub's TLS
    alone is not enough.
  - Accept: `swivel update` on an old version installs the latest release and
    the running copy is untouched until restart.
  - Accept: a corrupted or wrongly signed download is refused and the
    installed binary is unchanged.
- [x] **T-138** Stable self-signed release certificate (was B-002)
  - macOS ties the microphone grant to the code signing identity. Ad-hoc
    signatures change with every build, so every update re-prompts. One
    self-signed certificate on the release machine keeps the identity stable.
  - Accept: `scripts/build-release.sh` signs with the certificate when it is
    present and falls back to ad-hoc with a warning when it is not.
- [x] **T-139** Team releases through CI
  - Finished by ndemonner on the admin's machine, after wtachau handed it
    over. The three secrets are set, and v0.2.2 published through CI.
  - The release key was rotated. The old key was created on wtachau's
    machine, and a private key must not move between laptops, so this
    created a new key on the admin's machine. A v0.2.0 binary embeds the old
    public key and refuses every later release. Anyone on v0.2.0 installs
    once more with curl.
  - The first attempt, v0.2.1, failed on CMake 4 on the runner. See
    `.cargo/config.toml` and JOURNAL.md. The tag is deleted.
  - Four people must be able to release. Copying the signing key and the
    certificate to four laptops means four theft targets and no revocation,
    so the build moves to GitHub Actions instead. The two secrets live once,
    in the repository's secret store.
  - `scripts/release.sh` becomes the trigger: it bumps, commits, tags, and
    pushes. The workflow builds the universal binary, codesigns with the
    imported certificate, signs the artifact with the release key, and
    publishes the GitHub release. `--local` keeps the old single-machine path
    as a fallback for when CI is down.
  - Accept: a collaborator with push access and no local keys can cut a
    release by running `./scripts/release.sh <version>`.
  - Accept: the workflow refuses a tag whose version does not match
    Cargo.toml.
  - Needs repository admin once, to set the secrets. wtachau does not have
    admin, so that step is a handoff.

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

- [x] **B-002** A stable self-signed certificate to stop repeat TCC prompts
  - Done as T-138.
- [ ] **B-003** More than nine slots with a two-digit entry mode
- [ ] **B-004** Opus stereo or a raw PCM mode for a LAN
- [ ] **B-005** Linux and Windows user interfaces
- [ ] **B-006** A per-contact volume trim
- [ ] **B-007** Two independent simultaneous sessions
- [ ] **B-008** A push-to-talk key for people who want the microphone closed
- [ ] **B-009** A release key rollover path, so a rotation stops breaking clients
  - `RELEASE_PUBKEY_HEX` is one trust anchor with no way to move. Every
    rotation makes each installed binary refuse all later releases, which is
    what the T-139 rotation did to v0.2.0. Key loss has the same effect and
    no recovery.
  - Two options. Compile a short list of trusted keys and accept a signature
    from any of them. Or publish the new public key signed by the old key, so
    a client that trusts the old one can adopt the new one.
  - Accept: a rotation ships a release that binaries on the previous version
    still install.
