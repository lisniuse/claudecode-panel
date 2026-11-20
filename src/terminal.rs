use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::System::{
    Console::*, JobObjects::*, SystemInformation::GetSystemDirectoryW, Threading::*,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const COLOR_ENV: [(&str, &str); 3] = [
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("FORCE_COLOR", "3"),
];
const SHIM_ENV: &str = "CLAUDE_PANEL_AGENT_COMMAND";
const TAB_HELPER_ARG: &str = "--agent-tab";
const CLOSE_GRACE: Duration = Duration::from_millis(600);
const WINDOW_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn launch(cwd: &Path, args: &[&str]) -> Result<(), String> {
    Command::new(resolve_cli(cwd)?)
        .current_dir(cwd)
        .args(args)
        .env_remove("NO_COLOR")
        .envs(COLOR_ENV)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map_err(|error| {
            trf!(
                "Could not start Claude Code: {error}",
                "无法启动 Claude Code：{error}"
            )
        })?;
    Ok(())
}

pub(crate) fn open_shell(cwd: &Path) -> io::Result<()> {
    if let Some(terminal) = windows_terminal()
        && !contains_separator(cwd.as_os_str())
        && Command::new(terminal)
            .args(["--window", "new", "new-tab", "--startingDirectory"])
            .arg(cwd)
            .current_dir(cwd)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok()
    {
        return Ok(());
    }

    Command::new(system_directory()?.join(r"WindowsPowerShell\v1.0\powershell.exe"))
        .arg("-NoLogo")
        .current_dir(cwd)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()?;
    Ok(())
}

fn resolve_cli(cwd: &Path) -> Result<PathBuf, String> {
    if !cwd.is_dir() {
        return Err(trf!(
            "Could not find the original working directory: {}",
            "找不到原工作目录：{}",
            cwd.display()
        ));
    }
    let path = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| {
            ["claude.exe", "claude.cmd", "claude.bat"].map(|name| directory.join(name))
        })
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            tr!(
                "Claude Code was not found on PATH. Install it and reopen the panel.",
                "在 PATH 中找不到 Claude Code。安装后请重新打开面板。"
            )
            .into()
        })
}

pub(crate) struct AgentTerminals {
    window_name: String,
    active: Option<AgentTerminal>,
    previous: Option<AgentTerminal>,
    pending: Option<Launch>,
}

impl Default for AgentTerminals {
    fn default() -> Self {
        let started = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        Self {
            window_name: format!("ClaudePanelAgent-{}-{started}", std::process::id()),
            active: None,
            previous: None,
            pending: None,
        }
    }
}

impl AgentTerminals {
    pub(crate) fn open(&mut self, cwd: &Path, session: &str) -> Result<(), String> {
        if let Some(active) = &mut self.active
            && active.closing.is_none()
            && active.running()
            && active.session == session
            && active.cwd == cwd
        {
            self.pending = None;
            active.fitted = None;
            if active.owns_window() {
                unsafe {
                    if IsIconic(active.window) != 0 {
                        ShowWindow(active.window, SW_RESTORE);
                    }
                    SetForegroundWindow(active.window);
                }
            }
            return Ok(());
        }

        let request = Launch::new(cwd, session)?;
        self.pending = Some(request);
        self.launch_pending()
    }

    pub(crate) fn cancel_pending(&mut self) {
        self.pending = None;
        if self.previous.is_some() && self.active.as_ref().is_some_and(|active| !active.ready()) {
            self.active = self.previous.take();
        }
    }

