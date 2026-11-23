//! Resolve portable provider program names when a supervised process starts, never in stored defaults.
use std::path::{Path,PathBuf};

/// A stdio-only service child must not allocate its own Windows console. Redirecting
/// stdio alone does not prevent a window, especially when the core itself is detached.
/// Never use this for backend-owned interactive terminals or UI clients.
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    background_command_with(program, cfg!(windows), |command, flags| {
        #[cfg(windows)]
        command.creation_flags(flags);
        #[cfg(not(windows))]
        let _ = (command, flags);
    })
}

// Preserve the native-tested Tokio API while permitting a powerless flag setter on Linux.
const BACKGROUND_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const _: () = assert!(BACKGROUND_NO_WINDOW == windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
fn background_command_with(
    program: impl AsRef<std::ffi::OsStr>, windows: bool,
    set_flags: impl FnOnce(&mut tokio::process::Command, u32),
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    if windows { set_flags(&mut command, BACKGROUND_NO_WINDOW); }
    command
}

fn candidate(dir:&Path, name:&str, suffix:&str) -> PathBuf {
    dir.join(if !suffix.is_empty() && !name.to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase()) { format!("{name}{suffix}") } else { name.into() })
}
fn executable(path:&Path) -> bool {
    let Ok(meta)=std::fs::metadata(path) else{return false}; if !meta.is_file(){return false}
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; if meta.permissions().mode()&0o111==0{return false} }
    true
}
/// The program and leading arguments that run argv[0] of an extension or client: `python3` names "a Python 3"
/// (see `python`), `vqro-*` an extension program (`resolve_provider`), anything else runs as written.
pub fn resolve_argv0(name:&str) -> Result<(PathBuf,Vec<String>),String> {
    if name=="python3" { return python(); }
    resolve_provider(name).map(|program|(program,Vec::new()))
}

/// A literal `python3` argv[0] means "a Python 3 interpreter": `VQRO_PYTHON` (one program path) on every platform,
/// else the first Unix PATH/Homebrew interpreter >= 3.11. Windows installs no `python3` from python.org, so there it is the first found walking PATH
/// of `py -3` (the python.org launcher), `python`, `python3` (that order within one directory). Directories named
/// `WindowsApps` are skipped entirely: their `python*.exe` may be stubs that open the Store instead.
pub fn python() -> Result<(PathBuf,Vec<String>),String> {
    let explicit = std::env::var_os("VQRO_PYTHON").filter(|p| !p.is_empty()).map(PathBuf::from);
    let path = std::env::var_os("PATH").unwrap_or_default();
    #[cfg(unix)]
    {
        unix_python(explicit, std::env::split_paths(&path).collect(),
            &[PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")], python_version_until)
            .map(|program| (program, Vec::new()))
    }
    #[cfg(not(unix))]
    {
        if let Some(program) = explicit { return Ok((program, Vec::new())); }
        windows_python(std::env::split_paths(&path).collect(), executable)
            .ok_or_else(||"no Python 3 found on PATH (tried py -3, python, python3); install Python 3.11+ or set VQRO_PYTHON to python.exe".into())
    }
}

#[cfg(unix)]
fn unix_python(explicit: Option<PathBuf>, mut dirs: Vec<PathBuf>, fallback: &[PathBuf],
               probe: impl Fn(&Path, std::time::Instant) -> Result<(u32, u32), String>) -> Result<PathBuf, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let overridden = explicit.is_some();
    let candidates = if let Some(program) = explicit {
        // A bare override uses PATH only, never a fallback directory. Relative paths and PATH
        // entries are anchored to the selecting process, not the eventual extension/UI cwd.
        let program = if program.components().count() == 1 && !program.is_absolute() {
            dirs.iter().map(|dir| dir.join(&program)).find(|p| executable(p)).unwrap_or(program)
        } else { program };
        vec![program]
    } else {
        for dir in fallback {
            if !dirs.contains(dir) { dirs.push(dir.clone()); }
        }
        dirs.into_iter().map(|dir| dir.join("python3")).collect()
    };
    let mut found = Vec::new();
    for program in candidates {
        if std::time::Instant::now() >= deadline {
            found.push("discovery exceeded its five-second total probe budget".into());
            break;
        }
        let program = std::path::absolute(&program).map_err(|error| format!("cannot anchor Python path: {error}"))?;
        if !overridden && !executable(&program) { continue; }
        match probe(&program, deadline) {
            Ok((major, minor)) if major == 3 && minor >= 11 => return Ok(program),
            Ok((major, minor)) => found.push(format!("{} is {major}.{minor}", program.display())),
            Err(error) => found.push(format!("{}: {error}", program.display())),
        }
    }
    Err(format!("no Python 3.11+ found ({}); install Python 3.11+ or set VQRO_PYTHON to its executable",
        if found.is_empty() { "no executable python3 on PATH or in /opt/homebrew/bin, /usr/local/bin".into() } else { found.join("; ") }))
}

#[cfg(unix)]
fn python_version_until(program: &Path, deadline: std::time::Instant) -> Result<(u32, u32), String> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, TryLockError};
    type Version = Result<(u32, u32), String>;
    type Versions = HashMap<PathBuf, Arc<Mutex<Option<Version>>>>;
    static VERSIONS: OnceLock<Mutex<Versions>> = OnceLock::new();
    // Only identical candidates share a probe. Never hold the cache map across external work.
    let version = VERSIONS.get_or_init(Default::default).lock()
        .map_err(|_| "Python version cache unavailable".to_string())?
        .entry(program.to_path_buf()).or_default().clone();
    let mut version = loop {
        match version.try_lock() {
            Ok(version) => break version,
            Err(TryLockError::Poisoned(_)) => return Err("Python version cache unavailable".into()),
            Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => return Err("Python discovery total probe budget exhausted waiting for cache".into()),
        }
    };
    if let Some(version) = version.as_ref() { return version.clone(); }
    if std::time::Instant::now() >= deadline { return Err("Python discovery total probe budget exhausted".into()); }
    let result = probe_python_version(program, deadline);
    *version = Some(result.clone());
    result
}

