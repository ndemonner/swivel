# walkie — Product Design

## 1. Summary

`walkie` is a peer-to-peer voice intercom for small groups of friends. It gives
you the feel of a walkie talkie or an open office door. You press a shortcut,
you press a number, and you speak. The other person hears you immediately.

There is no call. There is no ring. There is no hang up. There is no account.

The product is one CLI binary. You send the binary to a friend. Your friend runs
it. The binary puts an icon in the macOS menu bar and stays out of the way.

## 2. Design principles

1. **Latency is the product.** Every feature must defend the latency budget.
   The target is under 50 ms mouth-to-ear on a LAN. See ARCHITECTURE.md §7.
2. **No session setup.** Connections stay warm. A talk session only opens a
   microphone. It never opens a network connection.
3. **No accounts.** Identity is a keypair. You share a key out of band.
4. **The hands stay on the keyboard.** No mouse is needed to talk.
5. **You always know when your microphone is live.** This rule overrides
   minimalism. See §7.
6. **Minimal surface.** If a feature is not used weekly, it does not ship.

## 3. Users

The users are technical. They can run a binary from a terminal. They can grant
a microphone permission. They do not need an installer or a signed application.
They have 2 to 12 contacts. They talk to the same few people every day.

## 4. Identity and contacts

### 4.1 Your key

Each install creates one Ed25519 keypair on first run. The public key is your
`EndpointId`. Your shareable "key" is a **ticket**:

```
wt1qy8ptc9m4x...   (base32, contains your endpoint id and your display name)
```

Run `walkie key` to print the ticket. Run `walkie key --copy` to copy it.
You send the ticket to a friend over any channel. Signal, SMS, and a napkin all
work.

### 4.2 Adding a contact

Your friend runs:

```
walkie add wt1qy8ptc9m4x...
```

`walkie` stores the contact in a local SQLite database. It assigns the contact
the lowest free slot number. It then dials the contact and keeps the connection
warm.

### 4.3 Approving a contact

Adding is one-sided. The other side must approve you.

1. Your friend adds your ticket.
2. Your friend's binary dials you.
3. Your binary does not know that endpoint id. It records a **knock**.
4. The menu bar icon shows a badge. The panel shows the knock.
5. You press `a` to approve, or `x` to reject.
6. On approval `walkie` assigns a slot and the link goes live.

A rejected endpoint id is blocked. It cannot knock again.

### 4.4 Slot numbers

Slots 1 to 9 are assigned automatically in order. A slot is the only thing you
type to talk to a person. You can reassign a slot in the panel. Contacts past
slot 9 are stored and shown, but you must select them in the panel.

## 5. The talk interaction

### 5.1 Opening a session

1. Press `⌃⌥⌘T` anywhere. The panel appears. Nothing transmits yet.
2. Press `3`. Your microphone opens to contact 3. You can speak now.
3. Press `5`. Your microphone also opens to contact 5.
4. Contacts 3 and 5 are now connected to each other as well. It is one
   three-way conversation, not two private ones.
5. Press `3` again to drop contact 3 from the session.
6. Press `Esc` to hide the panel. The session stays live.
7. Press `⌃⌥⌘Esc` anywhere to end the session and close your microphone.

The microphone opens on the digit press. There is no confirmation step. This is
the walkie talkie behaviour the product is built around.

### 5.2 Receiving

There is no answer step. When a contact opens a session to you, their audio
plays. Your microphone opens back to them. You reply by speaking.

This is a hot microphone. §7 describes the controls that make it safe.

### 5.3 Multi-party

A session is a set of members. The initiator sends the member list to every
member. Every member then opens its microphone to every other member. Audio is
a full mesh. There is no server and there is no mixer.

The mesh is capped at 8 members. See ARCHITECTURE.md §6.4.

## 6. The interface

### 6.1 Menu bar

The application lives in an `NSStatusItem`. It has no Dock icon and no main
window. The icon shows the state:

| State | Icon | Meaning |
|---|---|---|
| Idle | `((·))` | Running, microphone closed |
| Armed | `((o))` | The microphone is open but nothing is sent |
| Live | `((3 5))` | Microphone open to slots 3 and 5 |
| Receiving | `((•))` | A contact is speaking to you |
| Muted | `((/))` | Microphone forced off |
| Do not disturb | `((x))` | Incoming sessions are refused |
| Offline | `((~))` | No relay yet, so nobody can reach you |

A left click opens the panel. A right click opens a menu with Open walkie,
Mute, Do not disturb, End session, Copy my key, and Quit.

The **armed** state matters. It means the input device is open but nothing is
being sent, which happens for a moment while a session comes up and whenever you
are muted inside one. It gets its own icon rather than hiding behind the idle
one.