    pub(crate) fn update(&mut self, bounds: RECT, dock: bool) -> Result<(), String> {
        let mut failed = None;
        if let Some(active) = &mut self.active {
            let now = Instant::now();
            if !active.running() {
                if !active.ready() {
                    failed = active.startup_error().or_else(|| {
                        matches!(active.host, Host::WindowsTerminal { .. }).then(|| {
                            tr!("The new session did not start. Check Windows Terminal for details.", "新会话未能启动。请查看 Windows Terminal 中的错误信息。").into()
                        })
                    });
                }
                if self.previous.is_some() {
                    self.active = self.previous.take();
                } else {
                    self.active = None;
                }
            } else if let Some(deadline) = active.closing {
                if now >= deadline
                    && let Host::Console { job, .. } = &active.host
                {
                    if unsafe { TerminateJobObject(job.as_raw_handle(), 1) } == 0 {
                        failed = Some(trf!(
                            "Could not close the Agent terminal: {}",
                            "无法关闭 Agent 终端：{}",
                            io::Error::last_os_error()
                        ));
                        self.active = None;
                    } else {
                        active.closing = Some(now + CLOSE_GRACE);
                    }
                }
            } else {
                active.refresh_window();
                if active.ready() {
                    if dock && !active.window.is_null() {
                        active.fit(bounds);
                    }
                    self.previous = None;
                } else {
                    failed = active.startup_error().or_else(|| {
                        (now.duration_since(active.started) >= WINDOW_TIMEOUT)
                            .then(|| tr!("The Agent session took too long to start. Check the terminal window.", "Agent 会话启动超时，请检查终端窗口。").into())
                    });
                    if failed.is_some() {
                        self.active = self.previous.take();
                    }
                }
            }
        }
        if let Some(error) = failed {
            self.pending = None;
            return Err(error);
        }
        self.launch_pending()
    }

    fn launch_pending(&mut self) -> Result<(), String> {
        if self.pending.is_none() || self.previous.is_some() {
            return Ok(());
        }
        if let Some(active) = &mut self.active
            && active.running()
        {
            if matches!(active.host, Host::Console { .. }) {
                active.close_console();
                return Ok(());
            }
            if !active.ready() {
                return Ok(());
            }
        }
        if let Some(request) = self.pending.take() {
            let next = request.spawn(&self.window_name).map_err(|error| {
                trf!(
                    "Could not open the Agent terminal: {error}",
                    "无法打开 Agent 终端：{error}"
                )
            })?;
            self.previous = self.active.take().filter(AgentTerminal::running);
            self.active = Some(next);
        }
        Ok(())
    }
}

struct Launch {
    cwd: PathBuf,
    session: String,
    cli: PathBuf,
}

impl Launch {
    fn new(cwd: &Path, session: &str) -> Result<Self, String> {
        if !crate::sessions::valid_id(session) {
            return Err(tr!("Invalid session ID.", "会话 ID 无效。").into());
        }
        Ok(Self {
            cwd: cwd.to_owned(),
            session: session.to_owned(),
            cli: resolve_cli(cwd)?,
        })
    }

    fn spawn(self, window_name: &str) -> io::Result<AgentTerminal> {
        let system = system_directory()?;
        if let Some(terminal) = windows_terminal()
            && !contains_separator(self.cwd.as_os_str())
            && let Ok(host) = self.start_windows_terminal(&terminal, &system, window_name)
        {
            return Ok(self.into_terminal(host));
        }
        let conhost = system.join("conhost.exe");
        let mut command = OsString::new();
        push_quoted(&mut command, conhost.as_os_str());
        // Use a dedicated console host so a default-terminal handoff cannot
        // place the session inside a window or a tab owned by another app.
        command.push(" -ForceNoHandoff ");
        let (_, cli_command, shim) = self.cli_command(&system);
        command.push(cli_command);

        let (job, process) = spawn_in_job(
            &conhost,
            &command,
            &self.cwd,
            shim,
            null_mut(),
            CREATE_NO_WINDOW,
        )?;
        Ok(self.into_terminal(Host::Console { job, process }))
    }