#[cfg(unix)]
fn probe_python_version(program: &Path, total_deadline: std::time::Instant) -> Result<(u32, u32), String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new(program).args(["-I", "-S", "-c", "import sys; print(sys.version_info[:2])"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|error| format!("version probe could not start: {error}"))?;
    let deadline = total_deadline.min(Instant::now() + Duration::from_secs(2));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() { return Err(format!("version probe exited with {status}")); }
                let mut output = String::new();
                let stdout = child.stdout.take().unwrap();
                // A descendant retaining stdout must not turn a bounded probe into a hang.
                use std::os::fd::AsRawFd;
                let fd = stdout.as_raw_fd();
                // SAFETY: fd is an owned live pipe; fcntl only changes its read flags.
                if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                    return Err("could not make version output nonblocking".into());
                }
                if let Err(error) = stdout.take(128).read_to_string(&mut output) {
                    if error.kind() != std::io::ErrorKind::WouldBlock { return Err(format!("invalid version output: {error}")); }
                }
                let pair = output.trim().strip_prefix('(').and_then(|s| s.strip_suffix(')'))
                    .and_then(|s| s.split_once(','));
                return pair.and_then(|(major, minor)| Some((major.trim().parse().ok()?, minor.trim().parse().ok()?)))
                    .ok_or_else(|| "invalid version probe output".into());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result { Err(error) => format!("version probe failed: {error}"), _ => "version probe timed out (two-second candidate / five-second total probe budget)".into() });
            }
        }
    }
}

#[cfg(any(not(unix), test))]
fn windows_python(dirs:Vec<PathBuf>, exists:impl Fn(&Path)->bool) -> Option<(PathBuf,Vec<String>)> {
    let store=|dir:&&PathBuf| dir.file_name().is_some_and(|n|n.eq_ignore_ascii_case("WindowsApps"));
    dirs.iter().filter(|d|!store(d))
        .flat_map(|dir|[("py.exe",vec!["-3".to_string()]),("python.exe",vec![]),("python3.exe",vec![])].map(|(name,args)|(dir.join(name),args)))
        .find(|(path,_)|exists(path))
}

