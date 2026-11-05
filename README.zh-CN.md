# Yuin CLI · 余音命令行

[English](README.md)

Rust 编写的余音终端版；命令为 **yuin**。CLI 与 TUI 共用一个后台播放服务、队列与资料库。

## 开发范围

- 五平台公开歌曲搜索、歌单与榜单：网易云 wy、酷我 kw、QQ tx、酷狗 kg、咪咕 mg；保存完整列表时核对分页与数量。
- 独立 QuickJS worker 执行 LX API 2.0 源；本地／在线导入、显式启用、能力检查、脚本快照与 SHA-256、候选更新失败回滚。
- mpv 本地／在线播放、暂停／继续、定位、音量、切歌；独立 UUID 队列、重排、顺序／随机／单曲循环；重启默认暂停。
- 个人列表、收藏、历史、最多双并发下载、完整解码和音质校验、取消／重试、离线播放。
- TUI 真实列表与搜索、分页保位、歌词、终端／深／浅主题、中英文、可选旧曲淡出和系统媒体控制。
- JSON 导入预览、备份后的事务导入、私有导出、Shell 补全、man 手册、压缩包／macOS 安装包及发行工作流。

**开发完成与验收分开记录。** 本机五平台真实目录与样本播放、Hei Music 初始化、完整下载至停源后离线播放已通过；硬件媒体键、设备切换、Intel/Linux 运行及远程发行尚需对应环境验收。详见[验证记录](docs/STATUS.md)。

## 启动

需要单独安装 Rust stable 与 mpv。macOS 可用 `brew install mpv`；本机验收为 mpv 0.41.0，项目不捆绑 mpv。

```sh
./scripts/cargo.sh build --release --locked
./target/release/yuin doctor
./target/release/yuin tui
```

`scripts/cargo.sh` 优先使用忽略于 Git 的项目私有工具链，否则使用 PATH 上的 cargo，不修改 Shell 配置。以下示例假设已将 yuin 放入 PATH：

```sh
yuin search '稻香' --platform wy --json
yuin source import '/路径/可信音乐源.js'   # 也接受 HTTP(S) 链接
yuin source list
yuin source use <音乐源UUID>
yuin play wy:123456                       # 也接受本地音频路径
yuin pause
yuin seek 90
yuin volume 70
yuin resume
yuin status --json
yuin stop                                # 停止音频，保留队列
yuin daemon stop                         # 关闭后台服务及其子进程
```

音乐源由用户选择与信任；**导入会执行候选脚本初始化**。worker 没有文件、Shell 或 Node 模块能力，但可发起网络请求，独立进程不是完整 OS 权限沙箱。不会自动切源、换平台或将同名歌曲当成同一身份。网易云以外的未缓存 ID，需要先搜索或打开列表，以获得 hash／专辑等元数据。

业务命令自动启动 daemon；查询恢复状态不会出声。doctor、paths、help、version、completions、man 不写应用数据或启动服务；daemon stop 不会启动 daemon。

更新可执行文件后，已经运行的后台服务仍使用旧代码。若新增命令提示后台无法识别，请先用更新后的程序执行 `yuin daemon stop`，再重新打开 TUI 或重试命令；队列和播放位置会保存，重启后默认暂停。

## 列表、队列、下载

```sh
yuin playlists list --platform wy
yuin playlists search '周杰伦' --platform wy --page 1
yuin playlists favorite wy:654321 --name '通勤歌单'
yuin playlists play wy:654321
yuin playlists show wy:654321 --page 2
yuin charts list --platform kw
yuin charts show kw:93 --all
yuin library create '通勤'
yuin library add <列表UUID> wy:123456
yuin library favorite wy:123456
yuin library save wy:654321 '保存完整歌单'
yuin library show <列表UUID> --page 2
yuin library play <列表UUID>
yuin library move <列表UUID> 0 3           # 从零开始的索引
yuin library remove <列表UUID> 0
yuin queue add wy:123456
yuin queue list --json
yuin queue play <队列条目UUID>
yuin queue move <队列条目UUID> 0
yuin mode shuffle                        # sequential / shuffle / repeat-one
yuin next
yuin previous
yuin history --page 1

yuin download wy:123456 --quality 128k --wait
yuin downloads list --page 1
yuin downloads show <任务UUID>
yuin downloads cancel <任务UUID>
yuin downloads retry <任务UUID>
yuin lyrics wy:123456
```

不加 `--wait` 时下载命令创建后台任务；加上后等待完成，失败返回错误。最多两首并发，可配置为一首；最多 100 个待处理任务。先写 `.part`，校验 HTTP、容量、音频签名、完整解码及请求音质后原子发布，禁止覆盖已有目标。FLAC 24-bit 从编码 STREAMINFO 核实；这些检查不证明录音的原始品质。

播放会优先重新校验同一歌曲／音质的离线文件与 SHA-256。`source stop` 禁用音乐源后，已校验文件仍能播放。正常控制／退出保存状态，播放期间每五秒 checkpoint；强制终止可能丢失最后几秒进度。

队列最多 10,000 项，并受 16 MiB 快照限制；个人列表最多 10,000 首，完整目录最多拉取 100 页。个人列表、下载与历史每页 100 项。推荐接口没有可信总数时返回 `total: null`，不伪造整站数量。顺序模式到末尾结束，错误需显式重试。