    fn cli_command(&self, system: &Path) -> (PathBuf, OsString, Option<OsString>) {
        let mut command = OsString::new();
        if self.cli.extension().is_some_and(|ext| ext != "exe") {
            let exe = system.join("cmd.exe");
            push_quoted(&mut command, exe.as_os_str());
            // Expand the quoted shim command after CMD parses metacharacters.
            command.push(" /d /v:on /c cd . && !CLAUDE_PANEL_AGENT_COMMAND!");
            let mut script_command = OsString::new();
            push_quoted(&mut script_command, self.cli.as_os_str());
            script_command.push(" --resume ");
            script_command.push(&self.session);
            (exe, command, Some(script_command))
        } else {
            push_quoted(&mut command, self.cli.as_os_str());
            command.push(" --resume ");
            command.push(&self.session);
            (self.cli.clone(), command, None)
        }
    }

    fn start_windows_terminal(
        &self,
        terminal: &Path,
        system: &Path,
        window_name: &str,
    ) -> io::Result<Host> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let token = format!("{window_name}-{unique}");
        let title = format!("Claude Code · Agent {}-{unique}", std::process::id());
        let signals = TabSignals::create(&token)?;
        let exe = env::current_exe()?;
        let launcher = self
            .windows_terminal_command(terminal, system, &exe, window_name, &token, &title)
            .spawn()?;
        Ok(Host::WindowsTerminal {
            launcher,
            title,
            signals,
        })
    }

    fn windows_terminal_command(
        &self,
        terminal: &Path,
        system: &Path,
        helper: &Path,
        window_name: &str,
        token: &str,
        title: &str,
    ) -> Command {
        let mut shim = OsString::new();
        push_quoted(&mut shim, helper.as_os_str());
        shim.push(" ");
        shim.push(TAB_HELPER_ARG);
        shim.push(" ");
        shim.push(token);
        shim.push(" ");
        shim.push(&self.session);
        shim.push(" ");
        push_quoted(&mut shim, self.cli.as_os_str());
        let mut command = Command::new(terminal);
        command
            .args([
                "--window",
                window_name,
                "new-tab",
                "--title",
                title,
                "--suppressApplicationTitle",
                "--inheritEnvironment",
                "--startingDirectory",
            ])
            .arg(&self.cwd)
            .arg("--")
            .current_dir(&self.cwd)
            .env_remove("NO_COLOR")
            .envs(COLOR_ENV)
            .creation_flags(CREATE_NO_WINDOW)
            .arg(system.join("cmd.exe"))
            .args([
                "/d",
                "/v:on",
                "/c",
                "cd",
                ".",
                "&&",
                "!CLAUDE_PANEL_AGENT_COMMAND!",
            ])
            .env(SHIM_ENV, shim);
        command
    }

    fn into_terminal(self, host: Host) -> AgentTerminal {
        AgentTerminal {
            host,
            window: null_mut(),
            cwd: self.cwd,
            session: self.session,
            started: Instant::now(),
            closing: None,
            fitted: None,
        }
    }
}

fn windows_terminal() -> Option<PathBuf> {
    let alias = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|local| local.join(r"Microsoft\WindowsApps\wt.exe"));
    let exists = |path: &PathBuf| std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir());
    alias.filter(exists).or_else(|| {
        let path = env::var_os("PATH").unwrap_or_default();
        env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("wt.exe"))
            .find(exists)
    })
}

fn native_claude(cli: &Path) -> Option<PathBuf> {
    if cli.extension().is_some_and(|ext| ext == "exe") {
        return Some(cli.to_owned());
    }
    let installed = cli
        .parent()?
        .join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
    installed.is_file().then_some(installed)
}

fn contains_separator(argument: &OsStr) -> bool {
    argument.encode_wide().any(|word| word == u16::from(b';'))
}

fn spawn_in_job(
    exe: &Path,
    command: &OsStr,
    cwd: &Path,
    shim: Option<OsString>,
    desktop: *mut u16,
    flags: PROCESS_CREATION_FLAGS,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let application = wide(exe.as_os_str());
    let mut command = wide(command);
    let directory = wide(cwd.as_os_str());
    let environment = console_environment(shim);
    unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(job);
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..JOBOBJECT_BASIC_LIMIT_INFORMATION::default()
            },
            ..JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default()
        };
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: desktop,
            ..STARTUPINFOW::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        if CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | flags,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup,
            &mut info,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle::from_raw_handle(info.hProcess);
        let thread = OwnedHandle::from_raw_handle(info.hThread);
        if AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) == 0
            || ResumeThread(thread.as_raw_handle()) == u32::MAX
        {
            let error = io::Error::last_os_error();
            TerminateProcess(process.as_raw_handle(), 1);
            return Err(error);
        }
        Ok((job, process))
    }
}