pub fn resolve_provider(name:&str) -> Result<PathBuf,String> {
    let path=Path::new(name);
    if !name.starts_with("vqro-") || path.components().count()!=1 || name.contains('\\') { return Ok(path.into()); }
    let sibling=crate::client::executable_path().ok().and_then(|p|p.parent().map(Path::to_path_buf));
    if let Some(dir)=&sibling { let path=candidate(dir,name,std::env::consts::EXE_SUFFIX); if executable(&path){return Ok(path)} }
    if let Some(paths)=std::env::var_os("PATH") { for dir in std::env::split_paths(&paths) { let path=candidate(&dir,name,std::env::consts::EXE_SUFFIX);if executable(&path){return Ok(path)} } }
    Err(format!("extension program {name} not found beside vqro ({}) or on PATH; install extension programs, then use recovery choices",sibling.map(|p|p.display().to_string()).unwrap_or_else(||"unknown executable".into())))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn windows_python_walks_path_with_store_stubs_skipped() {
        let have=|names:&'static [&'static str]| move |p:&Path| names.iter().any(|n|Path::new(n)==p);
        let dirs=|ds:&[&str]| ds.iter().map(PathBuf::from).collect::<Vec<_>>();
        let apps="C:/Users/u/AppData/Local/Microsoft/WindowsApps";
        // Only Store stubs: refuse rather than opening the Store.
        assert_eq!(windows_python(dirs(&[apps,"C:/Windows"]),have(&["C:/Users/u/AppData/Local/Microsoft/WindowsApps/python3.exe"])), None);
        // Never even inspect candidates in a WindowsApps directory (case-insensitive).
        assert_eq!(windows_python(dirs(&[apps,"C:/wInDoWsApPs"]), |_| panic!("Store directory must be skipped")), None);
        // python.org: the launcher wins over a stub that comes earlier on PATH.
        assert_eq!(windows_python(dirs(&[apps,"C:/Windows"]),have(&["C:/Users/u/AppData/Local/Microsoft/WindowsApps/python3.exe","C:/Windows/py.exe"])),
                   Some((Path::new("C:/Windows").join("py.exe"),vec!["-3".into()])));
        // An earlier PATH entry (an activated venv) wins over the launcher.
        assert_eq!(windows_python(dirs(&["C:/venv/Scripts","C:/Windows"]),have(&["C:/venv/Scripts/python.exe","C:/Windows/py.exe"])),
                   Some((Path::new("C:/venv/Scripts").join("python.exe"),vec![])));
        assert_eq!(windows_python(dirs(&["C:/Windows"]),have(&[])),None);
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn background_child_has_no_console_and_retains_stdio() {
        let output = background_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "Add-Type -Namespace VqroTest -Name Console -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow();'; [Console]::Write([VqroTest.Console]::GetConsoleWindow().ToInt64())"])
            .stdin(std::process::Stdio::null())
            .output().await.unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "0");
    }
    #[cfg(unix)]
    fn fake_python(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("python3");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[cfg(unix)]
    #[test]
    fn unix_python_skips_old_versions_and_uses_missing_homebrew_dir() {
        let root = tempfile::tempdir().unwrap();
        let old = fake_python(&root.path().join("apple"), "echo '(3, 9)'");
        let new = fake_python(&root.path().join("brew"), "echo '(3, 12)'");
        let dirs = vec![old.parent().unwrap().into(), new.parent().unwrap().into()];
        assert_eq!(unix_python(None, dirs.clone(), &[], python_version_until).unwrap(), new);
        assert_eq!(unix_python(None, vec![dirs[0].clone()], &[dirs[1].clone()], python_version_until).unwrap(), new);
        // An explicit override wins over PATH; a bad override fails rather than falling back.
        assert_eq!(unix_python(Some(new.clone()), vec![dirs[0].clone()], &[], python_version_until).unwrap(), new);
        let error = unix_python(Some(old.clone()), dirs.clone(), &[], python_version_until).unwrap_err();
        assert!(error.contains("is 3.9") && error.contains("VQRO_PYTHON"), "{error}");
        let error = unix_python(None, vec![dirs[0].clone()], &[], python_version_until).unwrap_err();
        assert!(error.contains("is 3.9"), "{error}");
        assert!(unix_python(None, vec![], &[], python_version_until).unwrap_err().contains("install Python 3.11+"));
        let calls = std::cell::Cell::new(0);
        unix_python(None, dirs.clone(), &dirs, |_, _| { calls.set(calls.get()+1); Err("no".into()) }).unwrap_err();
        assert_eq!(calls.get(), 2, "fallback directories already on PATH are not repeated");
    }
    #[cfg(unix)]
    fn python_version(program: &Path) -> Result<(u32, u32), String> {
        python_version_until(program, std::time::Instant::now() + std::time::Duration::from_secs(5))
    }
    #[cfg(unix)]
    #[test]
    fn unix_version_probes_are_bounded_and_cached() {
        let root = tempfile::tempdir().unwrap();
        let count = root.path().join("calls");
        let path = fake_python(root.path(), &format!("echo called >> '{}'; echo '(3, 12)'", count.display()));
        assert_eq!(python_version(&path).unwrap(), (3, 12));
        assert_eq!(python_version(&path).unwrap(), (3, 12));
        assert_eq!(std::fs::read_to_string(count).unwrap(), "called\n");
        let bad = fake_python(&root.path().join("bad"), "echo nonsense");
        assert!(python_version(&bad).unwrap_err().contains("invalid version"));
        let slow = fake_python(&root.path().join("slow"), "exec /bin/sleep 10");
        let start = std::time::Instant::now();
        assert!(python_version(&slow).unwrap_err().contains("timed out"));
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
        let dirs = (0..4).map(|i| {
            let path = fake_python(&root.path().join(format!("slow-{i}")), "exec /bin/sleep 10");
            path.parent().unwrap().to_path_buf()
        }).collect();
        let start = std::time::Instant::now();
        let error = unix_python(None, dirs, &[], python_version_until).unwrap_err();
        assert!(error.contains("total probe budget"), "{error}");
        assert!(start.elapsed() < std::time::Duration::from_secs(6), "candidate deadlines must share one total budget");
    }
    #[cfg(unix)]
    #[test]
    fn relative_python_path_is_anchored_before_probe_and_launch() {
        let root = tempfile::tempdir_in(".").unwrap();
        let path = fake_python(&root.path().join("bin"), "echo '(3, 12)'");
        let dir = path.parent().unwrap().to_path_buf();
        let selected = unix_python(None, vec![dir.clone()], &[], python_version_until).unwrap();
        assert!(selected.is_absolute());
        assert_eq!(selected, std::path::absolute(&path).unwrap());
        let other = root.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert!(std::process::Command::new(&selected).current_dir(&other).status().unwrap().success());
        assert_eq!(unix_python(Some(path.clone()), vec![], &[], python_version_until).unwrap(), selected);
        assert_eq!(unix_python(Some("python3".into()), vec![dir], &[], python_version_until).unwrap(), selected);
    }
    #[test] fn windows_program_suffix_is_added_once() {
        assert_eq!(candidate(Path::new("bin"),"vqro-herdr",".exe"),Path::new("bin").join("vqro-herdr.exe"));
        assert_eq!(candidate(Path::new("bin"),"vqro-herdr.exe",".exe"),Path::new("bin").join("vqro-herdr.exe"));
    }

    #[test]
    fn background_command_sets_only_no_window_on_windows_and_nothing_on_unix() {
        for windows in [false, true] {
            let mut calls = Vec::new();
            let command = background_command_with("not-executed", windows, |_, flags| calls.push(flags));
            let command = command.as_std();
            assert_eq!(command.get_program(), "not-executed");
            assert_eq!(calls, if windows { vec![0x0800_0000] } else { vec![] });
            assert!(command.get_args().next().is_none());
            assert!(command.get_envs().next().is_none());
            assert!(command.get_current_dir().is_none());
        }
    }

    #[test]
    fn background_spawn_sites_use_the_shared_helper() {
        for (name, source, calls) in [
            ("ext", include_str!("ext.rs"), 1),
            ("herdr connector", include_str!("adapter/herdr.rs"), 1),
            ("tmux control/capture", include_str!("tmux/client.rs"), 2),
            ("tmux CLI/version", include_str!("bin/vqro-tmux.rs"), 2),
            ("Rex CLI", include_str!("bin/vqro-rex/server.rs"), 1),
            ("Codex app-server", include_str!("bin/vqro-bridge-codex.rs"), 1),
            ("Pi version", include_str!("bin/vqro-bridge-pi/main.rs"), 1),
            ("Pi RPC", include_str!("bin/vqro-bridge-pi/native.rs"), 1),
            ("snapshot test child", include_str!("tui/ghostty/mod.rs"), 1),
        ] {
            assert_eq!(source.matches("program::background_command(").count(), calls, "{name}");
            assert!(!source.contains("Command::new("), "unclassified raw constructor: {name}");
        }
        for (name, source, calls) in [
            ("UI launch", include_str!("ui_launch.rs"), 1),
            ("TUI restart", include_str!("tui/mod.rs"), 1),
            ("Unix kill fallback", include_str!("main.rs"), 1),
            ("detached core", include_str!("client.rs"), 1),
            ("Unix Rex fixtures", include_str!("bin/vqro-rex/status_tests.rs"), 2),
            ("unexecuted Pi env fixture", include_str!("bin/vqro-bridge-pi/tests.rs"), 1),
        ] {
            assert_eq!(source.matches("Command::new(").count(), calls, "new constructor: {name}");
        }
        let core = include_str!("client.rs").split_whitespace().collect::<String>();
        assert!(core.contains("cmd.creation_flags(DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP)"));
    }

    #[cfg(windows)]
    #[test]
    fn background_child_round_trips_all_stdio_without_console() {
        use std::io::Write;
        use std::process::Stdio;
        let script = r#"
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }';
$line = [Console]::In.ReadLine();
[Console]::Out.WriteLine('window=' + [ConsoleProbe]::GetConsoleWindow().ToInt64());
[Console]::Out.WriteLine('stdin=' + $line);
[Console]::Error.WriteLine('stderr=intact');
"#;
        let mut child = background_command("powershell.exe").into_std()
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().expect("Windows PowerShell console probe");
        child.stdin.take().unwrap().write_all(b"stdio-intact\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill(); let _ = child.wait();
                panic!("background console probe timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.lines().any(|line| line.trim() == "window=0"), "{stdout}");
        assert!(stdout.lines().any(|line| line.trim() == "stdin=stdio-intact"), "{stdout}");
        assert!(stderr.contains("stderr=intact"), "{stderr}");
    }
}
