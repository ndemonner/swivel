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
  by hand without it, and `swivel tui` is the only way to test two peers.
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
  limiter, fault tone, and `swivel doctor`.
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
- Testing note: driving `swivel tui` through a fifo needs a writer held open,
  for example `sleep 900 > /tmp/wa.in &`. Without it the first `echo` closes
  the pipe, standard input reaches end of file, and the instance shuts down.
- Next: M5, the menu bar and the panel.

## 2026-08-18 — M5 and M6. The menu bar interface.

- Did: T-070..T-079, T-090..T-093, T-110. Menu bar item, floating panel,
  hand-drawn roster, global shortcuts, and the embedded `Info.plist`.
- **Surprise, and it cost an hour: `screencapture` from this environment
  returns the desktop with every window missing.** The panel was drawing
  correctly the whole time and the screenshots showed bare wallpaper, which
  reads exactly like a window that failed to composite. The terminal lacks the
  screen recording permission. The fix is `swivel snapshot`, which renders the
  panel from inside the process to a PNG. Never trust `screencapture` here.
- Learned: a window ordered front **before** `NSApplication::run` is never
  composited. The debug panel now opens on the first timer tick instead.
- Learned: `makeKeyAndOrderFront` does nothing when an accessory application is
  not active. `orderFrontRegardless` is what actually shows the panel.
- Bug found by the user: **the microphone stayed open permanently.** `arm` was
  called when the panel opened and nothing ever disarmed. Two fixes. Arming now
  lives inside `show_panel`, so every way of opening the panel arms and every
  way of closing it disarms, in one place. And `disarm_if_stale` closes a
  microphone that was armed but never used after 8 seconds. `MicState::Armed`
  and the `((o))` icon make the state visible rather than silent.
- Learned: `NSStatusItem` reports its button window at `y = -24` before the menu
  bar lays it out, so the panel was placed off screen. Every placement is now
  clamped to the visible frame, which also covers a user whose status item sits
  in a hidden overflow area.
- Learned: a status item gives one action for both mouse buttons. Reading
  `NSApp.currentEvent()` inside the action tells them apart, and
  `sendActionOn(LeftMouseUp | RightMouseUp)` is needed or a right click does
  nothing. The menu is attached only for the instant it is shown, because
  leaving it attached makes a left click open the menu.
- Learned: a single-line `NSTextField` draws its text at the top of its frame.
  The frame is sized to the text and centred in the box the roster draws.
- Note: `pkill swivel` is the fallback when the menu bar icon is unreachable.
  T-127 tracks a keyboard quit.
- Next: M7 remaining commands, then M8 packaging.

## 2026-08-18 — T-052. The bug that sounded fine.

- **Playback ran at 44100/48000 of the correct speed on Bluetooth headphones,
  and nothing errored.** Audio worked. It sounded almost right. The only
  evidence was the `played` counter falling behind `sent` by a steady 8.2
  percent.
- How it was found: after the session work, `played` was 65 frames behind
  `sent` where it had been 1 frame behind. The instinct was to call it a
  startup cost and move on. Sampling the counters three times five seconds
  apart showed the gap **growing** at a constant rate, and a constant ratio is
  never a startup cost. 0.918 is 44100/48000.
- Root cause: `device::choose` correctly reported `native_rate = false` for a
  44.1 kHz device, and `swivel doctor` correctly printed a fault about it. The
  code then did nothing with that fact and fed 48 kHz audio to the device
  anyway. `ARCHITECTURE.md` claimed the pipeline resampled with `rubato`. It
  did not. **A document is not an implementation.**
- Fix: `audio/resample.rs`, a Catmull-Rom cubic that runs inside the callback.
  It allocates nothing, costs a few multiplies per sample, and adds no block of
  latency. `rubato` has better stopband rejection but cannot run in a real-time
  callback without a block delay.
- Confirmed: `played` now tracks `sent` exactly. 621/621, 1221/1222, 1622/1622,
  zero concealed.
- Lesson worth keeping: **the positive counter caught this and no error path
  ever would.** Every other counter reports a failure, and there was no failure
  here. If `played` did not exist, this would have shipped, and the report
  would have been "it sounds a bit off" from someone on AirPods.
- Lesson two: a Bluetooth headset at 44.1 kHz is the common case, not an edge
  case. Test on the hardware people actually own.

## 2026-08-18 — T-135 engine-owned session membership

- Bug reported by the user: in a three-person session the hub heard only the
  member added last. It looked like a net split. It was not: every connection
  was healthy, and the hub itself was heard by everyone.
