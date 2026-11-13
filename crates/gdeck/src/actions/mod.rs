//! Carrying out what a key is bound to.
//!
//! Two rules shape everything here.
//!
//! **Nothing is resolved through a shell or through `PATH`.** A launch uses
//! `CreateProcessW` with `lpApplicationName` set to the absolute path, which
//! means Windows does not search anywhere or parse the string looking for a
//! program. This closes the classic unquoted-path problem outright, and it
//! means what a key does cannot change because something else got installed.
//!
//! **Targets are re-validated at press time, not just at parse time.** A config
//! loaded minutes ago could have been changed since, and the check costs
//! nothing compared to starting a process.

pub mod ffi;

use core::ptr;
use std::path::PathBuf;
use std::sync::Once;

use crate::config::{classify_target, Action, Slot, TargetKind};
use crate::ffi::{wide, ReadFile};
use crate::security::Policy;
use crate::{store, Error, Result};

use ffi::*;

/// How long to wait for a shell command before leaving it to run on alone.
const SHELL_WAIT_MS: u32 = 15_000;

/// Cap on captured output, so a chatty command cannot grow without bound.
const MAX_SHELL_OUTPUT: usize = 64 * 1024;

/// The result of running a command line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShellRun {
    pub process_id: u32,
    /// `None` if it was still running when we stopped waiting.
    pub exit_code: Option<u32>,
    /// Captured output. Empty when the command was given its own console,
    /// because then its output goes there instead of to us.
    pub output: String,
}

/// What actually happened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Performed {
    /// The key is unassigned.
    Nothing,
    /// A new process was started.
    Launched { process_id: u32 },
    /// An already running window was brought forward instead.
    Focused,
    /// Handed to the shell's registered handler.
    Opened,
    /// The interface should show this note. Phase 4 draws it.
    ShowNote { path: PathBuf },
    /// A shell command was run.
    RanShell(ShellRun),
}

/// What the action engine needs to know beyond the slot itself.
pub struct Context<'a> {
    /// Mirrors the config setting. Checked again here on purpose.
    pub shell_enabled: bool,
    pub policy: &'a Policy,
}

fn refused(reason: impl Into<String>) -> Error {
    Error::Refused {
        reason: reason.into(),
    }
}

/// COM is required by some shell handlers. Initialise once, ignore the result:
/// `S_FALSE` simply means it was already initialised on this thread.
fn ensure_com() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        CoInitializeEx(
            ptr::null_mut(),
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );
    });
}

/// Build a command line for `CreateProcessW`.
///
/// `lpApplicationName` already names the executable, so this exists only
/// because convention says argv[0] should be the program. The path is quoted;
/// a double quote cannot legally appear in a Windows path, so one showing up
/// here means the config was crafted and is refused by the caller.
fn command_line(executable: &str, args: &str) -> Vec<u16> {
    let mut line = String::with_capacity(executable.len() + args.len() + 4);
    line.push('"');
    line.push_str(executable);
    line.push('"');
    if !args.is_empty() {
        line.push(' ');
        line.push_str(args);
    }
    wide(&line)
}

/// Re-check a target immediately before acting on it.
fn revalidate(target: &str) -> Result<TargetKind> {
    if target.contains('"') {
        return refused_err("target contains a double quote, which no real Windows path does");
    }
    classify_target(target).map_err(refused).and_then(|kind| {
        if kind == TargetKind::Unc {
            refused_err("UNC targets are refused")
        } else {
            Ok(kind)
        }
    })
}

fn refused_err<T>(reason: &str) -> Result<T> {
    Err(refused(reason))
}