enum Host {
    Console {
        job: OwnedHandle,
        process: OwnedHandle,
    },
    WindowsTerminal {
        launcher: Child,
        title: String,
        signals: TabSignals,
    },
}

struct TabSignals {
    ready: OwnedHandle,
    stop: OwnedHandle,
    done: OwnedHandle,
}

impl TabSignals {
    fn create(token: &str) -> io::Result<Self> {
        Self::open(token, true)
    }

    fn open(token: &str, create: bool) -> io::Result<Self> {
        let event = |name| {
            let name = wide(OsStr::new(&format!("Local\\{token}-{name}")));
            let handle = unsafe {
                if create {
                    CreateEventW(null(), 1, 0, name.as_ptr())
                } else {
                    OpenEventW(
                        EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
                        0,
                        name.as_ptr(),
                    )
                }
            };
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
            }
        };
        Ok(Self {
            ready: event("ready")?,
            stop: event("stop")?,
            done: event("done")?,
        })
    }
}

fn is_signaled(event: &OwnedHandle) -> bool {
    unsafe { WaitForSingleObject(event.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

pub(crate) fn run_tab_helper() -> Option<i32> {
    let mut args = env::args_os().skip(1);
    if args.next()?.as_os_str() != TAB_HELPER_ARG {
        return None;
    }
    let token = args.next().and_then(|value| value.into_string().ok());
    let session = args.next().and_then(|value| value.into_string().ok());
    let cli = args.next().map(PathBuf::from);
    let (Some(token), Some(session), Some(cli)) = (token, session, cli) else {
        return Some(1);
    };
    if !token.starts_with("ClaudePanelAgent-")
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || !crate::sessions::valid_id(&session)
        || !cli.is_absolute()
        || !cli
            .extension()
            .is_some_and(|ext| matches!(ext.to_str(), Some("exe" | "cmd" | "bat")))
    {
        return Some(1);
    }
    if let Ok(preferences) = crate::settings::load() {
        crate::i18n::select(preferences.language);
    }
    let result = run_tab(&token, &session, cli);
    Some(match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "{}",
                trf!(
                    "Could not start the Agent session: {error}",
                    "无法启动 Agent 会话：{error}"
                )
            );
            1
        }
    })
}

fn run_tab(token: &str, session: &str, cli: PathBuf) -> Result<i32, String> {
    let signals = TabSignals::open(token, false).map_err(|error| error.to_string())?;
    struct Completion<'a>(&'a OwnedHandle);
    impl Drop for Completion<'_> {
        fn drop(&mut self) {
            unsafe { SetEvent(self.0.as_raw_handle()) };
        }
    }
    let _completion = Completion(&signals.done);
    if is_signaled(&signals.stop) {
        return Ok(0);
    }
    if unsafe { GetConsoleCP() == 0 && AttachConsole(ATTACH_PARENT_PROCESS) == 0 } {
        return Err(io::Error::last_os_error().to_string());
    }
    let cwd = env::current_dir().map_err(|error| error.to_string())?;
    let mut launch = Launch {
        cwd: cwd.clone(),
        session: session.to_owned(),
        cli,
    };
    if let Some(native) = native_claude(&launch.cli) {
        launch.cli = native;
    }
    let system = system_directory().map_err(|error| error.to_string())?;
    let (exe, command, shim) = launch.cli_command(&system);
    let (_job, process) = spawn_in_job(
        &exe,
        &command,
        &cwd,
        shim,
        null_mut(),
        CREATE_NEW_PROCESS_GROUP,
    )
    .map_err(|error| error.to_string())?;
    unsafe { SetEvent(signals.ready.as_raw_handle()) };
    let handles = [signals.stop.as_raw_handle(), process.as_raw_handle()];
    let result =
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };
    match result {
        WAIT_OBJECT_0 => {
            unsafe {
                let pid = GetProcessId(process.as_raw_handle());
                if pid != 0 && GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 {
                    WaitForSingleObject(process.as_raw_handle(), CLOSE_GRACE.as_millis() as u32);
                }
            }
            // A zero exit closes this tab under Terminal's normal profile
            // policy. Dropping the job clears any remaining child processes.
            Ok(0)
        }
        value if value == WAIT_OBJECT_0 + 1 => {
            let mut code = 0;
            unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
            Ok(code as i32)
        }
        _ => Err(io::Error::last_os_error().to_string()),
    }
}

