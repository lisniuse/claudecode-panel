# Claude Code Panel

[English](README.md) · [下载](https://github.com/lisniuse/claudecode-panel/releases/latest)

一个 Windows 侧栏，用于浏览和恢复本地 Claude Code 会话。面板按工作目录分组，最近活跃的文件夹排在最上方。

## 使用方法

1. 在 Windows 11 x64 上下载并运行 `ClaudeCodePanel.exe`。面板启动后驻留在系统托盘。
2. 把鼠标移到屏幕左侧并稍作停留，或点击托盘图标来打开面板。
3. 搜索文件夹名或会话标题。点击文件夹可以展开，双击会话可以在 Claude Code 中继续。

当前屏幕的全屏或无边框全屏应用处于前台时，屏幕左缘暂停触发，可以从托盘手动打开面板。

### 会话操作

- 文件夹右键菜单可以在该工作目录新建会话，或删除该分组的本地会话记录。删除会保留项目目录和项目文件。
- 会话右键菜单支持重命名、删除。删除前会确认，Windows 支持时会把记录移到回收站。
- 顶部图标可以切换紧凑布局、开机启动和 Agent 模式。开启 Agent 模式后，侧栏固定展开，单击即可打开会话。有 Windows Terminal 时，面板复用右侧的专用窗口，切换时只结束上一个由面板打开的会话标签页。
- 托盘菜单的「语言」可以切换简体中文、English 或跟随系统。

## 系统要求和设置

- Windows 11 x64。系统磨砂效果需要 Windows 11 Build 22621 或更新版本，并取决于系统的透明效果支持。
- 新建或恢复会话需要 PATH 中有 Claude Code。面板从 `%USERPROFILE%\.claude\projects` 读取本地记录，设置了 `CLAUDE_CONFIG_DIR` 时则读取 `%CLAUDE_CONFIG_DIR%\projects`。
- Windows Terminal 为可选组件，用于 Agent 模式在同一窗口切换会话。
- 配置位于 `~/.claudecode-panel/settings.json`。Agent 模式每次启动时关闭。开机启动由 Windows 的当前用户登录项执行。

面板在本地读取会话记录。新建和恢复会话调用你安装的 Claude Code，其网络和账号设置由 Claude Code 管理。

## 编译

安装 Windows MSVC 目标的稳定版 Rust 及 Visual Studio C++ 生成工具，然后在 PowerShell 中运行：

```powershell
cargo build --release --locked
cargo test --locked
```

输出文件为 `target\release\ClaudeCodePanel.exe`。发布版静态链接 C 运行库，运行时使用 Windows 系统 DLL。

## 许可证

项目采用 [MIT 许可证](LICENSE)。内置依赖见[第三方许可声明](THIRD_PARTY_NOTICES.txt)。Claude Code 是 Anthropic 的产品，本面板是独立项目。