/// Full path of the image backing a process, if it can be read.
fn process_image_path(process_id: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if handle == 0 {
            return None;
        }
        let mut buf = vec![0u16; 32768];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

struct Candidates {
    windows: Vec<(isize, u32)>,
}

extern "system" fn collect_windows(window: isize, param: isize) -> i32 {
    unsafe {
        // Only top level windows a person could actually switch to.
        if IsWindowVisible(window) == 0 || GetWindowTextLengthW(window) == 0 {
            return 1;
        }
        let mut process_id: u32 = 0;
        GetWindowThreadProcessId(window, &mut process_id);
        if process_id != 0 {
            let candidates = &mut *(param as *mut Candidates);
            candidates.windows.push((window, process_id));
        }
    }
    1
}

/// A visible window belonging to a process running this exact image.
pub fn find_window_for(executable: &str) -> Option<isize> {
    let mut candidates = Candidates {
        windows: Vec::new(),
    };
    unsafe {
        EnumWindows(collect_windows, &mut candidates as *mut Candidates as isize);
    }

    let wanted = executable.to_ascii_lowercase();
    for (window, process_id) in candidates.windows {
        if let Some(path) = process_image_path(process_id) {
            if path.to_ascii_lowercase() == wanted {
                return Some(window);
            }
        }
    }
    None
}

/// Bring a window forward.
///
/// `SetForegroundWindow` is restricted: Windows refuses it from a process that
/// does not own the foreground or a recent input event. Our key presses arrive
/// over a HID pipe rather than the normal input queue, so the system does not
/// credit us with input and this can legitimately fail. The window is still
/// restored if it was minimised, so the press is not a complete no op, and the
/// caller is told which happened rather than being left to guess.
pub fn focus_window(window: isize) -> bool {
    unsafe {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        SetForegroundWindow(window) != 0
    }
}

fn start_process(
    executable: &str,
    args: &str,
    workdir: &str,
    creation_flags: u32,
    show: u16,
) -> Result<u32> {
    let application = wide(executable);
    let mut line = command_line(executable, args);
    let directory = if workdir.is_empty() {
        None
    } else {
        Some(wide(workdir))
    };

    // STARTF_USESHOWWINDOW
    const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
    let uses_show = show != u16::MAX;
    let mut startup = StartupInfoW {
        flags: if uses_show { STARTF_USESHOWWINDOW } else { 0 },
        show_window: if uses_show { show } else { 0 },
        ..Default::default()
    };
    let mut process = ProcessInformation::default();

    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            // Never inherit handles. A launched program has no business
            // receiving ours.
            0,
            creation_flags | CREATE_UNICODE_ENVIRONMENT,
            ptr::null_mut(),
            directory
                .as_ref()
                .map(|d| d.as_ptr())
                .unwrap_or(ptr::null()),
            &mut startup,
            &mut process,
        )
    };

    if ok == 0 {
        return Err(Error::Win32 {
            what: "CreateProcessW",
            code: unsafe { GetLastError() },
        });
    }

    unsafe {
        CloseHandle(process.thread);
        CloseHandle(process.process);
    }
    Ok(process.process_id)
}

/// Explain an `SE_ERR_*` value from `hInstApp`.
///
/// `ShellExecuteExW` is awkward: it can return nonzero while having done
/// nothing at all. The documented success signal is `hInstApp` greater than
/// 32, and on failure that field holds one of these instead. Ignoring it means
/// reporting that a key opened something when nothing happened, which is
/// exactly the sort of quiet lie this project should not tell.
fn shell_error(code: isize) -> String {
    match code {
        0 => "the system is out of memory or resources".to_string(),
        2 => "the file was not found".to_string(),
        3 => "the path was not found".to_string(),
        5 => "access was denied".to_string(),
        8 => "there was not enough memory to finish".to_string(),
        26 => "a sharing violation occurred".to_string(),
        27 => "the file association is incomplete or invalid".to_string(),
        28 => "the request timed out".to_string(),
        29 | 30 => "the handler refused or was busy".to_string(),
        31 => "no application is associated with this file type. Check the \
             association in Windows Settings, or bind the key to the program \
             directly with a launch action instead."
            .to_string(),
        32 => "the required library was not found".to_string(),
        other => format!("the shell refused it (SE_ERR {})", other),
    }
}

