# 初始化与验证记录

日期：2026-11-04（Asia/Shanghai）。

## 已完成

- 名称 Yuin CLI · 余音命令行；目录 yuin-cli；二进制 yuin。
- 用户已确认同时提供纯命令行和交互式 TUI。
- 单 Cargo package，Rust 2024 edition，stable toolchain，依赖由 Cargo.lock 锁定。
- clap 命令入口：help/version、doctor、paths、tui；无参数且为交互终端时进入 TUI。
- TUI 五页导航骨架，方向键、j/k、Tab、数字键选择，q/Esc/Ctrl+C 退出。
- doctor/paths 提供 JSON 输出；不创建应用数据目录、不启动后台服务、不触发音频。
- 英文默认 README、中文 README、完整中文技术方案，以及 macOS/Linux GitHub Actions 检查配置。
- 本地 Git 初始化，作者 linq <fancylinq@gmail.com>，分支 codex/bootstrap。

## 本地验证

环境：Apple Silicon macOS。项目私有工具链 rustc 1.99.1（2026-10-13），通过官方 rustup 安装于 .tools；安装使用 --no-modify-path，未修改 Shell 配置。工具链、编译缓存与临时诊断均由 Git 忽略。

- cargo fmt --all -- --check：通过。
- cargo clippy --all-targets --locked -- -D warnings：通过。
- cargo test --locked：4 项集成检查通过，0 失败。覆盖非交互默认帮助、JSON 输出、无终端时拒绝 TUI 且不输出控制序列，以及无效命令退出码。
- cargo build --release --locked：通过；产物 target/release/yuin。
- 实际运行 help、version、doctor --json、paths --json：通过。
- 独立 PTY 检查 Release 二进制：初始渲染、数字键切到 Playlists、120×30 缩至 60×12、q 退出、退出码 0、终端 termios 状态恢复均通过。早期验证脚本把 ANSI 分段绘制误当成连续文本，并曾因未排空 PTY 输出而阻塞；完善测试驱动后通过，没有因此修改应用逻辑。PTY 驱动只用于本地初始化验收，位于忽略的 .tools 中。

## 尚未实现或验证

- 平台目录 API、JavaScript 源宿主、daemon、SQLite、实际播放、下载与迁移均尚未实现。
- 当前 PATH 未检测到 mpv。它是技术方案建议的后续播放依赖，本次未安装。
- 未下载或运行第三方真实音乐源，未读写桌面版资料库。
- CI 文件已创建，尚未在 GitHub 执行；Linux/Intel/Windows 及真实音频设备均未做运行验收。
- 尚未创建远程仓库或配置发布；仓库许可证未选择，crates.io 发布关闭。
