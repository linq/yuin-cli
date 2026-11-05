# Yuin CLI · 余音命令行

[简体中文](README.zh-CN.md)

Yuin in your terminal: a Rust CLI and TUI sharing one background player, queue and music library. The executable is **yuin**.

## Implemented

- Public track search, playlists and charts for NetEase (`wy`), Kuwo (`kw`), QQ (`tx`), Kugou (`kg`) and Migu (`mg`). Stable platform IDs retain the metadata required by LX sources. Complete collection saves verify paging and counts.
- LX API 2.0 sources in independent QuickJS processes: local/online import, explicit activation, capability checks, hash-verified snapshots and candidate updates with rollback. Sources receive HTTP, timers, byte/crypto/zlib helpers; no filesystem, shell or Node modules are exposed. Source scripts can access the network: choose scripts you trust. Import initializes the candidate script.
- Local/online mpv playback, shared controls, independent queue entry UUIDs, queue reordering, sequential/shuffle/repeat-one modes, position/volume recovery paused after restart, and optional old-track fade.
- Personal lists, favorites, history, validated offline downloads, cancellation/retry, lyrics with local caching, themes and English/Chinese UI. System media keys are optional.
- Private versioned Unix RPC, a single daemon/SQLite writer, bounded requests, cancellation and generation checks. TUI exit leaves accepted playback running, including a track still loading.
- Native/desktop JSON import preview, backed-up transactional import, private exports, shell completions, a manual, portable packaging, optional macOS installer and draft-release CI.

Implementation and validation are separate. Apple Silicon real checks passed for the five platform catalogs and sample playback, Hei Music initialization and one complete download/offline flow. Hardware media keys, device switching, Intel/Linux execution and GitHub release jobs require separate acceptance. See [STATUS](docs/STATUS.md) and the [technical design](docs/TECHNICAL_DESIGN.zh-CN.md).

## Build and run