/// Hand something to its registered handler.
fn shell_open(target: &str, verb: &str) -> Result<()> {
    ensure_com();

    let file = wide(target);
    let verb = wide(verb);
    let mut info = ShellExecuteInfoW {
        mask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        show: SW_SHOWNORMAL,
        ..Default::default()
    };

    let ok = unsafe { ShellExecuteExW(&mut info) };

    if info.process != 0 {
        unsafe { CloseHandle(info.process) };
    }

    // Both checks matter. The return value catches outright failure; hInstApp
    // catches the case where the call "succeeded" but the shell did nothing.
    if ok == 0 {
        return Err(Error::Win32 {
            what: "ShellExecuteExW",
            code: unsafe { GetLastError() },
        });
    }
    if info.inst_app <= 32 {
        return Err(refused(format!(
            "could not open {}: {}",
            target,
            shell_error(info.inst_app)
        )));
    }

    Ok(())
}

/// Where a note lives on disk.
pub fn note_path(name: &str) -> Result<PathBuf> {
    Ok(store::store_dir()?
        .join(store::NOTES_DIR)
        .join(format!("{}.txt", name)))
}

fn ensure_note(name: &str, policy: &Policy) -> Result<PathBuf> {
    let dir = store::ensure_store_dir()?.join(store::NOTES_DIR);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io {
            what: "create the notes directory",
            detail: e.to_string(),
        })?;
        crate::security::apply(&dir, &policy.directory_sddl())?;
    }

    let path = dir.join(format!("{}.txt", name));
    if !path.exists() {
        std::fs::write(&path, "").map_err(|e| Error::Io {
            what: "create the note",
            detail: e.to_string(),
        })?;
        crate::security::apply(&path, &policy.file_sddl())?;
    }
    Ok(path)
}

/// Carry out a key's action.
pub fn perform(slot: &Slot, context: &Context<'_>) -> Result<Performed> {
    match &slot.action {
        Action::None => Ok(Performed::Nothing),

        Action::Launch {
            target,
            args,
            workdir,
            focus_if_running,
        } => {
            let kind = revalidate(target)?;

            if *focus_if_running && kind == TargetKind::AbsolutePath {
                if let Some(window) = find_window_for(target) {
                    if focus_window(window) {
                        return Ok(Performed::Focused);
                    }
                    // Refusing to start a second copy. The window was found and
                    // restored; Windows simply would not let us raise it.
                    return Err(refused(
                        "the program is already running but Windows refused the \
                         foreground change. It has been restored if it was minimised.",
                    ));
                }
            }

            match kind {
                TargetKind::AbsolutePath => {
                    let process_id = start_process(target, args, workdir, 0, SW_SHOWNORMAL as u16)?;
                    Ok(Performed::Launched { process_id })
                }
                // A URI has no executable to name, so it must go to the handler.
                TargetKind::Uri(_) => {
                    shell_open(target, "open")?;
                    Ok(Performed::Opened)
                }
                TargetKind::Unc => refused_err("UNC targets are refused"),
            }
        }

        Action::Open { target } => {
            revalidate(target)?;
            shell_open(target, "open")?;
            Ok(Performed::Opened)
        }

        Action::Note { name } => {
            let path = ensure_note(name, context.policy)?;
            Ok(Performed::ShowNote { path })
        }

        Action::Shell { command, hidden } => {
            // The parser already refuses shell actions when the setting is off.
            // Checked again here so that the guarantee does not depend on the
            // config having come through that path.
            if !context.shell_enabled {
                return refused_err(
                    "shell actions are disabled. Set shell_enabled in the config to allow them.",
                );
            }
            if command.contains('"') {
                return refused_err("shell command contains a double quote");
            }

            Ok(Performed::RanShell(run_shell(command, *hidden)?))
        }
    }
}

