# LOOP.md — How to work on walkie

This file tells an agent how to run one work session. Follow it in order.
Do not skip step 1.

## 0. What this product is

`walkie` is a peer-to-peer voice intercom. A user presses a shortcut, presses a
contact number, and speaks. Latency is the product.

Read these before you write code:

| File | Content |
|---|---|
| `DESIGN.md` | What the product does and why |
| `ARCHITECTURE.md` | How it is built. Verified API facts. |
| `TODO.md` | The task list. Claim work here. |
| `JOURNAL.md` | What past sessions did and learned |
| `reference/` | The visual reference images |

## 1. Start a session

Run these commands. Read all of the output.

```bash
git switch main
git pull --ff-only 2>/dev/null || true
git log --oneline -15
cat JOURNAL.md | tail -60
grep -n '^\- \[~\]' TODO.md      # what is claimed by someone else
grep -n '^\- \[!\]' TODO.md      # what is blocked
cargo build 2>&1 | tail -20
```

If `cargo build` fails on `main`, fixing it is your task. Nothing else matters
until `main` builds.

## 2. Pick a task

1. Open `TODO.md`.
2. Find the first `[ ]` task whose milestone dependencies are `[x]`.
3. Milestones run in order. Do not start M4 while M3 has open tasks, unless the
   task is clearly independent.
4. Do not pick a task marked `[~]`. Another agent holds it.
5. Prefer one task per session. Two small related tasks are acceptable.

## 3. Claim the task

Claim on `main` so other agents see it immediately.

```bash
git switch main
# edit TODO.md: change "- [ ] **T-045**" to "- [~] **T-045**" and append
#   " — branch task/045-jitter-buffer"
git add TODO.md
git commit -m "claim: T-045 adaptive jitter buffer"
git switch -c task/045-jitter-buffer
```

The branch name is always `task/<number>-<short-slug>`.

## 4. Do the work

### 4.1 Ground your facts

Never write an API call from memory. The dependencies move fast. Check the real
source:

```bash
ls ~/.cargo/registry/src/*/iroh-1*/src/
grep -rn "pub fn send_datagram" ~/.cargo/registry/src/*/iroh-1*/src/
```

`ARCHITECTURE.md` §1.1 lists the facts already checked. If you find one of them
is wrong, fix the document in the same commit.

### 4.2 Respect the real-time rules

`ARCHITECTURE.md` §2.1 forbids allocation, locks, input, output, and logging
inside the CoreAudio callbacks. A change that breaks this rule is a defect even
if it works on your machine.

### 4.3 Keep the latency budget

`ARCHITECTURE.md` §7 holds the budget. If your change adds a buffer, a thread
hop, or a copy, say so in the commit message and in `JOURNAL.md`.

### 4.4 Match the visual language

The user interface follows `DESIGN.md` §6.3 exactly. Two pixel borders. Square
corners. Monospace only. Look at `reference/aesthetic-neobrutalist-mono.png`
before you draw anything.

## 5. Check the work

Every one of these must pass before you commit.

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

If the task touches audio or network behaviour, also run the two-process test:

```bash
WALKIE_DB=/tmp/walkie-a.db cargo run -- tui &
WALKIE_DB=/tmp/walkie-b.db cargo run -- tui &
# add each to the other, confirm presence goes online
```

If the task touches the user interface, **look at it**. Do not claim a visual
task is done without seeing it.

```bash
cargo run -- snapshot --demo --out /tmp/panel.png        # every state
cargo run -- snapshot --demo --live --out /tmp/live.png  # a live session
```

`walkie snapshot` renders the panel from inside the process and writes a PNG.
Use it rather than `screencapture`. A terminal without the screen recording
permission captures the desktop with every window missing, and the result looks
like the application failed to draw when it drew correctly.

`WALKIE_PANEL_ON_START=1 cargo run` opens the panel on launch, for the cases a
snapshot cannot show.

## 6. Commit

Small commits. One logical change each. Present tense.

```
T-045: adaptive jitter buffer

Grows on a late packet. Shrinks one frame after 5 s of stability, and only
during a silence gap. Target depth is clamped to 1..=6 frames.

Adds 0 ms to the fixed budget. Removes up to 20 ms on a stable LAN.
```

Mark the task done in the **last** commit of the branch:

```bash
# edit TODO.md: "- [~] **T-045** ... — branch task/045-..." becomes "- [x] **T-045** ..."
git add TODO.md && git commit -m "T-045: mark done"
```

## 7. Write the journal

Append to `JOURNAL.md`. Keep it short. Future agents read this first.

```markdown
## 2026-08-18 — T-045 adaptive jitter buffer

- Did: implemented growth and delayed shrink in `audio/jitter.rs`.
- Learned: `opus::Decoder::decode` with an empty slice runs concealment. The
  output length must still be one full frame.
- Surprise: p95 tracking needed a 200 sample window. 50 was too noisy.
- Next: T-046 needs the FEC path. The decoder must be called with the *next*
  packet and `fec = true`.
```

Record surprises. A surprise is the most valuable thing you can leave behind.

## 8. Merge

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git switch main
git merge --no-ff task/045-jitter-buffer -m "merge T-045: adaptive jitter buffer"
git branch -d task/045-jitter-buffer
```

Never force push. Never rebase `main`. Never squash a task branch. The merge
history is the record of who did what.

## 9. If you get stuck

1. Mark the task `[!]` in `TODO.md` and write the reason on the next line.
2. Commit that to `main`.
3. Write what you tried in `JOURNAL.md`.
4. Leave the branch in place. Do not delete it.
5. Pick a different task, or stop.

Do not increase a timeout to make a test pass. A slow test is a bug in the code.

## 10. Working with other agents

Several agents may work at once.

1. Claim on `main` before you branch. This is the only lock.
2. Pull `main` before you merge.
3. Do not edit a file that another agent's claimed task clearly owns. Check the
   module map in `ARCHITECTURE.md` §3.
4. If two tasks need the same file, the second agent waits or takes another task.
5. `TODO.md` conflicts are resolved by keeping both sides. Never discard another
   agent's claim.

## 11. Session end checklist

- [ ] `main` builds.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean.
- [ ] `cargo test` passes.
- [ ] `TODO.md` shows the true state. No stale `[~]` from your session.
- [ ] `JOURNAL.md` has an entry.
- [ ] The branch is merged or the task is marked `[!]`.
