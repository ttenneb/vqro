use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

pub(crate) struct BoundedFile {
    pub(crate) bytes: Vec<u8>,
    pub(crate) metadata: Metadata,
}

pub(crate) fn read_bounded_regular_nofollow(path: &Path, maximum: u64) -> io::Result<BoundedFile> {
    let mut options = OpenOptions::new();
    options.read(true);
    super::configure_package_nofollow(&mut options);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > maximum
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "path is not a bounded regular file",
        ));
    }
    let capacity = usize::try_from(metadata.len().min(maximum)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "regular file grew beyond its size limit",
        ));
    }
    let after = file.metadata()?;
    if !after.file_type().is_file()
        || after.file_type().is_symlink()
        || after.len() != bytes.len() as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "regular file changed while it was read",
        ));
    }
    Ok(BoundedFile {
        bytes,
        metadata: after,
    })
}

pub(crate) struct SecurePackageStaging {
    path: PathBuf,
    backend: super::PackageStagingBackend,
}

impl SecurePackageStaging {
    pub(crate) fn create(parent: &Path) -> io::Result<Self> {
        fs::create_dir_all(parent)?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "package staging parent is not a real directory",
            ));
        }
        for _ in 0..32 {
            let mut random = [0_u8; 16];
            super::fill_secure_random(&mut random)?;
            let name = random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = parent.join(name);
            match super::create_private_package_directory(&path) {
                Ok(()) => match super::open_package_staging(&path) {
                    Ok(backend) => return Ok(Self { path, backend }),
                    Err(error) => {
                        let _ = fs::remove_dir_all(&path);
                        return Err(error);
                    }
                },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate an unpredictable package staging directory",
        ))
    }

    pub(crate) fn write_file(&self, relative: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        let components = normal_components(relative)?;
        if components.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty package path",
            ));
        }
        super::write_package_staging_file(&self.backend, &components, bytes, mode)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SecurePackageStaging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn normal_components(path: &Path) -> io::Result<Vec<std::ffi::OsString>> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => Ok(value.to_os_string()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "package output path is not relative",
            )),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root(label: &str) -> PathBuf {
        let mut random = [0_u8; 8];
        super::super::fill_secure_random(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = std::env::temp_dir().join(format!("vqro-package-fs-{label}-{suffix}"));
        fs::create_dir(&root).unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn staging_is_private_unpredictable_and_rejects_parent_symlink_substitution() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = temporary_root("staging");
        let first = SecurePackageStaging::create(&root).unwrap();
        let second = SecurePackageStaging::create(&root).unwrap();
        assert_ne!(first.path().file_name(), second.path().file_name());
        assert_eq!(
            fs::metadata(first.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let outside = root.join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, first.path().join("services")).unwrap();
        assert!(first
            .write_file(Path::new("services/main.wasm"), b"payload", 0o644)
            .is_err());
        assert!(!outside.join("main.wasm").exists());
        drop(first);
        drop(second);
        fs::remove_dir_all(root).unwrap();
    }
}