/// Run a command line through the shell and find out how it went.
///
/// The two modes are genuinely different, not cosmetic:
///
/// * **Hidden** redirects the command's output to a pipe we own, waits for it
///   to finish, and reports the exit code and what it printed. A hidden window
///   that tells you nothing is indistinguishable from one that did nothing.
/// * **Shown** gives the command a console of its own with
///   `CREATE_NEW_CONSOLE`. Without that flag a child started from a process
///   that already owns a console simply attaches to it, so no window appears
///   and the option looks broken. Its output goes to that console, so there is
///   nothing for us to capture.
///
/// Handle inheritance is switched on only for the hidden case, and only the
/// pipe's write end is marked inheritable. The read end is explicitly cleared
/// with `SetHandleInformation`, so the child cannot read back what it wrote.
fn run_shell(command: &str, hidden: bool) -> Result<ShellRun> {
    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string());

    let application = wide(&comspec);
    let mut line = command_line(&comspec, &format!("/C {}", command));

    let mut startup = StartupInfoW::default();
    let mut read_end: isize = 0;
    let mut write_end: isize = 0;

    let (inherit_handles, creation_flags) = if hidden {
        let mut attributes = SecurityAttributes {
            length: core::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: ptr::null_mut(),
            inherit_handle: 1,
        };
        if unsafe { CreatePipe(&mut read_end, &mut write_end, &mut attributes, 0) } == 0 {
            return Err(Error::Win32 {
                what: "CreatePipe",
                code: unsafe { GetLastError() },
            });
        }
        // Only the write end crosses into the child.
        unsafe { SetHandleInformation(read_end, HANDLE_FLAG_INHERIT, 0) };

        startup.flags |= STARTF_USESTDHANDLES;
        startup.std_output = write_end;
        startup.std_error = write_end;
        startup.std_input = 0;
        (1, CREATE_NO_WINDOW)
    } else {
        (0, CREATE_NEW_CONSOLE)
    };

    let mut process = ProcessInformation::default();
    let started = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            inherit_handles,
            creation_flags | CREATE_UNICODE_ENVIRONMENT,
            ptr::null_mut(),
            ptr::null(),
            &mut startup,
            &mut process,
        )
    };

    if started == 0 {
        let code = unsafe { GetLastError() };
        if write_end != 0 {
            unsafe {
                CloseHandle(write_end);
                CloseHandle(read_end);
            }
        }
        return Err(Error::Win32 {
            what: "CreateProcessW",
            code,
        });
    }
    unsafe { CloseHandle(process.thread) };

    let mut output = String::new();
    if hidden {
        // Our own copy of the write end has to go, or the read below never
        // reaches end of file and this would hang forever.
        unsafe { CloseHandle(write_end) };

        let mut chunk = [0u8; 4096];
        let mut raw: Vec<u8> = Vec::new();
        loop {
            let mut got: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    read_end,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    &mut got,
                    ptr::null_mut(),
                )
            };
            if ok == 0 || got == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..got as usize]);
            if raw.len() >= MAX_SHELL_OUTPUT {
                break;
            }
        }
        unsafe { CloseHandle(read_end) };
        output = String::from_utf8_lossy(&raw).trim().to_string();
    }

    let waited = unsafe { WaitForSingleObject(process.process, SHELL_WAIT_MS) };
    let exit_code = if waited == WAIT_OBJECT_0 {
        let mut code: u32 = 0;
        if unsafe { GetExitCodeProcess(process.process, &mut code) } != 0 {
            Some(code)
        } else {
            None
        }
    } else {
        None
    };
    unsafe { CloseHandle(process.process) };

    Ok(ShellRun {
        process_id: process.process_id,
        exit_code,
        output,
    })
}

/// Open a path with its registered handler.
///
/// Exposed for the interface's "open the config folder" control. It goes
/// through the same validation and the same `hInstApp` check as an Open
/// action, rather than being a second, looser way to launch things.
pub fn open_path(path: &std::path::Path) -> Result<()> {
    let text = path.to_string_lossy().to_string();
    revalidate(&text)?;
    shell_open(&text, "open")
}

