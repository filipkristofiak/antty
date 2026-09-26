# Installation

## Prerequisites

A Rust toolchain via [rustup](https://rustup.rs), 1.88 or newer (edition 2024
plus let-chains, used in `src/cli.rs`).

## Install

```
cargo install --path .
```

This puts the binary at `~/.cargo/bin/antty`; make sure that's on `PATH`.

With a Homebrew-installed rustup, plain `cargo` may not be on `PATH`. Use:

```
rustup run stable cargo install --path .
```

## Running from source

```
cargo run --release -- --project <dir>
```

## Uninstall

```
cargo uninstall antty
```

## Files read and written

- Reads `~/.omp/agent/sessions`
- Writes `~/.local/state/antty/<project path with / replaced by _>.jsonl`
