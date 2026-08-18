# CLAUDE.md

## Start here

Read `LOOP.md` and follow it. It defines how to run a work session on this
project. Do not start work before you read it.

Short version:

1. Read `LOOP.md`, `DESIGN.md`, `ARCHITECTURE.md`, and the tail of `JOURNAL.md`.
2. Claim a task in `TODO.md` on `main`, then branch `task/<n>-<slug>`.
3. Do the work. Check it. Commit.
4. Write a `JOURNAL.md` entry. Merge with `--no-ff`.

## What this is

`walkie` is a peer-to-peer, low-latency voice intercom for macOS. It is one CLI
binary that lives in the menu bar. It uses `iroh` for transport and Opus for
audio. There are no accounts and no servers that hold state.

## Rules that override defaults

1. **Latency is the product.** Never add a buffer, a thread hop, or a copy to
   the audio path without recording the cost in `ARCHITECTURE.md` §7.
2. **The audio callbacks are real-time.** No allocation. No locks. No input or
   output. No logging. See `ARCHITECTURE.md` §2.1.
3. **Never write an API call from memory.** Check the crate source in
   `~/.cargo/registry/src/`. `iroh` 1.0 renamed most of its types.
4. **Never raise a timeout to make something pass.** A long run means a loop
   that does not terminate. Find the real cause.
5. **Every AppKit call runs on the main thread.** The tokio runtime lives on a
   spawned thread.
6. **The visual style is fixed.** Two pixel borders, square corners, monospace,
   hard offset shadows. See `DESIGN.md` §6.3 and `reference/`.

## Writing style

Use Simplified Technical English in all documents and comments. Short sentences.
Active voice. One instruction per sentence. One word for one meaning.

## Commands

```bash
cargo build                                  # debug
cargo clippy --all-targets -- -D warnings    # must be clean
cargo test                                   # must pass
cargo run -- doctor                          # check the local machine
cargo run -- tui                             # headless, for two-process tests
./scripts/build-release.sh                   # signed release binary
```

`WALKIE_DB` overrides the database path. Use it to run two instances at once.
`WALKIE_LOG=debug` sets the log filter.