Install Rust stable and **mpv** separately (`brew install mpv` on macOS, or your Linux distribution's mpv package). The tested backend is mpv 0.41.0; mpv is not bundled.

```sh
./scripts/cargo.sh build --release --locked
./target/release/yuin doctor
./target/release/yuin tui
```

The wrapper uses an ignored checkout-local toolchain when available, otherwise cargo on PATH; it does not install tools or edit shell profiles. The following examples assume `yuin` is on PATH:

```sh
yuin search '稻香' --platform wy --json
yuin source import '/path/to/trusted-source.js'  # also accepts HTTP(S)
yuin source list
yuin source use <source-uuid>
yuin play wy:123456                            # or a local audio path
yuin pause
yuin seek 90
yuin volume 70
yuin resume
yuin status --json
yuin stop                                     # retain the queue
yuin daemon stop                              # also stop child processes
```

Business commands start the daemon as needed; status never plays restored audio. `doctor`, `paths`, help, version, completions and man are read-only. `daemon stop` never starts a daemon. Track IDs from search are stable; other than NetEase, uncached IDs require a search or collection lookup first so hash/album metadata is available. No platform substitution or automatic source switching occurs.

## Lists, queues and offline files

```sh
yuin playlists list --platform wy
yuin playlists show wy:654321 --page 2
yuin charts list --platform kw
yuin charts show kw:93 --all
yuin library create 'Commute'
yuin library add <list-uuid> wy:123456
yuin library favorite wy:123456
yuin library save wy:654321 'Saved complete collection'
yuin library show <list-uuid> --page 2
yuin library move <list-uuid> 0 3              # indices are zero-based
yuin library remove <list-uuid> 0
yuin queue add wy:123456
yuin queue list --json
yuin queue play <entry-uuid>
yuin queue move <entry-uuid> 0
yuin mode shuffle
yuin next
yuin previous
yuin history --page 1

yuin download wy:123456 --quality 128k --wait
yuin downloads list --page 1
yuin downloads show <job-uuid>
yuin downloads cancel <job-uuid>
yuin downloads retry <job-uuid>
yuin lyrics wy:123456
```

A download without `--wait` enqueues a persistent task; `--wait` waits for success or returns an error. At most two files transfer concurrently (configurable to one); pending tasks are limited to 100. Temporary files are removed on failure/cancel. Completed files are published atomically without overwriting a target. HTTP status, size, magic, full decoding and requested quality are checked. FLAC 24-bit is verified from STREAMINFO, not decoded floating-point samples; encoding checks cannot certify the original recording. Same-ID/quality offline files are revalidated against their SHA-256 before playback. `source stop` disables the source; validated matching downloads still play.

The queue is limited to 500 entries and a 1 MiB snapshot. Personal lists allow 10,000 tracks, complete catalog fetches up to 100 pages. Personal-list pages and download/history pages contain 100 items. If a recommendation API does not provide a trustworthy total, JSON reports `total: null`. EOF stops at the end in sequential mode; failures require an explicit retry.

## TUI and preferences

The default page is Playlists. `/` edits a search query; text input owns Space/j/k/q. Tab cycles navigation/content/queue focus; arrows or j/k select; 1–5 change pages; Enter opens a collection or plays; Backspace returns without losing selection/scroll; `]` appends a page; a queues, d downloads, f favorites, x cancels a selected download, r refreshes, P changes the default platform, l toggles synchronized lyrics, Space pauses/resumes, n/p changes tracks, ←/→ seeks, +/- adjusts volume, s stops and q/Esc/Ctrl+C leaves. On small terminals Tab can reveal the collapsed queue.

```sh
yuin config get --json
yuin config set platform wy
yuin config set play_quality 320k
yuin config set download_quality flac
yuin config set download_dir '/absolute/path/to/downloads'
yuin config set download_concurrency 2
yuin config set theme terminal                 # terminal, dark, light
yuin config set language en                    # en, zh-CN
yuin config set fade_seconds 5                 # 0–10; default 0
yuin config set media_keys true                # restart the daemon afterward
```

The old track fades after the new track starts at normal volume; stop/pause cancels outgoing audio. macOS media controls use a background AppKit event loop; Linux uses the user D-Bus session. No accessibility hook is installed. `--platform` affects one query; setting config or using P changes the saved default. Config saves use a private temporary file and atomic rename.

## Data, import and distribution

`yuin paths --json` reports independent config/data/cache/runtime locations. SQLite migrations back up existing databases. Playback position is checkpointed every five seconds and on controls/shutdown. `YUIN_DATA_DIR` selects an absolute isolated root; `YUIN_MPV` overrides the executable without shell arguments.

```sh
yuin export '/absolute/path/backup.json' --include-sources
yuin import '/absolute/path/desktop-or-native-export.json'        # preview only
yuin import '/absolute/path/desktop-or-native-export.json' --commit
yuin completions zsh
yuin man
```

Import merges personal lists, source snapshots inactive, history and only existing verified offline files. It backs up first and writes database changes in one transaction. The preview explicitly reports old queue entries excluded from import: it preserves the current player/queue and preferences. Desktop library files are only read when selected explicitly. Export never overwrites an existing destination; source scripts are included only with `--include-sources`.

Every business JSON response has `schema_version`; playback snapshots also have daemon `instance_id` and monotonic `sequence`. Runtime errors go to stderr; exit codes include usage 2, platform/network 3, music source 4, audio 5 and missing resources/daemon 6. No ANSI is emitted by non-interactive commands.

Local packaging and the Homebrew formula generator are documented in [packaging/INSTALL.md](packaging/INSTALL.md). The project repository is [linq/yuin-cli](https://github.com/linq/yuin-cli), licensed under [Apache-2.0](LICENSE). Release artifacts must be uploaded before the generated Homebrew formula can be installed. Local `.pkg` files are unsigned and not notarized. Windows playback is outside this implementation's accepted platform scope.

## Checks

```sh
./scripts/cargo.sh fmt --all -- --check
./scripts/cargo.sh clippy --all-targets --locked -- -D warnings
./scripts/cargo.sh test --locked
python3 scripts/verify-tui.py target/release/yuin

# Explicit real-device/service checks (generated silence or a trusted source)
./scripts/cargo.sh test --locked --test daemon real_mpv_controls_and_restart -- --ignored
python3 scripts/verify-tui.py target/release/yuin --real-mpv
python3 scripts/verify-real.py target/release/yuin --source /path/to/trusted-source.js
```

Default tests require Python 3 and use local HTTP, controlled JS, generated silence and an mpv protocol fixture. They do not access music platforms or audio devices. Dependency notices and synthetic test-audio provenance are retained; no third-party music-source script or recorded music is distributed.
