#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod controls;
mod dialogs;
mod folders;
mod paint;
mod panel;
mod sessions;
mod settings;
mod terminal;

fn main() {
    i18n::select(i18n::Preference::System);
    if let Some(code) = terminal::run_tab_helper() {
        std::process::exit(code);
    }
    let result = settings::load().and_then(|preferences| {
        i18n::select(preferences.language);
        if panel::run(preferences)? {
            // The old panel has removed its tray icon and released the singleton.
            std::process::Command::new(std::env::current_exe()?).spawn()?;
        }
        Ok(())
    });
    if let Err(error) = result {
        panel::show_error(&trf!(
            "Could not start Claude Code Panel: {error}",
            "无法启动 Claude Code 面板：{error}"
        ));
    }
}
