use crate::paint;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::{
    Controls::{
        EM_SETLIMITTEXT, EM_SETSEL, TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0,
        TD_WARNING_ICON, TDF_ALLOW_DIALOG_CANCELLATION, TaskDialogIndirect,
    },
    Input::KeyboardAndMouse::{EnableWindow, SetFocus},
    Shell::*,
    WindowsAndMessaging::*,
};

const NAME_EDIT: u16 = 100;
const TITLE_LIMIT: usize = 180;

pub(crate) fn confirm_delete(owner: HWND, message: &str) -> bool {
    let message: Vec<_> = message.encode_utf16().chain([0]).collect();
    let caption = crate::i18n::wide(tr!("Confirm deletion", "确认删除"));
    let delete = crate::i18n::wide(tr!("Delete", "删除"));
    let cancel = crate::i18n::wide(tr!("Cancel", "取消"));
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: IDYES,
            pszButtonText: delete.as_ptr(),
        },
        TASKDIALOG_BUTTON {
            nButtonID: IDCANCEL,
            pszButtonText: cancel.as_ptr(),
        },
    ];
    let dialog = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: caption.as_ptr(),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_WARNING_ICON,
        },
        pszContent: message.as_ptr(),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: IDCANCEL,
        ..TASKDIALOGCONFIG::default()
    };
    unsafe {
        let mut choice = IDCANCEL;
        if TaskDialogIndirect(
            &dialog,
            &mut choice,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) >= 0
        {
            return choice == IDYES;
        }
        MessageBoxW(
            owner,
            message.as_ptr(),
            caption.as_ptr(),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

pub(crate) fn recycle(owner: HWND, paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let files: Vec<_> = paths
        .iter()
        .flat_map(|path| path.as_os_str().encode_wide().chain([0]))
        .chain([0])
        .collect();
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: owner,
        wFunc: FO_DELETE,
        pFrom: files.as_ptr(),
        fFlags: (FOF_ALLOWUNDO
            | FOF_NOCONFIRMATION
            | FOF_WANTNUKEWARNING
            | FOF_NOERRORUI
            | FOF_SILENT) as u16,
        ..SHFILEOPSTRUCTW::default()
    };
    let result = unsafe { SHFileOperationW(&mut operation) };
    if result != 0 {
        Err(trf!(
            "Deletion did not complete (Windows error 0x{result:X}). The list will reflect what was removed.",
            "删除未完成（Windows 错误 0x{result:X}），列表会按实际结果刷新。"
        ))
    } else if operation.fAnyOperationsAborted != 0 {
        Err(tr!(
            "Deletion was canceled. The list will reflect what was removed.",
            "删除已取消，列表会按实际结果刷新。"
        )
        .into())
    } else {
        Ok(())
    }
}

pub(crate) fn rename_prompt(owner: HWND, current: &str) -> io::Result<Option<String>> {
    let template = rename_template();
    let mut title = current.to_owned();
    let result = unsafe {
        DialogBoxIndirectParamW(
            GetModuleHandleW(null()),
            template.as_ptr().cast(),
            owner,
            Some(rename_proc),
            (&raw mut title) as LPARAM,
        )
    };
    match result {
        -1 => Err(io::Error::last_os_error()),
        value if value == IDOK as isize => Ok(Some(title)),
        _ => Ok(None),
    }
}

fn rename_template() -> Vec<u32> {
    // A standard dialog template is packed as WORDs, with each item and the
    // template itself beginning on a DWORD boundary.
    let mut words = Vec::<u16>::new();
    let style = WS_POPUP
        | WS_CAPTION
        | WS_SYSMENU
        | DS_MODALFRAME as u32
        | DS_CENTER as u32
        | DS_SETFONT as u32;
    push_dword(&mut words, style);
    push_dword(&mut words, 0);
    words.extend([3, 0, 0, 246, 58, 0, 0]);
    words.extend(
        tr!("Rename session", "重命名会话")
            .encode_utf16()
            .chain([0]),
    );
    words.push(10);
    words.extend_from_slice(paint::font_face());

    for (id, class, style, bounds, title) in [
        (
            NAME_EDIT,
            0x81,
            WS_BORDER | ES_AUTOHSCROLL as u32,
            [10, 10, 226, 16],
            "",
        ),
        (
            IDOK as u16,
            0x80,
            BS_DEFPUSHBUTTON as u32,
            [128, 35, 50, 14],
            tr!("Save", "保存"),
        ),
        (
            IDCANCEL as u16,
            0x80,
            BS_PUSHBUTTON as u32,
            [186, 35, 50, 14],
            tr!("Cancel", "取消"),
        ),
    ] {
        align_dword(&mut words);
        push_dword(&mut words, style | WS_CHILD | WS_VISIBLE | WS_TABSTOP);
        push_dword(&mut words, 0);
        words.extend(bounds);
        words.extend([id, 0xffff, class]);
        words.extend(title.encode_utf16().chain([0, 0]));
    }
    align_dword(&mut words);
    words
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u32::from(pair[0]) | u32::from(pair[1]) << 16)
        .collect()
}

