# Installation

## Prerequisites

A Rust toolchain via [rustup](https://rustup.rs), 1.88 or newer (edition 2024
plus let-chains, used in `src/dirs.rs`).

## Install

```
cargo install --path .
```

This installs two binaries into `~/.cargo/bin`: `antty` (one project's timeline) and `formi` (an overview of every project with coding-agent sessions); make sure that directory is on `PATH`.

With a Homebrew-installed rustup, plain `cargo` may not be on `PATH`. Use:

```
rustup run stable cargo install --path .
```

## Running from source

```
cargo run --release -- --project <dir>
```

For formi:

```
cargo run --release --bin formi
```

## Uninstall

```
cargo uninstall antty
```

This removes both `antty` and `formi`.

## Files read and written

- Reads `~/.omp/agent/sessions` (omp); `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects` (Claude Code); and `$CODEX_HOME/sessions`, else `~/.codex/sessions` (Codex CLI). Codex session titles come from `session_index.jsonl` next to the sessions directory.
- Writes `$XDG_STATE_HOME/antty/<project path with / replaced by _>.jsonl` (default `~/.local/state/antty/<project path with / replaced by _>.jsonl`)
- formi reads the same three session roots and writes its activity cache to `$XDG_STATE_HOME/antty/formi/activity.bin` (default `~/.local/state/antty/formi/activity.bin`). Deleting it only makes the next start rescan every session.
