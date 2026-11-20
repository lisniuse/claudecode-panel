# Contributing

Bug reports and pull requests are welcome. For UI issues, include the app version, Windows build, display scale, and steps to reproduce. Redact session text, paths, and credentials from logs and screenshots before posting.

Use stable Rust with the Windows MSVC toolchain. Before opening a pull request, run:

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Keep changes focused and update both languages when changing visible text. Use temporary transcripts for tests that rename or delete session records.

The build links the checked-in `assets/icons.res`. After editing `assets/logo.svg` or `assets/tray.svg`, regenerate it from a Visual Studio Developer PowerShell with CairoSVG and Pillow installed.

```powershell
python assets/build-icons.py
```