struct AgentTerminal {
    host: Host,
    window: HWND,
    cwd: PathBuf,
    session: String,
    started: Instant,
    closing: Option<Instant>,
    fitted: Option<[i32; 4]>,
}

impl AgentTerminal {
    fn running(&self) -> bool {
        match &self.host {
            Host::Console { process, .. } => unsafe {
                WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_TIMEOUT
            },
            Host::WindowsTerminal { signals, .. } => !is_signaled(&signals.done),
        }
    }

    fn ready(&self) -> bool {
        match &self.host {
            Host::Console { .. } => !self.window.is_null(),
            Host::WindowsTerminal { signals, .. } => is_signaled(&signals.ready),
        }
    }

    fn owns_window(&self) -> bool {
        match &self.host {
            Host::Console { job, .. } => window_belongs_to_job(self.window, job.as_raw_handle()),
            Host::WindowsTerminal { title, .. } => is_terminal_window(self.window, title),
        }
    }

    fn startup_error(&mut self) -> Option<String> {
        match &mut self.host {
            Host::Console { process, .. } => {
                let mut code = 0;
                (unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } != 0
                    && code != 0
                    && code != STILL_ACTIVE as u32)
                    .then(|| {
                        trf!(
                            "The Agent terminal exited immediately (code 0x{code:X}).",
                            "Agent 终端启动后立即退出（代码 0x{code:X}）。"
                        )
                    })
            }
            Host::WindowsTerminal { launcher, .. } => match launcher.try_wait() {
                Ok(Some(status)) if !status.success() => Some(trf!(
                    "Windows Terminal did not start: {status}",
                    "Windows Terminal 启动失败：{status}"
                )),
                Err(error) => Some(trf!(
                    "Could not check Windows Terminal startup: {error}",
                    "无法检查 Windows Terminal 启动状态：{error}"
                )),
                Ok(_) => None,
            },
        }
    }

    fn refresh_window(&mut self) {
        if unsafe { IsWindow(self.window) } != 0 {
            return;
        }
        self.window = match &self.host {
            Host::Console { job, .. } => find_console(job.as_raw_handle()),
            Host::WindowsTerminal { title, .. } => {
                find_window(|hwnd| is_terminal_window(hwnd, title))
            }
        };
        self.fitted = None;
    }

    fn close_console(&mut self) {
        if self.closing.is_some() {
            return;
        }
        self.refresh_window();
        let now = Instant::now();
        if self.owns_window() {
            unsafe { PostMessageW(self.window, WM_CLOSE, 0, 0) };
            self.closing = Some(now + CLOSE_GRACE);
        } else {
            self.closing = Some(now);
        }
    }

    fn fit(&mut self, bounds: RECT) {
        let target = [bounds.left, bounds.top, bounds.right, bounds.bottom];
        if self.fitted == Some(target)
            || bounds.left >= bounds.right
            || bounds.top >= bounds.bottom
            || !self.owns_window()
        {
            return;
        }
        unsafe {
            if IsZoomed(self.window) != 0 || IsIconic(self.window) != 0 {
                ShowWindow(self.window, SW_RESTORE);
            }
            let mut target = bounds;
            let mut window = RECT::default();
            let mut frame = RECT::default();
            if GetWindowRect(self.window, &mut window) != 0
                && DwmGetWindowAttribute(
                    self.window,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    (&raw mut frame).cast(),
                    size_of::<RECT>() as u32,
                ) >= 0
            {
                target.left -= frame.left - window.left;
                target.top -= frame.top - window.top;
                target.right += window.right - frame.right;
                target.bottom += window.bottom - frame.bottom;
            }
            if SetWindowPos(
                self.window,
                HWND_TOP,
                target.left,
                target.top,
                target.right - target.left,
                target.bottom - target.top,
                SWP_SHOWWINDOW,
            ) != 0
            {
                if self.fitted.is_none() {
                    SetForegroundWindow(self.window);
                }
                self.fitted = Some([bounds.left, bounds.top, bounds.right, bounds.bottom]);
            }
        }
    }
}