整单收藏会完整拉取分页并保存本地快照；重复收藏同一平台的同一歌单不会新增副本，也不会覆盖已经编辑的收藏。整单播放会在完整拉取成功后替换队列，保持歌单顺序并沿用当前播放模式；随机模式会随机选择首曲。空歌单、超过 10,000 首、内容截断或超出快照容量时会报错并保留原队列，不会只播放预览页。

## TUI 与配置

默认歌单页。在歌单页按 `/` 搜索当前平台的歌单，在其他页面按 `/` 搜索歌曲；输入框中 Ctrl+U 清空，再按 Enter 可恢复推荐歌单。输入状态下 Space、j/k、q 都作为文本。Tab 切换导航／内容／队列焦点；方向键或 j/k 选择；1–5 切页；Enter 打开／播放；Backspace 返回并保留选中项与滚动；`]` 追加一页；a 入队；d 下载；f 收藏；x 取消选中下载；r 刷新；P 改默认平台；l 显示歌词；Space 暂停；n/p 切歌；左右定位；+/- 音量；s 停止；q、Esc 或 Ctrl+C 退出。小窗口可用 Tab 查看收起的队列。已接受的播放即使仍在加载，退出界面也会继续。

默认采用 Catppuccin Macchiato 配色，包含深蓝灰背景、淡紫色选中行、蓝色交互提示和绿色播放状态；也可切换终端默认、深色或浅色主题。

Ghostty、WezTerm 和 Kitty 在窗口宽高足够时显示当前歌曲封面；缺少封面、终端不支持或处于 tmux/screen 时显示文字界面。封面来自平台的公开元数据，获取失败不会影响播放。

在歌单／排行榜列表或详情中按 **F（Shift+f）** 收藏整个歌单，按 **b** 播放整个歌单；“我的列表”中的歌单也可按 b 播放。小写 f 在歌曲详情中仍收藏单曲，在歌单列表中收藏选中的整个歌单。输入搜索文字时，这些按键只输入文字。

```sh
yuin config get --json
yuin config set platform wy
yuin config set play_quality 320k
yuin config set download_quality flac
yuin config set download_dir '/绝对路径/下载'
yuin config set download_concurrency 2
yuin config set theme macchiato           # macchiato（默认）/ terminal / dark / light
yuin config set language zh-CN            # zh-CN / en
yuin config set fade_seconds 5            # 0–10，默认 0
yuin config set media_keys true           # 设置后重启 daemon
```

新曲以正常音量开始后，旧曲按配置淡出；暂停／停止会回收旧播放器。macOS 媒体控制使用 AppKit 事件循环，Linux 使用用户 D-Bus 会话，不安装辅助功能监听钩子。`--platform` 仅覆盖一次请求，配置或 TUI 的 P 才保存默认值。配置通过私有临时文件和原子 rename 保存。

## 数据、迁移、发行

`paths --json` 查询独立的配置／数据／缓存／runtime。`YUIN_DATA_DIR` 可指定绝对根目录隔离测试；`YUIN_MPV` 可覆盖可执行文件，不拼接 Shell 参数。

```sh
yuin export '/绝对路径/备份.json' --include-sources
yuin import '/绝对路径/桌面版或CLI导出.json'          # 只预览
yuin import '/绝对路径/桌面版或CLI导出.json' --commit # 备份后写入
yuin completions zsh
yuin man
```

导入合并个人列表、未启用的源快照、历史和经过验证的既有下载；数据库变更使用单一事务。预览明确统计未导入的旧队列：保持当前播放器／队列与偏好。桌面版文件仅在显式选择后读取。导出不覆盖目标；仅 `--include-sources` 才携带脚本快照。

业务 JSON 有 `schema_version`；播放快照另有 daemon `instance_id` 与递增 `sequence`。运行错误写 stderr；参数错误退出码 2，平台／网络 3，音乐源 4，音频 5，资源／daemon 不可用 6。非交互命令没有 ANSI 控制序列。

[安装与打包说明](packaging/INSTALL.md) 包含压缩包、unsigned macOS `.pkg` 与 Homebrew 生成器。项目仓库为 [linq/yuin-cli](https://github.com/linq/yuin-cli)，采用 [Apache-2.0 许可证](LICENSE)。生成的 Homebrew 公式需要对应发行产物上传后才能安装。当前安装包未签名／公证；Windows 播放不在本次验收范围。

## 验证

```sh
./scripts/cargo.sh fmt --all -- --check
./scripts/cargo.sh clippy --all-targets --locked -- -D warnings
./scripts/cargo.sh test --locked
python3 scripts/verify-tui.py target/release/yuin

# 显式运行真实设备／服务检查
./scripts/cargo.sh test --locked --test daemon real_mpv_controls_and_restart -- --ignored
python3 scripts/verify-tui.py target/release/yuin --real-mpv
python3 scripts/verify-real.py target/release/yuin --source '/路径/可信源.js'
```

默认测试需要 Python 3，仅使用本地 HTTP、受控 JS、合成静音和 mpv 协议 fixture，不访问音乐平台或音频设备。仓库保留依赖说明与合成测试音频来源，不分发第三方音乐源或录制音乐。
