# Yuin CLI · 余音命令行

[简体中文](README.zh-CN.md)

A Rust terminal companion to Yuin, combining scriptable commands and an interactive TUI. The repository is named **yuin-cli** and the executable is **yuin**.

## Current status

This is a project scaffold, not a working music player. Implemented: help/version, a read-only environment report, application paths, and a keyboard-navigable TUI shell. Music catalogs, LX sources, playback, downloads, and a shared daemon are planned in the [technical design](docs/TECHNICAL_DESIGN.zh-CN.md).

## Run

With a Rust stable toolchain:

```sh
cargo run -- --help
cargo run -- doctor --json
cargo run -- paths
cargo run -- tui
```

Running `yuin` without arguments opens the TUI in an interactive terminal; redirected invocation prints help. Use ↑/↓, j/k, Tab, or 1–5 to switch pages. Press q, Esc, or Ctrl+C to leave. This scaffold does not start audio.

This checkout also has an ignored, project-local Rust toolchain. If Rust is not on your PATH, use:

```sh
./scripts/cargo.sh run -- --help
./scripts/cargo.sh run -- tui
./scripts/cargo.sh fmt --all -- --check
./scripts/cargo.sh clippy --all-targets --locked -- -D warnings
./scripts/cargo.sh test --locked
```

The wrapper uses the local toolchain when present and otherwise uses `cargo` on PATH. It does not install tools or modify shell profiles. `Cargo.lock` is committed for reproducible dependency resolution; `rust-toolchain.toml` tracks stable.

## Planned runtime

- CLI and TUI share a Rust daemon for persistent playback and queue state.
- Rust platform adapters provide catalogs; LX-compatible JavaScript sources resolve playback URLs in an isolated QuickJS worker.
- The first audio backend will control a separately installed **mpv** process via local IPC. mpv is not bundled or installed by this scaffold.
- Data lives in dedicated Yuin CLI directories; the macOS application's library is imported explicitly, never edited in place.

No third-party source scripts, music files, or desktop application code are bundled. An open-source license has not been selected yet; package publishing is disabled.
