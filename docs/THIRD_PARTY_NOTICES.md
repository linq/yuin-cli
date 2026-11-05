# Third-party notices

The public platform adapters in `src/catalog/`, the LX API 2.0 bridge in `src/source/`, and the platform lyric adapters derive their behavior from the user's Yuin desktop reference implementation (`mymusic`). Its endpoint parameters, QQ signature algorithm/constants and Migu search parameters reference LX Music Desktop 2.12.6 by lyswhut, licensed under Apache-2.0. The LX license is retained at [licenses/LX-Music-Apache-2.0.txt](licenses/LX-Music-Apache-2.0.txt). Upstream: https://github.com/lyswhut/lx-music-desktop.

The Rust hosts and adapters are separate implementations. No LX application, music files or third-party music-source scripts are bundled. Cargo dependencies retain their own licenses; release packaging must include this notice and the retained license. The repository's own license remains undecided and publishing to crates.io is disabled.
