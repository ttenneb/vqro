//! Where vqro keeps its socket and state. Environment overrides first, then per-user defaults.

use std::path::PathBuf;

use anyhow::Result;
#[cfg(unix)]
use anyhow::bail;

/// `VQRO_STATE_DIR`, else the platform's per-user state directory.
pub fn state_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("VQRO_STATE_DIR") {
        return Ok(PathBuf::from(d));
    }
    #[cfg(unix)]
    {
        if let Ok(d) = std::env::var("XDG_STATE_HOME") {
            return Ok(PathBuf::from(d).join("vqro"));
        }
        Ok(home().join(".local/state/vqro"))
    }
    #[cfg(windows)]
    { Ok(local_appdata()?.join("vqro/state")) }
}

/// `VQRO_EXTENSIONS_DIR`, else the platform's per-user extensions directory.
pub fn extensions_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("VQRO_EXTENSIONS_DIR") {
        return Ok(PathBuf::from(d));
    }
    #[cfg(unix)]
    { Ok(home().join(".local/share/vqro/extensions")) }
    #[cfg(windows)]
    { Ok(local_appdata()?.join("vqro/extensions")) }
}

/// `VQRO_SOCKET`, else `<state_dir>/vqro.sock`.
pub fn socket_path() -> Result<PathBuf> {
    // A terminal-injected endpoint is ambient reachability, not an explicit scratch-run choice.
    let inherited = std::env::var("VQRO_SOCKET_SOURCE").ok().as_deref()==Some("terminal") && std::env::var_os("VQRO_STATE_DIR").is_some();
    let p = match std::env::var("VQRO_SOCKET").ok().filter(|_|!inherited) {
        Some(s) => PathBuf::from(s),
        None => {
            #[cfg(unix)]
            { state_dir()?.join("vqro.sock") }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                let state = state_dir()?;
                // Canonicalize even on first launch, before the store is opened. Scratch state
                // directories get distinct endpoints; relative/symlink aliases share one endpoint.
                std::fs::create_dir_all(&state)?;
                let canonical = state.canonicalize()?;
                // Fixed FNV-1a over UTF-16 bytes, not DefaultHasher's unspecified algorithm.
                let mut hash = 0xcbf29ce484222325u64;
                for byte in canonical.as_os_str().encode_wide().flat_map(u16::to_le_bytes) {
                    hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
                PathBuf::from(format!(r"\\.\pipe\vqro-{hash:016x}"))
            }
        },
    };
    // macOS limits Unix socket paths to 103 bytes, Linux to 107.
    #[cfg(unix)]
    if p.as_os_str().len() > 100 {
        bail!("socket path is too long ({} bytes): {}; set VQRO_SOCKET to a shorter path", p.as_os_str().len(), p.display());
    }
    #[cfg(windows)]
    crate::transport::validate_windows_endpoint(&p)?;
    Ok(p)
}

#[cfg(windows)]
fn local_appdata() -> Result<PathBuf> {
    local_appdata_from(std::env::var_os("LOCALAPPDATA"))
}

#[cfg(windows)]
fn local_appdata_from(value: Option<std::ffi::OsString>) -> Result<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA is not set; set VQRO_STATE_DIR and VQRO_EXTENSIONS_DIR explicitly"))
}

#[cfg(windows)]
#[test]
fn windows_local_appdata_is_required() {
    assert!(local_appdata_from(None).unwrap_err().to_string().contains("LOCALAPPDATA"));
    assert!(local_appdata_from(Some("".into())).is_err());
    assert_eq!(local_appdata_from(Some(r"C:\Users\owner\AppData\Local".into())).unwrap(), PathBuf::from(r"C:\Users\owner\AppData\Local"));
}

#[cfg(unix)]
fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}
