# Third-party notices

The public platform adapters in `src/catalog/`, the LX API 2.0 bridge in `src/source/`, and the platform lyric adapters derive their behavior from the user's Yuin desktop reference implementation (`mymusic`). Its endpoint parameters, QQ signature algorithm/constants and Migu search parameters reference LX Music Desktop 2.12.6 by lyswhut, licensed under Apache-2.0. The LX license is retained at [licenses/LX-Music-Apache-2.0.txt](licenses/LX-Music-Apache-2.0.txt). Upstream: https://github.com/lyswhut/lx-music-desktop.

The Rust hosts and adapters are separate implementations. No LX application, music files or third-party music-source scripts are bundled. Cargo dependencies retain their own licenses; release packaging must include this notice and the retained license. The repository is licensed under Apache-2.0; publishing to crates.io is disabled.

Audio decoding reuses Symphonia (MPL-2.0); mixing/resampling uses Rodio (MIT OR Apache-2.0), and CPAL (Apache-2.0) connects to native audio devices. macOS uses system CoreAudio frameworks and Linux uses system ALSA. No mpv binary or library is linked or bundled. The packaging script includes Cargo dependency licenses and an SPDX inventory.

`tests/fixtures/audio-formats.json` contains only generated stereo silence (44.1 kHz, two seconds), encoded as MP3 and AAC/M4A using FFmpeg. These are synthetic test signals, not recorded music. The fixture generator is not a runtime or test dependency. Existing FLAC fixtures are also synthetic.
