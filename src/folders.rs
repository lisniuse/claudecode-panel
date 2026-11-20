use std::env;
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null;
use windows_sys::Win32::Foundation::{HWND, S_OK};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::UI::Shell::{
    ASSOCF_INIT_BYEXENAME, ASSOCSTR_EXECUTABLE, AssocQueryStringW, SEE_MASK_FLAG_NO_UI,
    SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows_sys::w;

pub(crate) fn open_in_explorer(hwnd: HWND, path: &Path) -> io::Result<()> {
    let path: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut request = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        hwnd,
        lpVerb: w!("explore"),
        lpFile: path.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..SHELLEXECUTEINFOW::default()
    };
    unsafe {
        let com = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let result = if ShellExecuteExW(&mut request) != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
        if com >= 0 {
            CoUninitialize();
        }
        result
    }
}

pub(crate) fn open_in_vscode(code: &Path, folder: &Path) -> io::Result<()> {
    Command::new(code)
        .arg(folder)
        .env_remove("ELECTRON_RUN_AS_NODE")
        .spawn()?;
    Ok(())
}

pub(crate) fn find_vscode() -> Option<PathBuf> {
    registered_vscode()
        .into_iter()
        .chain(
            [
                ("LOCALAPPDATA", r"Programs\Microsoft VS Code\Code.exe"),
                ("ProgramW6432", r"Microsoft VS Code\Code.exe"),
                ("ProgramFiles", r"Microsoft VS Code\Code.exe"),
                ("ProgramFiles(x86)", r"Microsoft VS Code\Code.exe"),
            ]
            .into_iter()
            .filter_map(|(name, suffix)| {
                env::var_os(name).map(|root| PathBuf::from(root).join(suffix))
            }),
        )
        .chain(
            env::split_paths(&env::var_os("PATH").unwrap_or_default()).flat_map(|directory| {
                // The installer adds its bin directory, one level below Code.exe.
                let sibling = directory
                    .file_name()
                    .filter(|name| name.eq_ignore_ascii_case("bin"))
                    .and_then(|_| directory.parent())
                    .map(|root| root.join("Code.exe"));
                std::iter::once(directory.join("Code.exe")).chain(sibling)
            }),
        )
        .find(|path| path.is_absolute() && path.is_file())
}

fn registered_vscode() -> Option<PathBuf> {
    let mut length = 0;
    unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_BYEXENAME,
            ASSOCSTR_EXECUTABLE,
            w!("Code.exe"),
            w!("open"),
            std::ptr::null_mut(),
            &mut length,
        );
        if length == 0 {
            return None;
        }
        let mut path = vec![0; length as usize];
        let status = AssocQueryStringW(
            ASSOCF_INIT_BYEXENAME,
            ASSOCSTR_EXECUTABLE,
            w!("Code.exe"),
            w!("open"),
            path.as_mut_ptr(),
            &mut length,
        );
        if status != S_OK {
            return None;
        }
        let path = OsString::from_wide(path.split(|&ch| ch == 0).next()?);
        Some(PathBuf::from(path))
    }
}