/// A short action name and detail, for the audit log.
pub fn describe(slot: &Slot) -> (&'static str, String) {
    match &slot.action {
        Action::None => ("none", String::new()),
        Action::Launch { target, .. } => ("launch", target.clone()),
        Action::Open { target } => ("open", target.clone()),
        Action::Note { name } => ("note", name.clone()),
        Action::Shell { command, .. } => ("shell", command.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_executable_is_quoted_in_the_command_line() {
        let line = command_line(r"C:\Program Files\App\run.exe", "-silent");
        let text = String::from_utf16_lossy(&line[..line.len() - 1]);
        assert_eq!(text, "\"C:\\Program Files\\App\\run.exe\" -silent");
    }

    #[test]
    fn no_args_means_no_trailing_space() {
        let line = command_line(r"C:\a\b.exe", "");
        let text = String::from_utf16_lossy(&line[..line.len() - 1]);
        assert_eq!(text, "\"C:\\a\\b.exe\"");
    }

    #[test]
    fn a_quote_in_a_target_is_refused_at_press_time() {
        // No legal Windows path contains a double quote, so one here means the
        // config was crafted to break out of the quoting above.
        let err = revalidate(r#"C:\a" & calc.exe & "\b.exe"#).unwrap_err();
        assert!(format!("{}", err).contains("double quote"));
    }

    #[test]
    fn relative_targets_are_refused_again_at_press_time() {
        // Even though the parser already refused them. The config could have
        // changed since it was loaded.
        assert!(revalidate("notepad.exe").is_err());
        assert!(revalidate(r"..\thing.exe").is_err());
    }

    #[test]
    fn unc_targets_are_refused_at_press_time() {
        let err = revalidate(r"\\server\share\app.exe").unwrap_err();
        assert!(format!("{}", err).contains("UNC"));
    }

    #[test]
    fn absolute_paths_and_allowed_uris_pass() {
        assert_eq!(
            revalidate(r"C:\Windows\System32\notepad.exe").unwrap(),
            TargetKind::AbsolutePath
        );
        assert_eq!(
            revalidate("https://example.com").unwrap(),
            TargetKind::Uri("https".to_string())
        );
    }

    #[test]
    fn the_no_association_case_is_explained_rather_than_shrugged_at() {
        // SE_ERR_NOASSOC. This is the one a person will actually hit, and
        // "error 31" would tell them nothing.
        let message = shell_error(31);
        assert!(message.contains("no application is associated"));
        assert!(
            message.contains("launch action"),
            "it should suggest the way out"
        );
    }

    #[test]
    fn every_failing_hinstapp_value_is_treated_as_failure() {
        // Anything at or below 32 is an error, including 0 and 32 itself.
        for code in [0, 2, 3, 5, 8, 26, 31, 32] {
            assert!(
                !shell_error(code).is_empty(),
                "code {} has no message",
                code
            );
        }
    }

    #[test]
    fn a_hidden_command_reports_what_it_printed() {
        // The point of hidden mode: a window you cannot see has to tell you
        // what it did, or running it is indistinguishable from doing nothing.
        let run = run_shell("echo gdeck-capture-check", true).expect("should run");
        assert_eq!(run.exit_code, Some(0));
        assert!(
            run.output.contains("gdeck-capture-check"),
            "captured nothing useful: {:?}",
            run.output
        );
    }

    #[test]
    fn a_failing_command_reports_a_non_zero_exit_code() {
        let run = run_shell("exit /b 3", true).expect("should run");
        assert_eq!(run.exit_code, Some(3), "a failure was reported as success");
    }

    #[test]
    fn stderr_is_captured_too_not_just_stdout() {
        // A command that only complains must not look silent.
        let run = run_shell("echo trouble 1>&2", true).expect("should run");
        assert!(
            run.output.contains("trouble"),
            "stderr was lost: {:?}",
            run.output
        );
    }

    #[test]
    fn describe_reports_the_target_for_the_audit_log() {
        let slot = Slot {
            label: "Steam".to_string(),
            action: Action::Launch {
                target: r"C:\Steam\steam.exe".to_string(),
                args: String::new(),
                workdir: String::new(),
                focus_if_running: true,
            },
        };
        let (action, detail) = describe(&slot);
        assert_eq!(action, "launch");
        assert_eq!(detail, r"C:\Steam\steam.exe");
    }
}
