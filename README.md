# walkie

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
./walkie doctor        # check audio, permission, and connectivity
./walkie key           # print your key, and send it to a friend
./walkie add wt1…      # add theirs
./walkie               # run it, and look in the menu bar
```

Your friend runs `walkie approve <key>` once, and the link goes live.

**Wear headphones.** Version 1 has no echo canceller, so on speakers the other
person hears themselves.

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
| `walkie` | Run it in the menu bar |
| `walkie key [--copy]` | Print your key |
| `walkie add <key> [--name N]` | Add a contact |
| `walkie list` | Contacts, slots, and who is waiting |
| `walkie approve <key>` | Let someone in |
| `walkie block <key>` | Keep someone out |
| `walkie slot <who> <n>` | Move a contact to another number |
| `walkie rm <who>` | Remove a contact |
| `walkie devices` | List audio devices, or choose one |
| `walkie doctor` | Check this machine |
| `walkie tui` | Run without the menu bar |
| `walkie snapshot --demo` | Draw the interface to a PNG |

`⌃⌥⌘T` opens the roster. A left click on the icon does the same. A right click
opens the menu, which has Quit in it. `pkill walkie` also works.

## Building

```bash
./scripts/build-release.sh
```

That produces one signed binary, about 15 MB, with no dependencies to install.
Send the file. libopus and SQLite are compiled in.

The permission prompt comes from an `Info.plist` linked into the binary itself,
because there is no bundle to put one in. An ad-hoc signature gives macOS a
stable identity to remember the grant against. A rebuild changes that identity,
so a new build asks once more.

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
