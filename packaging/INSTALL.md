# Install and runtime requirements

Yuin is a Rust CLI/TUI. Audio decoding and playback are built in (Symphonia and Rodio/CPAL). No external player is required. macOS uses CoreAudio; Linux uses ALSA. A music source must be imported and selected explicitly for online audio.

## Portable archive

Verify the adjacent SHA-256 file, extract the archive, and copy `bin/yuin` to a user-controlled directory on PATH. macOS needs no additional audio package. Linux requires the distribution ALSA runtime (`libasound2` / `libasound2t64`); source builds also need `libasound2-dev` and `pkg-config`. The archive includes manual pages and bash/zsh/fish completions for manual installation. It does not change shell profiles, register a login service or start audio during installation.

## macOS installer

The optional `.pkg` installs the binary under `/usr/local/bin`, manual under `/usr/local/share/man/man1` and notices under `/usr/local/share/doc/yuin`. It does not start a daemon or install a separate player. Local builds are unsigned and not notarized; signing and notarization require a developer identity and credentials supplied for release. The portable archive is available without a system-wide install.

## Homebrew

`scripts/homebrew-formula.py` generates a formula with exact checksums from built archives. It requires the real repository URL, version and chosen project license. The formula has no mpv dependency; only Linux declares `alsa-lib` for system audio. Generated URLs must correspond to uploaded release artifacts before a tap is usable; the generator does not upload anything.

## Data and removal

`yuin paths --json` reports application data and runtime locations. `yuin daemon stop` stops playback and child processes. Removing the executable does not remove the user's library, source snapshots or downloaded audio. Data deletion is an explicit user action. No desktop Yuin library is modified automatically.

System media controls are optional: `yuin config set media_keys true`, then stop/restart the daemon. macOS requires the daemon's AppKit event loop; Linux requires a user D-Bus session. Themes are macchiato (default)/terminal/dark/light, language is en/zh-CN, and optional old-track fade is configured through `fade_seconds` (0–10, default 0).

The package includes dependency SPDX information and the available upstream license/notice files. Third-party music-source scripts and downloaded audio are never included in release artifacts.

## Background playback and upgrades

`yuin pause` retains the current position, `yuin stop` stops audio while retaining the queue, and `yuin daemon stop` closes the background service. Exiting the TUI keeps playing. After replacing an older binary, stop the old daemon before starting the new TUI. Existing separately installed mpv applications are not modified by Yuin.