### 6.2 The panel

The panel is a floating, borderless `NSPanel`. It appears near the menu bar
icon. It takes keyboard focus. It closes on `Esc` or on focus loss.

```
┌─ WALKIE ─────────────────────────────────┐
│                                          │
│  ┌────────────────────────────────────┐  │
│  │ search or paste key…               │  │
│  └────────────────────────────────────┘  │
│                                          │
│  ONLINE ─────────────────────────────    │
│  ┌─┐                                     │
│  │1│ MAGGIE HENRY          ● 12ms  DIR   │
│  └─┘                                     │
│  ┌─┐                                     │
│  │3│ WILL TACHAU           ● 44ms  RLY   │
│  └─┘                                     │
│                                          │
│  OFFLINE ────────────────────────────    │
│  ┌─┐                                     │
│  │5│ DAVID MARCIN          ○  --         │
│  └─┘                                     │
│                                          │
└──────────────────────────────────────────┘
```

A row in the live session is drawn inverted. Black fill, white text. This is
the only strong visual state in the interface.

The right of each row shows the round trip time and the path type. `DIR` means
a direct hole-punched path. `RLY` means the traffic goes through a relay.

### 6.3 Visual language

The style comes from `reference/aesthetic-neobrutalist-mono.png`.

| Token | Value | Use |
|---|---|---|
| `ink` | `#111318` | Borders, text |
| `paper` | `#F7F8FA` | Window background |
| `card` | `#FFFFFF` | Input fields |
| `control` | `#E4E8EF` | Buttons, slot boxes |
| `live` | `#E5484D` | Live indicator only |
| `online` | `#30A46C` | Presence dot |

Rules:

1. Every border is 2 px and solid `ink`.
2. Every corner is square. No radius anywhere.
3. Every font is monospace. Use SF Mono, then Menlo.
4. Section labels are uppercase and sit in a gap in the border line.
5. Raised elements get a hard offset shadow, 6 px right and 6 px down. The
   shadow is a stipple pattern, not a blur.
6. There is no animation except the presence dot and the live row.

### 6.4 Search and add field

One text field does two jobs.

1. Type letters to filter the roster.
2. Paste a `wt1…` ticket and press `Return` to add a contact.

## 7. Microphone safety

The hot microphone is the main risk of this product.

**There is no tone when the microphone opens or closes.** An intercom that
beeps every time somebody speaks is a worse intercom, and the product is built
on the idea that talking should feel like turning your head. Opening a
microphone stays seamless. The controls below carry the job instead, and they
are all silent and visible.

1. **Menu bar state.** The icon is filled and shows the live slot numbers. This
   is the primary signal and it is always on screen.
2. **The macOS microphone indicator.** The input device opens when a
   conversation starts and closes when it ends. It is never opened to look at
   the roster, and never held open for the life of the process, so the system
   indicator means exactly what it appears to mean.
3. **Mute.** `⌃⌥⌘M` forces the microphone off. Sessions stay open. Members see
   you as muted.
4. **Do not disturb.** Refuses incoming session opens. Contacts see `DND`.
5. **Per-contact auto-open.** Set a contact to `knock` instead of `auto`. Their
   session open then needs one keypress from you.

An idle timer also applies. If nobody in a session speaks for 10 minutes, the
session closes.

One sound remains: a short falling tone when the **audio devices fail**. That
is the one case the interface cannot cover, because a user whose microphone
never opened would otherwise talk into nothing.

## 8. Command line

The binary is `walkie`.

| Command | Action |
|---|---|
| `walkie` | Run the application in the menu bar |
| `walkie key` | Print your ticket |
| `walkie add <ticket>` | Add a contact |
| `walkie list` | List contacts, slots, and presence |
| `walkie rm <slot|name>` | Remove a contact |
| `walkie slot <name> <n>` | Reassign a slot |
| `walkie doctor` | Check audio, permission, and connectivity |
| `walkie tui` | Run without the menu bar, for headless debugging |

## 9. Non-goals

The following are out of scope for version 1.

1. Video.
2. Text chat.
3. Recording.
4. Windows and Linux user interfaces. The core is portable. The interface is not.
5. Groups larger than 8.
6. Any server that holds user state. Relays forward packets only.
7. Acoustic echo cancellation. Version 1 assumes headphones. See TODO.md.

## 10. Success criteria

1. Mouth-to-ear latency under 50 ms on a LAN, measured by loopback.
2. Mouth-to-ear latency under 90 ms between two US coasts on a direct path.
3. A talk session starts in under 20 ms after the digit press.
4. The binary starts and reaches the menu bar in under 500 ms.
5. Idle CPU use is under 1 percent.
6. A new user goes from download to first word in under 2 minutes.
