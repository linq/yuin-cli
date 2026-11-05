# Yuin CLI · Rust 命令行与终端界面技术方案

日期：2026-11-04（Asia/Shanghai）

## 1. 名称、定位与本次交付

- 产品名称：**Yuin CLI · 余音命令行**，延续 Yuin 品牌。
- 项目目录／建议仓库名称：`yuin-cli`。
- 可执行命令：`yuin`。
- 用户已确认：Rust 开发；同时支持脚本化命令行和交互式 TUI。
- 当前创建位置：`/Users/linq/Dev/yuin-cli`。参考桌面项目：`/Users/linq/Dev/mymusic`。

S0–S4 的功能代码、本地打包与草稿发行工作流已落地，当前版本仍为 0.1.0-dev。五平台真实目录和样本播放、Hei Music worker、完整下载至停源后离线播放均已验证。硬件媒体键、设备切换、多架构运行、签名公证及远程发行仍需对应环境或发行元数据，不能视为全部验收完成。范围与结果以 [STATUS](STATUS.md) 为准。下文保留架构决策与验收门槛。

## 2. 产品范围与实施原则

第一阶段以当前 Apple Silicon Mac 为运行验收环境，代码结构兼顾 Linux；Windows 的音频、进程和 IPC 适配后续单独验收。CI 中编译 Linux 不代表已完成 Linux 真实音频验收。

最终基础功能对齐桌面版：搜索、公开歌单、排行榜、个人列表、队列、播放、下载和离线播放；支持 LX API 2.0 JavaScript 音乐源的本地／在线导入。先让一个平台的真实流程闭环，再补其余平台。

两种使用方式共享状态：终端内搜索和播放；另一终端用 `yuin pause` 控制；关闭 TUI 后音乐继续播放。TUI 的布局参考音乐播放器的分区和信息层次，不追求桌面窗口的像素复刻。

初版不要求平台账号、跨设备同步、自动跟随远程歌单、本地目录扫描、终端封面图片协议、无缝切歌和交叉淡化。歌词、系统媒体键和可配置的旧曲淡出在基础链路稳定后加入。后续淡出可沿用桌面版的 5 秒默认值，新曲直接以正常音量播放。

## 3. 架构决策

```text
CLI（clap） ──────┐
                  ├─ 本地版本化 RPC ── Rust daemon ── Library / Config / SQLite
TUI（Ratatui） ───┘                         │
                                          ├─ Catalog adapters ── 平台公开目录
                                          ├─ Source supervisor ── QuickJS worker
                                          ├─ Playback backend ── mpv 子进程
                                          └─ Download manager ── 音频文件
```

### CLI 与 TUI 共用常驻核心

播放状态不能属于某次短暂的 `yuin play` 命令，否则命令退出就会终止音频，其他终端也无法控制。采用按当前用户运行的 daemon，统一拥有队列、音乐源实例、音频引擎和数据库写入权。CLI/TUI 是客户端，禁止各自维护第二份播放队列。

用户操作转换为 `AppCommand`；核心通过一个串行处理循环改变状态，再发布版本递增的 `AppEvent` 和快照。网络查询、脚本解析、数据库操作与音频回调都不能阻塞该循环。高频播放位置由时间基准推导，事件节流；队列和收藏只在实际变化时保存。

### 第一版音频后端选择 mpv IPC

采用 Rust 启动并控制独立的 mpv 进程，负责 URL 流媒体、解码、缓冲、播放、暂停、跳转与设备输出。Rust 负责业务状态，不把队列真相交给 mpv。`PlaybackBackend` 的接口只暴露 load / play / pause / seek / volume / stop 和状态事件。

