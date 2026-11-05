//! Unix device facts for explicit client provenance and ownership publication.
//! No terminal data is read or written by these observers.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use crate::pty_ownership::TtyIdentity;

pub(crate) fn tty_identity_from_path(path: &Path) -> Option<TtyIdentity> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    tty_identity_from_file(&file)
}

pub(crate) fn tty_identity_from_file(file: &std::fs::File) -> Option<TtyIdentity> {
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_char_device() {
        return None;
    }
    // SAFETY: isatty only queries this owned, open descriptor.
    if unsafe { libc::isatty(file.as_raw_fd()) } != 1 {
        return None;
    }
    Some(TtyIdentity {
        device: meta.dev(),
        inode: meta.ino(),
        rdev: meta.rdev(),
    })
}

pub(crate) fn validate_private_ownership_path(path: &Path, directory: bool) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no memory preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ownership path is not private, owned and nonsymlinked",
        ));
    }
    Ok(())
}

pub(crate) fn private_ownership_file_options(options: &mut OpenOptions) {
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
}
