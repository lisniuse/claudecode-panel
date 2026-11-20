# Claude Code Panel

[简体中文](README.zh-CN.md) · [Download](https://github.com/lisniuse/claudecode-panel/releases/latest)

A Windows sidebar for browsing and resuming local Claude Code sessions. It groups sessions by working directory and keeps the most recently active folders at the top.

## Getting started

1. Download and run `ClaudeCodePanel.exe` on Windows 11 x64. The panel starts in the system tray.
2. Hold the pointer at the left edge of the display to reveal the panel. You can also click its tray icon.
3. Search by folder or session title. Click a folder to expand it, or double-click a session to resume it in Claude Code.

The left-edge trigger pauses when a fullscreen or borderless fullscreen application has focus. The tray icon remains available.

### Session actions

- Right-click a folder to start a Claude Code session in its working directory or delete its local session records. Deleting a folder from the panel preserves the project directory and project files.
- Right-click a session to rename or delete it. Deletion asks for confirmation and uses the Windows Recycle Bin when supported.
- The toolbar toggles compact layout, startup on sign-in, and Agent mode. Agent mode keeps the sidebar open and opens sessions with a single click. With Windows Terminal installed, it reuses a dedicated window to the right of the sidebar and only ends the previous tab started by the panel.
- Use **Language** in the tray menu to select English, Simplified Chinese, or the Windows language.

## Requirements and storage

- Windows 11 x64. Desktop acrylic is available on Windows 11 build 22621 and later and follows system support for transparency.
- Claude Code on `PATH` for starting or resuming sessions. Browsing reads existing local transcripts under `%USERPROFILE%\.claude\projects`, or under `%CLAUDE_CONFIG_DIR%\projects` when that variable is set.
- Windows Terminal is optional and enables same-window switching in Agent mode.
- Preferences are stored in `~/.claudecode-panel/settings.json`. Agent mode resets every time the app starts. Windows uses its per-user startup registration to honor the startup setting.

The panel reads session records locally. Starting or resuming a session invokes the installed Claude Code CLI, which uses its own network and account settings.

## Build

Install stable Rust for the Windows MSVC target and the Visual Studio C++ build tools. From a PowerShell terminal:

```powershell
cargo build --release --locked
cargo test --locked
```

The executable is built at `target\release\ClaudeCodePanel.exe`. The release build links the C runtime statically and uses Windows system DLLs.

## License

[MIT](LICENSE). See [third-party notices](THIRD_PARTY_NOTICES.txt) for bundled dependencies. Claude Code is an Anthropic product and Claude Code Panel is an independent project.
