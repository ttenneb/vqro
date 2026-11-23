//! Rex reports Unix endpoints with a `unix://` prefix; Windows reports its named pipe.
//! Only that Rex-specific spelling is normalized here; connection mechanics belong to the shared transport.
use std::io;
use std::path::{Path, PathBuf};

pub type Stream = crate::transport::ClientStream;
pub type ReadHalf = tokio::io::ReadHalf<Stream>;
pub type WriteHalf = tokio::io::WriteHalf<Stream>;

#[cfg(any(windows, test))]
fn windows_endpoint(value: &Path) -> PathBuf { value.to_path_buf() }

pub fn endpoint(value: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        match value.to_str().and_then(|s| s.strip_prefix("unix://")) {
            Some(path) => PathBuf::from(path),
            None => value.to_path_buf(),
        }
    }
    #[cfg(windows)]
    { windows_endpoint(value) }
}

pub async fn connect(value: &Path) -> io::Result<Stream> {
    crate::transport::connect(endpoint(value)).await
}

pub fn connect_sync(value: &Path) -> io::Result<crate::transport::SyncStream> {
    crate::transport::connect_sync(endpoint(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rex_endpoint_spellings() {
        #[cfg(unix)]
        assert_eq!(endpoint(Path::new("unix:///private/rex/server.sock")), Path::new("/private/rex/server.sock"));
        assert_eq!(endpoint(Path::new("/private/rex/server.sock")), Path::new("/private/rex/server.sock"));
        let pipe = Path::new(r"\\.\pipe\rex-S-1-5-21-example-suffix");
        assert_eq!(endpoint(pipe), pipe);
    }

    #[test]
    fn windows_prefixed_pipe_is_not_rewritten_into_an_accepted_endpoint() {
        let pipe = Path::new(r"\\.\pipe\rex-example");
        assert!(crate::transport::validate_windows_endpoint(&windows_endpoint(pipe)).is_ok());
        let prefixed = Path::new(r"unix://\\.\pipe\rex-example");
        assert_eq!(windows_endpoint(prefixed), prefixed);
        assert!(crate::transport::validate_windows_endpoint(&windows_endpoint(prefixed)).is_err());
        #[cfg(windows)]
        assert_eq!(endpoint(prefixed), prefixed);
    }
}