- Root cause: slot ownership is derived state, and re-deriving it after a
  path rebuild was a convention spread across the `arm`/`disarm`/`set_devices`
  call sites. `add_member` arms on every digit press, `arm` installs a fresh
  empty `SlotTable`, and `add_member` re-activated only the new member. The
  older members' datagrams were then dropped at `deliver`, which is silent by
  design. `on_session_open` re-activated the full list, which is why only the
  hub was deaf.
- Fix: the engine now stores the declared member set and `swap_slots`
  populates every fresh table from it before publishing. The session declares
  membership through `AudioSink::set_members` from one place,
  `publish_session`. The re-activation loops in `set_device` and
  `on_session_open` are deleted, and `Engine::slots` is private so nothing can
  bypass the set.
- Verified: three tui instances in a full mesh. On this branch every instance
  settles at played/encoded = 2.0 exactly, so everyone hears both peers. The
  same script on unfixed `main` shows the hub at 1.0 and the other two at 2.0.
  The script drives the hub with digit presses over a fifo and reads the
  counter line from the tui output. Worth turning into T-120.
- Learned: the counters prove membership without listening. played/encoded is
  frames heard per frame spoken, and in a full mesh of three it must be 2.0.
  A ratio, not a rate: the wall clock drops out, so two samples of the log
  line are enough.
- Surprise: the fixed hub shows a one-time `overrun` burst of ~51 packets at
  the second digit press. It is the arm rebuild window: `deliver` pushes to
  the new table's queues before the audio thread starts draining them. On old
  `main` the same window showed zero overruns only because the packets were
  thrown away earlier, at `deliver`. That rebuild is also an audible hiccup on
  every membership change, which T-136 removes by making a redundant `arm` do
  nothing.
- Next: T-136. With `swap_slots` self-populating, the only remaining reason
  `arm` rebuilds while already talking is gone.

## 2026-08-18 — T-137, T-138 release flow and self-update

- Did: `swivel update` over GitHub releases with mandatory ed25519 signature
  verification; `scripts/release.sh` to cut releases; a stable self-signed
  code signing certificate so updates keep the microphone grant.
- Design: no API, no JSON, no tokens. The `releases/latest` redirect carries
  the version in its Location header, and `curl` does the transfers. curl
  also never sets the quarantine flag, so Gatekeeper stays out of it.
- Learned: OpenSSL 3 exports PKCS12 with ciphers `security import` cannot
  read. The error is "MAC verification failed (wrong password?)", which
  points at the wrong cause entirely. The fix is `-legacy` on the export.
  LibreSSL, which macOS ships, needs no flag, so the script probes for it.
- Learned: `security add-trusted-cert` blocks on a GUI password dialog. A
  script that runs it must say so, or a headless session looks hung.
- The keys: the release signing key is
  `~/Library/Application Support/dev.motor.swivel/release-signing.key`, and
  the public half is `RELEASE_PUBKEY_HEX` in config.rs. Losing the key file
  means no more releases that existing binaries accept. It needs a backup.
- Blocked on: the repository is still private, and only its owner
  (ndemonner) can make it public. `swivel update` works the moment that
  flips. wtachau has push, which is enough to publish the release itself.
- Next: T-103/T-104 doctor extensions, or T-120 to turn this session's local
  release-server test into a script.

## 2026-08-18 — T-139 team releases through CI

- Did: moved the release build into a GitHub Actions workflow so all four
  team members can release with `./scripts/release.sh <version>` and no keys
  on their machines. The script now only bumps, tags, and pushes; CI builds,
  codesigns, signs with the release key, and publishes. `--local` keeps the
  old path as a fallback.
- Rotated the signing certificate so the .p12 could be captured for the CI
  secret store; the original's private key was not exportable from the
  keychain without the GUI. make-signing-cert.sh now always saves the .p12
  and its password next to the database. The one release signed with the old
  certificate, v0.2.0, will re-ask for the microphone once on the next
  update, which today affects nobody.
- Learned: the trust boundary moves. With secrets in the repository, anyone
  who can edit the workflow or the secrets can ship a signed release.
  Repository admin is now the thing to guard, and the ARCHITECTURE note says
  so.
- Blocked: setting repository secrets needs admin, which wtachau does not
  have on ndemonner/swivel. The exact `gh secret set` commands are in the
  session notes and in make-signing-cert.sh's output. Until they run, the
  workflow fails with a clear message at the secret check.
- Next: once the secrets exist, cut v0.2.1 through CI to verify the whole
  path, then mark T-139 done.