impl Drop for AgentTerminal {
    fn drop(&mut self) {
        if let Host::WindowsTerminal { signals, .. } = &self.host {
            unsafe { SetEvent(signals.stop.as_raw_handle()) };
        }
    }
}

fn is_terminal_window(hwnd: HWND, title: &str) -> bool {
    if !window_has_class(hwnd, "CASCADIA_HOSTING_WINDOW_CLASS") {
        return false;
    }
    unsafe {
        let mut caption = [0u16; 256];
        let count = GetWindowTextW(hwnd, caption.as_mut_ptr(), caption.len() as i32) as usize;
        if !String::from_utf16_lossy(&caption[..count]).ends_with(title) {
            return false;
        }
        let Some(process) = window_process(hwnd) else {
            return false;
        };
        let mut image = [0u16; 1024];
        let mut count = image.len() as u32;
        QueryFullProcessImageNameW(process.as_raw_handle(), 0, image.as_mut_ptr(), &mut count) != 0
            && Path::new(&OsString::from_wide(&image[..count as usize]))
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("WindowsTerminal.exe"))
    }
}

fn window_has_class(hwnd: HWND, expected: &str) -> bool {
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) } as usize;
    class[..len].iter().copied().eq(expected.encode_utf16())
}

fn window_process(hwnd: HWND) -> Option<OwnedHandle> {
    let mut pid = 0;
    unsafe {
        if GetWindowThreadProcessId(hwnd, &mut pid) == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        (!process.is_null()).then(|| OwnedHandle::from_raw_handle(process))
    }
}

fn window_belongs_to_job(hwnd: HWND, job: HANDLE) -> bool {
    let Some(process) = window_process(hwnd) else {
        return false;
    };
    unsafe {
        let mut assigned = 0;
        IsProcessInJob(process.as_raw_handle(), job, &mut assigned) != 0 && assigned != 0
    }
}

fn find_console(job: HANDLE) -> HWND {
    find_window(|hwnd| {
        window_has_class(hwnd, "ConsoleWindowClass") && window_belongs_to_job(hwnd, job)
    })
}

fn find_window<F: Fn(HWND) -> bool>(matches: F) -> HWND {
    struct Search<F> {
        matches: F,
        result: HWND,
    }
    unsafe extern "system" fn visit<F: Fn(HWND) -> bool>(hwnd: HWND, param: LPARAM) -> i32 {
        unsafe {
            let search = &mut *(param as *mut Search<F>);
            if IsWindowVisible(hwnd) != 0 && (search.matches)(hwnd) {
                search.result = hwnd;
                return 0;
            }
            1
        }
    }
    let mut search = Search {
        matches,
        result: null_mut(),
    };
    unsafe { EnumWindows(Some(visit::<F>), (&raw mut search) as LPARAM) };
    search.result
}