fn push_dword(words: &mut Vec<u16>, value: u32) {
    words.extend([value as u16, (value >> 16) as u16]);
}

fn align_dword(words: &mut Vec<u16>) {
    if !words.len().is_multiple_of(2) {
        words.push(0);
    }
}

unsafe extern "system" fn rename_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
                let current = &*(lparam as *const String);
                let text: Vec<_> = current.encode_utf16().chain([0]).collect();
                let edit = GetDlgItem(hwnd, NAME_EDIT as i32);
                SendMessageW(edit, EM_SETLIMITTEXT, TITLE_LIMIT, 0);
                SetWindowTextW(edit, text.as_ptr());
                SetFocus(edit);
                SendMessageW(edit, EM_SETSEL, 0, -1);
                0
            }
            WM_CLOSE => {
                EndDialog(hwnd, IDCANCEL as isize);
                1
            }
            WM_COMMAND => {
                let id = (wparam & 0xffff) as u16;
                if id == IDCANCEL as u16 {
                    EndDialog(hwnd, IDCANCEL as isize);
                    return 1;
                }
                if id != IDOK as u16 && (id != NAME_EDIT || (wparam >> 16) as u32 != EN_CHANGE) {
                    return 0;
                }
                let mut buffer = [0u16; TITLE_LIMIT + 1];
                let len = GetDlgItemTextW(
                    hwnd,
                    NAME_EDIT as i32,
                    buffer.as_mut_ptr(),
                    buffer.len() as i32,
                );
                let text = String::from_utf16_lossy(&buffer[..len as usize]);
                let trimmed = text.trim();
                if id == NAME_EDIT {
                    EnableWindow(GetDlgItem(hwnd, IDOK), i32::from(!trimmed.is_empty()));
                } else if !trimmed.is_empty() {
                    let title = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut String;
                    *title = trimmed.to_owned();
                    EndDialog(hwnd, IDOK as isize);
                }
                1
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
#[test]
fn rename_dialog_loads_the_input_and_action_buttons() {
    let template = rename_template();
    let mut current = String::from("现有会话");
    unsafe {
        let dialog = CreateDialogIndirectParamW(
            GetModuleHandleW(null()),
            template.as_ptr().cast(),
            std::ptr::null_mut(),
            Some(rename_proc),
            (&raw mut current) as LPARAM,
        );
        assert!(!dialog.is_null(), "{}", io::Error::last_os_error());
        assert!(!GetDlgItem(dialog, IDOK).is_null());
        assert!(!GetDlgItem(dialog, IDCANCEL).is_null());
        let mut text = [0u16; 16];
        let length = GetDlgItemTextW(
            dialog,
            NAME_EDIT as i32,
            text.as_mut_ptr(),
            text.len() as i32,
        );
        assert_eq!(String::from_utf16_lossy(&text[..length as usize]), current);
        DestroyWindow(dialog);
    }
}
