//! Resolve portable provider program names when a supervised process starts, never in stored defaults.
use std::path::{Path,PathBuf};

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
/// else `python3` on Unix. Windows installs no `python3` from python.org, so there it is the first found walking PATH
/// of `py -3` (the python.org launcher), `python`, `python3` (that order within one directory). Directories named
/// `WindowsApps` are skipped entirely: their `python*.exe` may be stubs that open the Store instead.
pub fn python() -> Result<(PathBuf,Vec<String>),String> {
    if let Some(program)=std::env::var_os("VQRO_PYTHON").filter(|p|!p.is_empty()) { return Ok((program.into(),Vec::new())); }
    if !cfg!(windows) { return Ok(("python3".into(),Vec::new())); }
    let path=std::env::var_os("PATH").unwrap_or_default();
    windows_python(std::env::split_paths(&path).collect(), executable)
        .ok_or_else(||"no Python 3 found on PATH (tried py -3, python, python3); install Python 3.11+ or set VQRO_PYTHON to python.exe".into())
}

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
    #[test] fn windows_program_suffix_is_added_once() {
        assert_eq!(candidate(Path::new("bin"),"vqro-herdr",".exe"),Path::new("bin").join("vqro-herdr.exe"));
        assert_eq!(candidate(Path::new("bin"),"vqro-herdr.exe",".exe"),Path::new("bin").join("vqro-herdr.exe"));
    }
}
