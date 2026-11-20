use crate::i18n::Preference;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::*;
use windows_sys::w;

const PREFS_KEY: *const u16 = w!("Software\\ClaudeCodePanel");
const COMPACT_VALUE: *const u16 = w!("CompactLayout");
const RUN_KEY: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const STARTUP_VALUE: *const u16 = w!("ClaudeCodePanel");
pub(crate) const STARTUP_ARG: &str = "--startup";

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub compact: bool,
    pub startup: bool,
    pub language: Preference,
}

pub(crate) fn load() -> io::Result<Preferences> {
    let path = settings_path()?;
    match fs::read(&path) {
        Ok(contents) => Ok(serde_json::from_slice(&contents)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let preferences = Preferences {
                compact: legacy_compact(),
                startup: startup_registered(),
                ..Preferences::default()
            };
            save_to(&path, preferences)?;
            Ok(preferences)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn update(current: Preferences, next: Preferences) -> io::Result<()> {
    let path = settings_path()?;
    if current.startup != next.startup {
        register_startup(next.startup)?;
    }
    if let Err(error) = save_to(&path, next) {
        if current.startup != next.startup {
            register_startup(current.startup)?;
        }
        return Err(error);
    }
    Ok(())
}

fn settings_path() -> io::Result<PathBuf> {
    let profile = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                tr!("Could not find the user profile folder", "找不到用户目录"),
            )
        })?;
    Ok(PathBuf::from(profile)
        .join(".claudecode-panel")
        .join("settings.json"))
}

fn save_to(path: &Path, preferences: Preferences) -> io::Result<()> {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".settings-{}-{nonce}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        serde_json::to_writer_pretty(&mut file, &preferences)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn legacy_compact() -> bool {
    let mut value = 0u32;
    let mut length = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PREFS_KEY,
            COMPACT_VALUE,
            RRF_RT_REG_DWORD,
            null_mut(),
            (&raw mut value).cast(),
            &mut length,
        )
    };
    status == ERROR_SUCCESS && value != 0
}

fn startup_registered() -> bool {
    let mut length = 0;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            STARTUP_VALUE,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut length,
        )
    };
    status == ERROR_SUCCESS && length > size_of::<u16>() as u32
}

fn register_startup(enabled: bool) -> io::Result<()> {
    if !enabled {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, STARTUP_VALUE) };
        return match status {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(()),
            status => registry_result(status),
        };
    }
    let command = startup_command(&std::env::current_exe()?)?;
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            STARTUP_VALUE,
            REG_SZ,
            command.as_ptr().cast(),
            std::mem::size_of_val(command.as_slice()) as u32,
        )
    };
    registry_result(status)
}

fn startup_command(exe: &Path) -> io::Result<Vec<u16>> {
    let command: Vec<_> = [u16::from(b'"')]
        .into_iter()
        .chain(exe.as_os_str().encode_wide())
        .chain([u16::from(b'"'), u16::from(b' ')])
        .chain(STARTUP_ARG.encode_utf16())
        .chain([0])
        .collect();
    if command.len() > 261 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            tr!(
                "The executable path is too long for a Windows startup entry",
                "程序路径太长，无法写入 Windows 启动项"
            ),
        ));
    }
    Ok(command)
}

fn registry_result(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_replace_the_settings_file_without_persisting_agent_mode() -> io::Result<()> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "claude-panel-settings-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let path = directory.join("settings.json");
        save_to(&path, Preferences::default())?;
        let next = Preferences {
            compact: true,
            startup: true,
            language: Preference::English,
        };
        save_to(&path, next)?;
        let contents = fs::read(&path)?;
        assert_eq!(serde_json::from_slice::<Preferences>(&contents)?, next);
        let fields = serde_json::from_slice::<serde_json::Value>(&contents)?;
        let fields = fields.as_object().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields["language"], "en");
        fs::remove_file(&path)?;
        fs::remove_dir(&directory)
    }

    #[test]
    fn startup_command_quotes_the_executable_and_marks_login_launches() {
        let command = startup_command(Path::new(r"C:\My Apps\ClaudeCodePanel.exe")).unwrap();
        assert_eq!(command.last(), Some(&0));
        assert_eq!(
            String::from_utf16_lossy(&command[..command.len() - 1]),
            r#""C:\My Apps\ClaudeCodePanel.exe" --startup"#
        );
    }
}
