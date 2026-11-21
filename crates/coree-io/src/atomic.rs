// SPDX-License-Identifier: MPL-2.0

//! Crash-safe atomic file persistence (`NamedTempFile` + `sync_all` + atomic `persist`).

use crate::error::{IoError, IoResult};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

/// Atomically writes `bytes` to `path` via a temporary file in the same parent directory,
/// flushing and calling `sync_all()` before atomically persisting into place.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> IoResult<()> {
    atomic_write_with(path, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

/// Atomically writes to `path` using a caller-supplied closure writing into a freshly
/// created temporary file in `path`'s parent directory, followed by `flush()`, `sync_all()`,
/// and atomic rename via `persist(path)`.
pub fn atomic_write_with<F>(path: &Path, writer: F) -> IoResult<()>
where
    F: FnOnce(&mut File) -> IoResult<()>,
{
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
    }

    let mut tmp = NamedTempFile::new_in(parent)?;
    writer(tmp.as_file_mut())?;
    tmp.as_file_mut().flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| IoError::Io(err.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn atomic_write_bytes_creates_and_replaces_safely() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("nested").join("output.bin");

        atomic_write_bytes(&target, b"first version").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"first version");

        atomic_write_bytes(&target, b"second version").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second version");
    }

    #[test]
    fn atomic_write_with_preserves_original_on_writer_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("doc.coree");
        atomic_write_bytes(&target, b"intact").unwrap();

        let res = atomic_write_with(&target, |file| {
            file.write_all(b"partial")?;
            Err(IoError::InvalidFormat {
                format: "test".into(),
                message: "simulated failure".into(),
            })
        });
        assert!(res.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"intact");
    }
}
