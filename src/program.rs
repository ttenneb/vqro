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
    #[test] fn windows_program_suffix_is_added_once() {
        assert_eq!(candidate(Path::new("bin"),"vqro-herdr",".exe"),Path::new("bin").join("vqro-herdr.exe"));
        assert_eq!(candidate(Path::new("bin"),"vqro-herdr.exe",".exe"),Path::new("bin").join("vqro-herdr.exe"));
    }
}
