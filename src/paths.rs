//! Where prim keeps its socket and state. Environment overrides first, then per-user defaults.

use std::path::PathBuf;

use anyhow::{bail, Result};

/// `PRIM_STATE_DIR`, else `$XDG_STATE_HOME/prim`, else `~/.local/state/prim`.
pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("PRIM_STATE_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(d).join("prim");
    }
    home().join(".local/state/prim")
}

/// Bundled extensions: `PRIM_EXTENSIONS_DIR`, else `~/.local/share/prim/extensions`.
pub fn extensions_dir() -> PathBuf {
    match std::env::var("PRIM_EXTENSIONS_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => home().join(".local/share/prim/extensions"),
    }
}

/// `PRIM_SOCKET`, else `<state_dir>/prim.sock`.
pub fn socket_path() -> Result<PathBuf> {
    // A terminal-injected endpoint is ambient reachability, not an explicit scratch-run choice.
    let inherited = std::env::var("PRIM_SOCKET_SOURCE").ok().as_deref()==Some("terminal") && std::env::var_os("PRIM_STATE_DIR").is_some();
    let p = match std::env::var("PRIM_SOCKET").ok().filter(|_|!inherited) {
        Some(s) => PathBuf::from(s),
        None => state_dir().join("prim.sock"),
    };
    // macOS limits Unix socket paths to 103 bytes, Linux to 107.
    if p.as_os_str().len() > 100 {
        bail!("socket path is too long ({} bytes): {}; set PRIM_SOCKET to a shorter path", p.as_os_str().len(), p.display());
    }
    Ok(p)
}

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."))
}
