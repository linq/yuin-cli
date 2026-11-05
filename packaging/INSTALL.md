# Install and runtime requirements

Yuin is a Rust CLI/TUI. Its audio backend is an external mpv executable; mpv is not bundled. The native acceptance environment uses mpv 0.41.0. A music source must be imported and selected explicitly for online audio.

## Portable archive

Verify the adjacent SHA-256 file, extract the archive, and copy `bin/yuin` to a user-controlled directory on PATH. Install mpv separately (`brew install mpv` on macOS or the Linux distribution's mpv package). The archive includes manual pages and bash/zsh/fish completions for manual installation. It does not change shell profiles, register a login service or start audio during installation.

## macOS installer

The optional `.pkg` installs the binary under `/usr/local/bin`, manual under `/usr/local/share/man/man1` and notices under `/usr/local/share/doc/yuin`. It does not install mpv or start a daemon. Local builds are unsigned and not notarized; signing and notarization require a developer identity and credentials supplied for release. The portable archive is available without a system-wide install.

## Homebrew

`scripts/homebrew-formula.py` generates a formula with exact checksums from built archives. It requires the real repository URL, version and chosen project license. The formula declares mpv as a dependency. Generated URLs must correspond to uploaded release artifacts before a tap is usable; the generator does not upload anything.

## Data and removal

`yuin paths --json` reports application data and runtime locations. `yuin daemon stop` stops playback and child processes. Removing the executable does not remove the user's library, source snapshots or downloaded audio. Data deletion is an explicit user action. No desktop Yuin library is modified automatically.

System media controls are optional: `yuin config set media_keys true`, then stop/restart the daemon. macOS requires the daemon's AppKit event loop; Linux requires a user D-Bus session. Themes are macchiato (default)/terminal/dark/light, language is en/zh-CN, and optional old-track fade is configured through `fade_seconds` (0–10, default 0).

The package includes dependency SPDX information and the available upstream license/notice files. Third-party music-source scripts and downloaded audio are never included in release artifacts.
