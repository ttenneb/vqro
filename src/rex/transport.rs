//! Rex reports Unix endpoints with a `unix://` prefix; Windows reports its named pipe.
//! Only that Rex-specific spelling is normalized here; connection mechanics belong to the shared transport.
use std::io;
use std::path::{Path, PathBuf};

pub type Stream = crate::transport::ClientStream;
pub type ReadHalf = tokio::io::ReadHalf<Stream>;
pub type WriteHalf = tokio::io::WriteHalf<Stream>;

pub fn endpoint(value: &Path) -> PathBuf {
    match value.to_str().and_then(|s| s.strip_prefix("unix://")) {
        Some(path) => PathBuf::from(path),
        None => value.to_path_buf(),
    }
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
        assert_eq!(endpoint(Path::new("unix:///private/rex/server.sock")), Path::new("/private/rex/server.sock"));
        assert_eq!(endpoint(Path::new("/private/rex/server.sock")), Path::new("/private/rex/server.sock"));
        let pipe = Path::new(r"\\.\pipe\rex-S-1-5-21-example-suffix");
        assert_eq!(endpoint(pipe), pipe);
    }
}