理由：初期最大的兼容成本在平台接口与 LX 源协议；已有成熟音频后端可以缩短真实闭环，避免同时重写 HTTP 可定位缓存、各种容器解复用和音频设备恢复。mpv 官方提供 JSON IPC 控制接口。[mpv 手册](https://mpv.io/manual/stable/#json-ipc)

代价：第一版需要另行安装 mpv，发布包不是完全自包含，也不应宣传为全部依赖均由 Rust 实现。已接入本地／在线 mpv 播放；本机开发验收通过 Homebrew 单独安装，项目没有捆绑它。doctor 只读检查 PATH 或 YUIN_MPV 指定的可执行文件。后续若需要自包含分发，再评估内嵌 libmpv 的构建／许可成本，或用 rodio + Symphonia 替换这个后端；不同时维护两套生产音频实现。

mpv 以参数数组启动，不拼接 Shell 命令；禁用用户配置、外部脚本自动加载、终端输入和视频窗口，使用私有继承 socketpair（`--input-ipc-client=fd://0`），不创建 mpv 公共控制 socket；mpv 在连接断开时退出，也覆盖 daemon 被强制终止的情况。Rust 侧只发送允许的播放命令，不把它的任意命令接口直接暴露给客户端。

### LX 源采用独立 QuickJS worker

音乐源保持 JavaScript 形式，宿主用 Rust + rquickjs。worker 与 daemon 是不同进程；可由同一个 `yuin` 二进制以内部模式启动，不依赖 Node.js。JS 虚拟机的内存、栈和中断限制可以通过 rquickjs 配置，外部进程超时终止作为第二层兜底。[Runtime API](https://docs.rs/rquickjs/latest/rquickjs/struct.Runtime.html)

独立进程用于故障隔离，不能称为完整 OS 权限沙箱。只注册必要 HTTP、计时器、字节、压缩和加密能力，不提供文件系统、Shell、任意原生模块或完整 Node 环境。脚本 HTTP 能访问网络，因此导入的脚本仍需由用户选择和信任。

## 4. Rust 技术栈

| 部分 | 选择 | 说明 |
| --- | --- | --- |
| 命令解析 | clap | 已接入；帮助、参数约束和后续补全 |
| 终端界面 | Ratatui + Crossterm | 已接入导航骨架；终端初始化、退出恢复与按键事件 |
| 错误与输出 | anyhow；后续业务错误用 thiserror | stderr 日志、stdout 业务输出分开 |
| 序列化 | serde + serde_json | 已接入只读 JSON 输出；后续 RPC 与数据模型 |
| 应用路径 | directories | 已接入独立目录查询 |
| 异步与进程 | Tokio | S1 已接入超时、子进程监督、本地 IPC；网络后续 |
| HTTP | reqwest | 已接入复用 Client、流式下载、请求取消与超时 |
| 存储 | SQLite + rusqlite（bundled） | S1 已接入初始迁移、事务队列与进度恢复；下载后续 |
| 配置 | TOML | 已接入，daemon 校验后私有原子保存 |
| JS 引擎 | rquickjs / QuickJS | 已接入 worker，并通过受控脚本与 Hei Music 样本检查 |
| 音频 | mpv JSON IPC | S1 已接入本地播放；外部运行依赖 |
| 下载校验 | 媒体头解析 + Symphonia | 已接入完整解码、码率／FLAC STREAMINFO 校验 |

只添加当前实际使用的依赖；S1 增加 Tokio、rusqlite（bundled）、UUID 与 SHA-256，不预先引入后续阶段依赖。应用提交 Cargo.lock；Rust toolchain 跟随 stable，本机首次验证版本记录于 STATUS。具体后续依赖版本在接入时通过官方文档与构建确定，不将本文作为永久版本锁。

参考：[clap](https://docs.rs/clap/latest/clap/)、[Ratatui](https://docs.rs/ratatui/latest/ratatui/)、[Crossterm](https://docs.rs/crossterm/latest/crossterm/)、[reqwest](https://docs.rs/reqwest/latest/reqwest/)、[rusqlite](https://docs.rs/rusqlite/latest/rusqlite/)、[directories](https://docs.rs/directories/latest/directories/struct.ProjectDirs.html)。

## 5. 模块与代码组织

当前保持一个 Cargo package，避免接口未稳定就过早拆成多个 crate。随实现增加以下模块：

```text
src/
  main.rs             # 启动分派；当前已有
  cli.rs              # CLI 命令定义；当前已有
  doctor.rs           # 只读诊断；当前已有
  paths.rs            # 平台目录；当前已有
  tui.rs + tui/       # 交互、异步请求、浏览路由与渲染
  app/                # 命令、事件、状态转换、业务服务
  domain/             # Track、QueueEntry、Playlist、SourceCapability
  catalog/            # kw / kg / tx / wy / mg 适配器
  source/             # 导入、元数据、宿主监督、LX bridge
  playback/           # 后端接口、mpv 适配器、播放状态机
  download/           # 调度、临时文件、校验与离线索引
  storage/            # SQLite、迁移、备份与导入
  daemon/             # 单实例、RPC、订阅和生命周期
```

模块边界隐藏具体协议与文件路径。目录模块返回领域类型，不让 CLI/TUI 读取平台原始 JSON；音频后端不决定收藏与列表；数据库不直接发起网络请求。

关键领域定义：

- `TrackId = platform + song_id`，不按歌名猜测或跨平台合并。
- `Track` 保存标题、艺人、专辑、时长、封面与平台源参数；LX 所需 hash、albumId、音质映射等不能在展示时丢失。
- `QueueEntry` 有独立 UUID，同一首歌可在队列出现多次。
- `CollectionPage<T>` 包含 items、total 和是否还有后续页；保存整单必须拉取完整内容，不能把第一页误报为整单。
- `ResolvedMedia` 是短期结果，包含 URL、必要请求头及已知有效期；持久队列不以临时播放 URL 为身份。

## 6. CLI 设计（完整目标接口）

```sh
yuin                              # 交互终端进入 TUI
yuin search "周杰伦" --platform wy
yuin search "周杰伦" --platform wy --json
yuin play wy:123456                # 使用稳定 ID，不依赖上一条搜索的行号
yuin pause
yuin resume
yuin next
yuin seek 90                      # 定位到 90 秒
yuin volume 70
yuin status --json
yuin queue list
yuin queue add wy:123456
yuin playlists list --platform wy
yuin playlists show wy:654321
yuin charts list --platform wy
yuin library create "通勤"
yuin library add <list-id> wy:123456
yuin download wy:123456 --quality flac
yuin source import ./source.js
yuin source import https://example.com/source.js
yuin source list
yuin source use <source-id>
yuin source update <source-id>
yuin config set platform wy
yuin daemon stop
```

当前实际支持的子命令以 `yuin --help` 为准。`play <path-or-platform:id>` 与 `queue add <path-or-platform:id>` 接受本地文件或平台 ID，`queue play <entry-uuid>` 播放已有条目，并提供 pause/resume/stop/next/previous/seek/volume/status、队列查询／移除／清空和 daemon start/stop。平台目录、音乐源、个人列表、配置和下载命令现已实现，另有歌词、迁移、补全和手册入口。搜索先保存完整 Track 元数据，播放时通过 ID 找到缓存元数据或查询平台详情；缺失时明确报错，不将 ID 当成 URL。

输出约定：人类可读表格为默认；`--json` 返回有 schema_version 的稳定结构，进度和诊断仅进入 stderr；非交互模式不自动选择结果、不进入 TUI、不输出 ANSI 控制序列。建议退出码：0 成功、1 内部错误、2 参数错误、3 平台／网络不可用、4 音乐源错误、5 播放／音频设备错误、6 资源不存在或 daemon 不可用。

`--platform` 只覆盖本次请求；`config set platform` 和 TUI 平台切换才修改默认平台。避免自动化脚本意外改变用户长期偏好。

## 7. TUI 设计

```text
┌ Yuin                         平台：网易云      音乐源：已启用 ┐
│ 搜索      │ 歌曲／歌单列表                         │ 队列    │
│ 歌单      │ 标题  艺人  专辑  时长                  │ ...     │
│ 排行榜    │                                       │         │
│ 我的列表  │                                       │         │
│ 下载      │                                       │         │
├ 当前歌曲 · 艺人     播放进度                  音量／模式 ────┤
└ 快捷键说明／状态消息                                        ┘
```

初次进入默认歌单页。每个路由保存关键词、当前结果、分页、选中 TrackId、滚动偏移；返回列表时按 ID 恢复，追加一页不跳回开头。短窗口优先保留歌曲名和艺人，队列可收起；CJK 字符按终端显示宽度布局。

目标快捷键：`/` 搜索、方向键或 j/k 选曲、Enter 播放、Space 暂停／继续、n/p 切歌、左右键定位、+/- 音量、Tab 切换焦点、数字键切换页面、q 退出界面。输入框获得焦点时，Space、j/k 等作为文本输入处理。

q 只断开界面，daemon 播放继续；停止播放和关闭 daemon 是明确的独立操作。TUI 每次进入获取全量快照，再订阅增量事件；网络请求有取消和版本号，旧查询完成后不能覆盖新结果。界面按需重绘，高频事件合并，播放进度重绘上限建议 20–30 fps；目录查询与数据库工作不在渲染线程执行。

已加入播放状态栏、真实目录／搜索／个人列表／下载、本地队列、Tab 焦点切换、Enter 播放、Space 暂停／继续、n/p 切歌、左右定位、+/- 音量、s 停止，默认歌单页。选中项按 QueueEntry UUID 保留，小窗口合并为导航与队列两列；页面内容仍明确提示目录功能待实现。搜索输入拥有文本按键，路由保存列表与滚动，旧查询通过版本号失效，追加页与返回保位。TUI 断线后连接已有 daemon 并获取全量快照，不自动重启被显式停止的服务。

## 8. 目录查询与音乐源兼容

桌面版边界已经明确：`MusicCore/Catalog.swift` 查询五个平台的公开元数据；`SourceRuntime.swift` 和 `MusicSourceHost/bridge.js` 只把歌曲信息交给启用的自定义源解析音频 URL。Rust 版沿用这个业务分工，迁移协议与测试样例，不直接链接 Swift 或运行桌面应用。

实施顺序：先接网易云完成搜索／详情／公开列表，再接酷我、QQ、酷狗、咪咕；每个平台报告真实能力，某个平台失效不伪造结果，也不静默换成另一平台的同名歌曲。平台 API 属外部依赖，需要真实验收，既有桌面版通过不能替代 Rust 版验证。

LX bridge 目标：

1. `window.lx`、API 2.0.0、currentScriptInfo、EVENT_NAMES。
2. request / inited 生命周期、能力声明、musicUrl 请求与质量检查。
3. HTTP 回调及取消，计时器，Promise job 驱动，Buffer 相关字节编码。
4. MD5、randomBytes、AES CBC/ECB、LX 所需 RSA 填充语义和 zlib。
5. 初始化失败、语法错误、无限循环、未完成 Promise、重复初始化和子进程退出的错误传播。

worker 与监督进程使用有 request_id 的 JSON Lines 协议，stdout 仅输出协议，日志写 stderr；限制单帧大小。初始建议：脚本上限 9 MiB，初始化 15 秒，解析 65 秒，VM 内存 128 MiB；这些是配置起点，需以真实脚本测量调整。超时先协作取消，500 ms 后仍未退出则终止进程并回收全部请求。

源导入保存脚本快照、SHA-256、元数据和原始链接。更新先在候选 worker 中初始化，成功后原子替换；失败保留原版本。多源可保存，同时只激活一个，不自动切源。已用方案指定的 Hei Music 样本执行真实检查，版本／哈希与边界见 STATUS；脚本不随仓库或安装包分发。

## 9. daemon、IPC 与生命周期

macOS/Linux 使用 Unix domain socket；runtime 目录只允许当前用户访问，socket 权限 0600。父目录和所有权需要校验；macOS 使用 getpeereid、Linux 使用对等凭据核验连接用户。客户端拿不到任意 Shell 或 mpv 命令转发能力。Windows 后续使用命名管道。

协议包含 version、request_id、method、params 和结构化错误；事件订阅有递增 sequence，丢失事件时重新请求 snapshot。请求帧限制、超时、连接断开取消、事件背压和慢客户端处理都属于实现要求。

首次业务命令或 TUI 自动启动 daemon；doctor/paths/help 不启动任何服务。启动用文件锁避免双实例，socket 存在时先握手确认状态，不能仅凭旧路径删除另一进程的端点。daemon 与 mpv/source worker 分别监督；daemon 退出时回收子进程，避免孤立播放器继续出声。

音频状态机：Idle → Resolving → Buffering → Playing ↔ Paused → Ended/Error。加载事件携带 generation，切歌后旧 URL／旧 mpv 事件不能改变新歌。正常重启恢复队列与进度，默认暂停，不因 CLI 查询 status 而自动出声。设备断开进入可重试状态，不反复自动重启声音。

## 10. 存储、配置与迁移

使用 directories::ProjectDirs::from("dev", "linq", "yuin-cli") 生成独立路径。以 `yuin paths --json` 的实际输出为准；macOS 的配置／数据位于 `~/Library/Application Support/dev.linq.yuin-cli`，缓存在对应系统缓存目录。doctor/paths 仍只读；首次业务命令才创建 SQLite。S1 提供 YUIN_DATA_DIR 绝对根目录覆盖以隔离测试，runtime 在 `/tmp/yuin-<uid>-<data-hash>`，避免 Unix socket 路径长度限制；数据路径别名归一后共享锁。

后续文件：config.toml、library.sqlite3、sources/<id>/<hash>.js、cache/audio、cache/artwork、运行时 daemon socket／锁／诊断日志；mpv 直接使用继承连接。数据库仅由 daemon 持有写入端；SQLite 访问用专用阻塞线程或受限 spawn_blocking，不能阻塞异步调度器。

建议表：tracks、playlists、playlist_tracks、queue_entries、queue_state、sources、history、downloads、schema_migrations。列表顺序、重复入队和源更新使用事务；开启外键、设置 busy timeout，迁移前生成可恢复快照。

配置涵盖默认平台、启用源、播放／下载质量、下载目录、并发数、语言和主题。TUI 默认跟随终端配色，使用红色强调选中项。配置通过临时文件＋原子 rename 保存。配置文件和源文件权限 0600；日志不记录带鉴权的播放 URL、请求头或完整脚本。

桌面版迁移只能作为显式导入：读取用户选择的导出 JSON，校验 schema，预览／统计后写入新数据库，导入前备份。禁止与 `MyMusic/library.json` 共用写入路径；旧路径下的下载文件只有实际存在且校验通过才记为可离线播放。迁移的源不自动启用。

## 11. 下载与离线播放

最多两首并发，每个任务记录等待／解析／下载／校验／完成／失败／取消。流式写入目标文件系统中的 .part，成功校验后原子改名；已存在目标不覆盖。取消与退出清理未完成写入，保留可解释的任务状态。断点续传后续独立增加。

文件名包含稳定的歌曲与音质身份，清理非法字符；建立数据库索引，不依赖扫描文件名推测身份。下载需检查 HTTP 状态、容量限制、音频魔数和实际可解码性，拒绝 HTML 错误页；请求高音质不能静默降级。FLAC 的位深从编码头读取，不能用解码后浮点输出或源的标签宣称 24-bit。无法验证的质量明确报告未验证或失败。

播放优先使用经过校验的本地文件；断网或停止源 worker 后仍能播放已下载文件，才算离线验收通过。临时解析 URL 到期后重新通过源获取。

## 12. 实施阶段与验收门槛

| 阶段 | 交付内容 | 验收 |
| --- | --- | --- |
| S0：本次 | 项目、CLI/TUI 骨架、doctor、paths、文档、CI 配置 | 编译、格式、clippy、帮助／JSON／非 TTY 行为；交互终端退出恢复 |
| S1：播放核心（已实现） | daemon、IPC、SQLite 初始迁移、mpv 本地播放 | 一终端播放本地测试音频，另一终端暂停／定位；退出 TUI 仍播放；重启恢复但不自动出声 |
| S2：真实闭环（已实现） | 网易云目录、LX worker、源导入、在线播放、单曲下载 | 搜索 → 解析 → 播放 → 下载 → 停源断网后播放同一文件；失败与取消可解释 |
| S3：日常使用（已实现） | 歌单／排行榜全量分页、个人列表、其余四平台、下载调度 | 完整列表数量正确；按 ID 保留选择和滚动；CLI/TUI 状态一致 |
| S4：体验与发行（代码／本机打包已实现） | 歌词、主题／语言、媒体键、播放过渡、安装包／Homebrew | 真机连续使用、断网／源崩溃／设备切换；按架构发布并附校验和 |

每阶段先完成真实链路再扩展。自动测试用本地 HTTP fixture、生成的短音频和受控 JS，不默认访问真实平台；真实网络、真实脚本与音频设备测试显式启用并分别报告。CI 编译成功、虚拟音频通过、真实音乐源兼容是三个不同结论。

发布计划是 GitHub Actions 编译 macOS arm64/x86_64 与 Linux 目标，tag 上传二进制和 SHA-256；当前 CI 执行格式、clippy、隔离自动测试、Release 构建和受控 PTY 检查，不创建远程仓库或发布。mpv 的安装依赖和平台运行要求需随包说明，捆绑前核对许可证。

## 13. 外部资料与许可

资料核对日期：2026-11-04。以上库文档来自官方维护的 docs.rs 页面，音频 IPC 来自 mpv 官方手册，Rust 安装来自 [Rust 官方安装页](https://rust-lang.org/tools/install/)。

桌面项目的 Swift/JS 文件用于理解行为和边界，目录／歌词协议与 LX bridge 已按参考项目适配，来源与 LX 许可见 [第三方说明](THIRD_PARTY_NOTICES.md)。现有桌面版第三方说明明确记录 LX 的 Apache-2.0 来源与部分算法适配；后续若移植桥接脚本、签名算法或测试样本，应同步核对来源、保留所需许可和说明，不因改写成 Rust 就移除归属信息。仓库自身的开源许可证尚未选择，暂设 publish = false。

## 14. S1 实现补充

- 业务 JSON 快照包含 `schema_version=1`、daemon `instance_id` 与实例内递增 `sequence`。TUI 丢弃旧快照；事件携带全量快照，订阅慢客户端断开后重新获取快照。
- RPC 单帧上限 1 MiB、同时最多 32 个客户端、请求超时 120 秒（源解析限时 65 秒，保留目录与控制预算）。S1 队列最多 500 项且受快照容量限制。
- SQLite 使用专用线程；队列变化事务保存，位置每五秒单独 checkpoint；正常暂停、定位和关闭也保存。被强制终止最多损失最近一次 checkpoint 后的进度，重启始终暂停。
- mpv 启动首个应答限时 8 秒，普通命令 2 秒，本地加载 4 秒、远程加载 30 秒。加载期间只读状态请求直接读取共享快照；断开尚未完成的请求会取消加载。
- 默认自动测试依赖 Python 3 协议 fixture，不访问平台或音频设备。真实 mpv 与 PTY 验证显式执行；当前 Apple Silicon 检查使用生成的静音 WAV，未宣称已人工听辨或完成 Linux 真机音频验收。

## 15. 后续阶段实现说明

- 平台 Track 已保存完整源参数，ResolvedMedia 仅用于短期加载；请求头经私有 mpv IPC 设置。离线索引按稳定 ID／音质查询，文件重新验证后优先使用。
- 源导入会执行候选初始化；独立 QuickJS worker 不是 OS 网络沙箱。初始化／解析失败向调用者传播，更新失败不覆盖原记录。
- 加载不占用串行控制循环。CLI 等待最终结果，TUI 对已接受加载使用 detached 请求；退出界面不取消已接受播放。未完成普通请求断开则协作取消，generation 拦截旧结果。
- SQLite schema 已到 v3；个人列表顺序及重复约束以事务内元数据文档保存，源、下载、历史单独存表。队列与播放模式持久化。
- 个人列表／下载／历史按每页 100 条输出；1 MiB RPC 上限继续生效。大列表完整保存到数据库，不以单次客户端输出容量截断保存。
- JSON 迁移先预览，commit 前备份，源默认不启用，旧下载必须存在并通过音质验证；旧队列明确报告为不导入，以保持当前播放与偏好。
- 歌词、语言／主题、媒体控制、0–10 秒旧曲淡出、补全／手册、压缩包与 pkg 及草稿 release 流水线已加入。公开发布前仍需真实仓库 URL、项目许可证；签名公证需发行凭据。
