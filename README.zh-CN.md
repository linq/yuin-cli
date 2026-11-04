# Yuin CLI · 余音命令行

[English](README.md)

名称：**Yuin CLI**；仓库／目录名：`yuin-cli`；终端命令：`yuin`。

定位：用 Rust 开发的余音终端版，同时提供可脚本调用的命令行与交互式 TUI，两者共用音乐库、播放队列与后台播放服务。

## 当前已完成

- Rust 项目、依赖锁文件、格式检查与 CI 配置。
- `yuin --help`、`yuin --version`。
- `yuin doctor [--json]`：只读环境与 mpv 路径检查。
- `yuin paths [--json]`：显示独立的配置、数据和缓存目录，不创建或修改目录。
- `yuin tui`：可切换五个页面的终端界面骨架，支持方向键、j/k、Tab、1–5 与退出。

**当前尚未实现搜索、音乐源执行、播放、下载或后台服务。界面没有示例歌曲冒充真实结果。**

## 启动

当前 checkout 已安装项目私有 Rust 工具链，忽略于 Git，未修改 Shell 配置：

```sh
./scripts/cargo.sh run -- --help
./scripts/cargo.sh run -- doctor --json
./scripts/cargo.sh run -- tui
```

已有全局 Rust 的环境可直接使用 `cargo run -- …`。交互式终端中不带子命令会进入 TUI；重定向输出时打印帮助。

## 技术方案

见 [完整技术方案](docs/TECHNICAL_DESIGN.zh-CN.md)，包含模块边界、命令设计、TUI 交互、LX 音乐源兼容、播放后端、数据存储、迁移和分阶段验收。

第一版建议使用外部 mpv 作为音频引擎，由 Rust 通过本地 IPC 控制；当前未安装或捆绑 mpv。新项目使用独立数据目录，不直接修改 macOS 版的资料库。项目尚未选定开源许可证，暂不发布 crates.io。