fn system_directory() -> io::Result<PathBuf> {
    let mut buffer = [0u16; 260];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

fn console_environment(shim: Option<OsString>) -> Vec<u16> {
    let mut vars = BTreeMap::new();
    for (name, value) in env::vars_os() {
        let upper = name.to_ascii_uppercase();
        if upper != "NO_COLOR" && upper != SHIM_ENV {
            vars.insert(upper, (name, value));
        }
    }
    for (name, value) in COLOR_ENV {
        vars.insert(name.into(), (name.into(), value.into()));
    }
    if let Some(shim) = shim {
        vars.insert(SHIM_ENV.into(), (SHIM_ENV.into(), shim));
    }
    vars.into_values()
        .flat_map(|(name, value)| {
            let mut entry = name;
            entry.push("=");
            entry.push(value);
            wide(&entry)
        })
        .chain([0])
        .collect()
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

fn push_quoted(command: &mut OsString, path: &OsStr) {
    command.push("\"");
    command.push(path);
    command.push("\"");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use windows_sys::Win32::System::StationsAndDesktops::*;

    #[test]
    fn windows_terminal_reuses_the_named_window_and_controls_tabs_individually() {
        let launch = Launch {
            cwd: PathBuf::from(r"D:\My projects\Example"),
            session: "01234567-1234-1234-1234-0123456789ab".into(),
            cli: PathBuf::from(r"C:\My tools\claude.exe"),
        };
        let command = launch.windows_terminal_command(
            Path::new(r"C:\alias\wt.exe"),
            Path::new(r"C:\Windows\System32"),
            Path::new(r"C:\My Tools\ClaudeCodePanel.exe"),
            "ClaudePanelAgent-42",
            "ClaudePanelAgent-42-1",
            "Agent Fixture",
        );
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(&args[..3], ["--window", "ClaudePanelAgent-42", "new-tab"]);
        assert_eq!(
            &args[3..7],
            [
                "--title",
                "Agent Fixture",
                "--suppressApplicationTitle",
                "--inheritEnvironment"
            ]
        );
        assert_eq!(args[8], launch.cwd.as_os_str());
        assert_eq!(
            args[10],
            Path::new(r"C:\Windows\System32\cmd.exe").as_os_str()
        );
        let value = command
            .get_envs()
            .find_map(|(name, value)| (name == SHIM_ENV).then_some(value).flatten())
            .unwrap();
        assert_eq!(
            value,
            r#""C:\My Tools\ClaudeCodePanel.exe" --agent-tab ClaudePanelAgent-42-1 01234567-1234-1234-1234-0123456789ab "C:\My tools\claude.exe""#
        );
    }

    #[test]
    fn managed_console_owns_its_window_and_closes_only_its_job() -> io::Result<()> {
        let desktop_name = format!("ClaudePanelTest-{}-{}", std::process::id(), unsafe {
            GetCurrentThreadId()
        });
        let name = wide(OsStr::new(&desktop_name));
        let mut startup_desktop = wide(OsStr::new(&format!("WinSta0\\{desktop_name}")));
        let desktop = unsafe {
            CreateDesktopW(
                name.as_ptr(),
                null(),
                null(),
                0,
                DESKTOP_CREATEWINDOW
                    | DESKTOP_ENUMERATE
                    | DESKTOP_READOBJECTS
                    | DESKTOP_WRITEOBJECTS,
                null(),
            )
        };
        if desktop.is_null() {
            return Err(io::Error::last_os_error());
        }
        struct Desktop(HDESK, HDESK);
        impl Drop for Desktop {
            fn drop(&mut self) {
                unsafe {
                    SetThreadDesktop(self.1);
                    CloseDesktop(self.0);
                }
            }
        }
        let previous = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        let _desktop = Desktop(desktop, previous);
        if unsafe { SetThreadDesktop(desktop) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let system = system_directory()?;
        let conhost = system.join("conhost.exe");
        let mut command = OsString::new();
        push_quoted(&mut command, conhost.as_os_str());
        command.push(" -ForceNoHandoff ");
        push_quoted(&mut command, system.join("cmd.exe").as_os_str());
        command.push(" /d /v:on /c cd . && !CLAUDE_PANEL_AGENT_COMMAND!");
        let working_dir = env::temp_dir();
        let fixture = working_dir.join(format!("{desktop_name} & %USERNAME%.cmd"));
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&fixture)?
            .write_all(b"@echo off\r\ntitle %~2 FROM %CD%\r\npause >nul\r\n")?;
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let fixture = Fixture(fixture);
        let for_title = |title: &str| {
            let mut command = OsString::new();
            push_quoted(&mut command, fixture.0.as_os_str());
            command.push(" --resume ");
            push_quoted(&mut command, OsStr::new(title));
            Some(command)
        };
        let (job, process) = spawn_in_job(
            &conhost,
            &command,
            &working_dir,
            for_title("Claude Panel Agent Fixture"),
            startup_desktop.as_mut_ptr(),
            CREATE_NO_WINDOW,
        )?;
        let (other_job, other) = spawn_in_job(
            &conhost,
            &command,
            &working_dir,
            for_title("Unrelated Console Fixture"),
            startup_desktop.as_mut_ptr(),
            CREATE_NO_WINDOW,
        )?;
        let started = Instant::now();
        let mut last_title = String::new();
        let window = loop {
            let window = find_console(job.as_raw_handle());
            if !window.is_null() {
                let mut title = [0u16; 100];
                let len = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
                last_title = String::from_utf16_lossy(&title[..len as usize]);
                if last_title.contains("Claude Panel Agent Fixture") {
                    break window;
                }
            }
            if started.elapsed() >= WINDOW_TIMEOUT {
                let mut code = 0;
                unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) };
                panic!(
                    "the managed console did not open with its test title, window={window:?}, title={last_title}, exit={code:#x}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut active = AgentTerminal {
            host: Host::Console { job, process },
            window,
            cwd: working_dir,
            session: String::new(),
            started,
            closing: None,
            fitted: None,
        };
        let expected = active.cwd.to_string_lossy();
        assert!(
            last_title.to_ascii_lowercase().contains(
                &format!("FROM {}", expected.trim_end_matches('\\')).to_ascii_lowercase()
            ),
            "{last_title}"
        );
        assert!(active.owns_window());
        assert!(!window_belongs_to_job(window, other_job.as_raw_handle()));
        active.fit(RECT {
            left: 440,
            top: 0,
            right: 1250,
            bottom: 750,
        });
        assert_eq!(active.fitted, Some([440, 0, 1250, 750]));
        active.close_console();
        let Host::Console { process, .. } = &active.host else {
            unreachable!()
        };
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 2000) },
            WAIT_OBJECT_0
        );
        assert_eq!(
            unsafe { WaitForSingleObject(other.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        if let Some(helper) = env::var_os("CLAUDE_PANEL_TEST_HELPER") {
            let token = format!("ClaudePanelAgent-{}-Test", std::process::id());
            let signals = TabSignals::create(&token)?;
            let mut shim = OsString::new();
            push_quoted(&mut shim, &helper);
            shim.push(format!(
                " {TAB_HELPER_ARG} {token} 01234567-1234-1234-1234-0123456789ab "
            ));
            push_quoted(&mut shim, fixture.0.as_os_str());
            let (_tab_job, tab) = spawn_in_job(
                &conhost,
                &command,
                &active.cwd,
                Some(shim),
                startup_desktop.as_mut_ptr(),
                CREATE_NO_WINDOW,
            )?;
            assert_eq!(
                unsafe { WaitForSingleObject(signals.ready.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
            unsafe { SetEvent(signals.stop.as_raw_handle()) };
            assert_eq!(
                unsafe { WaitForSingleObject(signals.done.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
            assert_eq!(
                unsafe { WaitForSingleObject(tab.as_raw_handle(), 5000) },
                WAIT_OBJECT_0
            );
            assert_eq!(
                unsafe { WaitForSingleObject(other.as_raw_handle(), 0) },
                WAIT_TIMEOUT
            );
        }
        Ok(())
    }
}
