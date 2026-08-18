# swivel

A peer-to-peer voice intercom for people who talk to the same few friends every
day.

Press a shortcut. Press a number. Speak. The other person hears you.

There is no call. There is no ring. There is no hang up. There is no account.

## What it is

One macOS binary that lives in the menu bar. It connects straight to your
friends over [iroh](https://www.iroh.computer/), hole punching to a direct path
where it can. Audio is Opus in unreliable QUIC datagrams, because late audio is
worse than missing audio.

Measured on the development machine: **41 ms** of fixed latency, plus half the
network round trip. On a LAN that is about 42 ms, mouth to ear.

## Two minute start

```bash
./swivel doctor        # check audio, permission, and connectivity
./swivel key           # print your key, and send it to a friend
./swivel add sv1…      # add theirs
./swivel               # run it, and look in the menu bar
```

Your friend runs `swivel approve <key>` once, and the link goes live.

Speakers are fine. Echo cancellation is on by default, using the same Apple
voice processing unit FaceTime uses. The menu has a toggle if you wear
headphones and want the audio untouched.

## Talking

| Key | What happens |
|---|---|
| `⌃⌥⌘T` | Open the roster |
| `3` | Talk to contact 3. Your microphone opens, and so does theirs. |
| `5` | Add contact 5. Now all three of you are in one conversation. |
| `3` | Press it again to drop contact 3 |
| `⌃⌥⌘M` | Mute |
| `⌃⌥⌘Esc` | End the conversation |
| `Esc` | Hide the roster, and keep talking |

There is no answering. When a contact opens a conversation with you, you hear
them and they hear you. That is the point.

The microphone opens when a conversation starts and closes when it ends. It is
never opened just to look at the roster. The menu bar icon always says which:

| Icon | Meaning |
|---|---|
| `((·))` | Idle |
| `((o))` | The microphone is open but nothing is being sent |
| `((3 5))` | Live to contacts 3 and 5 |
| `((/))` | Muted |
| `((x))` | Do not disturb |
| `((~))` | Not connected yet |

## Commands

| Command | What it does |
|---|---|
| `swivel` | Run it in the menu bar |
| `swivel key [--copy]` | Print your key |
| `swivel add <key> [--name N]` | Add a contact |
| `swivel list` | Contacts, slots, and who is waiting |
| `swivel approve <key>` | Let someone in |
| `swivel block <key>` | Keep someone out |
| `swivel slot <who> <n>` | Move a contact to another number |
| `swivel rm <who>` | Remove a contact |
| `swivel devices` | List audio devices, or choose one |
| `swivel doctor` | Check this machine |
| `swivel tui` | Run without the menu bar |
| `swivel snapshot --demo` | Draw the interface to a PNG |

`⌃⌥⌘T` opens the roster. A left click on the icon does the same. A right click
opens the menu, which has Quit in it. `pkill swivel` also works.

## Building

```bash
./scripts/build-release.sh            # both architectures, for sending out
./scripts/build-release.sh --fast     # this machine only, for testing
```

That writes `./swivel` in the repository root: one signed universal binary,
about 32 MB, that runs on both Apple Silicon and Intel. There is nothing for the
recipient to install. libopus and SQLite are compiled in.

The permission prompt comes from an `Info.plist` linked into the binary itself,
because there is no bundle to put one in. An ad-hoc signature gives macOS a
stable identity to remember the grant against. A rebuild changes that identity,
so a new build asks once more.

## Sending it to a friend

**Give them a URL and one line.** A GitHub release works, and so does any static
host.

```bash
curl -fsSL <url>/swivel -o swivel && chmod +x swivel && ./swivel doctor
```

`curl` does not mark the file as quarantined, so Gatekeeper never blocks it.
This matters: the binary is ad-hoc signed rather than notarised, so a
**browser, AirDrop, Messages, or Slack** download *is* marked, and macOS refuses
to run it until the mark is cleared:

```bash
xattr -d com.apple.quarantine swivel
chmod +x swivel
./swivel doctor
```

Start with `swivel doctor` either way. It checks the audio devices and triggers
the microphone prompt, so anything wrong shows up before they try to talk to
somebody.

Notarising would remove the quarantine step for every route, but it needs a paid
Apple Developer account. For a handful of friends, the `curl` line is simpler.

## Privacy

- No accounts, and no server that holds anything about you.
- Your identity is a keypair on your machine.
- Only contacts you approved can reach you. Everyone else gets a knock you can
  refuse.
- Relays forward packets when a direct path cannot be made. They cannot read
  them.
- The microphone opens for a conversation and nothing else.

## Reading further

| File | What is in it |
|---|---|
| `DESIGN.md` | What it does and why |
| `ARCHITECTURE.md` | How it is built |
| `LOOP.md` | How to work on it |
| `TODO.md` | What is left |
| `JOURNAL.md` | What went wrong before, and what it taught us |
