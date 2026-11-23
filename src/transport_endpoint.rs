//! Windows endpoints are names in the local pipe namespace, never remote/device/file paths.
use std::{
    fmt, io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
struct Refused(PathBuf);
impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            r"unavailable: refusing endpoint {}: Windows core endpoint must be a local named pipe: \\.\pipe\<name> (non-empty name, no further backslash)",
            self.0.display()
        )
    }
}
impl std::error::Error for Refused {}

pub(super) fn validate(endpoint: &Path) -> io::Result<()> {
    // ASCII delimiters have identical encoded bytes in Windows OsStr's self-synchronizing encoding.
    // Inspect without normalizing or rewriting the name passed to the native open.
    let bytes = endpoint.as_os_str().as_encoded_bytes();
    const PREFIX: &[u8] = br"\\.\pipe\";
    let valid = bytes
        .get(..PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
        && bytes.len() > PREFIX.len()
        && !bytes[PREFIX.len()..].contains(&b'\\')
        && !bytes.contains(&0);
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            Refused(endpoint.into()),
        ));
    }
    Ok(())
}

pub(super) fn is_refusal(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|error| error.is::<Refused>())
}

/// Every native open goes through this guard; tests use a powerless opener instead of CreateFile.
#[cfg(any(windows, test))]
pub(super) fn open<S>(endpoint: &Path, open: impl FnOnce(&Path) -> io::Result<S>) -> io::Result<S> {
    validate(endpoint)?;
    open(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn local_names_keep_original_spelling_at_the_opener() {
        for name in [r"\\.\pipe\x", r"\\.\PiPe\Some name-λ", r"\\.\PIPE\vqro-123"] {
            let calls = Cell::new(0);
            let supplied = Path::new(name);
            open(supplied, |actual| {
                calls.set(calls.get() + 1);
                assert_eq!(actual.as_os_str(), supplied.as_os_str());
                Ok(())
            })
            .unwrap();
            assert_eq!(calls.get(), 1);
        }
    }
    #[test]
    fn nonlocal_device_file_empty_nested_and_nul_names_never_reach_opener() {
        for name in [
            r"\\host\pipe\x",
            r"\\localhost\pipe\x",
            r"\\?\pipe\x",
            r"\\?\UNC\host\pipe\x",
            r"\Device\NamedPipe\x",
            r"C:\x",
            "",
            r"\\.\pipe\",
            r"\\.\pipe\x\y",
            "\\\\.\\pipe\\x\0tail",
        ] {
            let opens = Cell::new(0);
            let writes = Cell::new(0);
            let result = open(Path::new(name), |_| {
                opens.set(opens.get() + 1);
                writes.set(23);
                Ok(())
            });
            assert_eq!(opens.get(), 0, "{name:?}");
            assert_eq!(writes.get(), 0, "{name:?}");
            let error = result.unwrap_err();
            assert!(is_refusal(&error));
            assert_eq!(
                error.to_string(),
                format!(
                    r"unavailable: refusing endpoint {name}: Windows core endpoint must be a local named pipe: \\.\pipe\<name> (non-empty name, no further backslash)"
                )
            );
            assert!(crate::client::core_probe_result::<()>(Err(error)).is_err());
        }
    }
}
