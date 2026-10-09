//! Root-confined file I/O based on *directory handles*, not path preflight.
//!
//! cap-std uses descriptor-relative operations to prevent traversal escaping
//! the explicitly authorized root, including symlink race attacks. The
//! caller must still decide WHICH root and remote peer are authorized.
//! The receiver writes a fresh random part file, validates SHA-256, fsyncs,
//! and creates a final hard link with atomic no-clobber semantics. A crash
//! before directory metadata is synced may leave incomplete metadata on
//! some filesystems; full durable-commit handling remains future work.

use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::{Component, Path},
};

use cap_std::{ambient_authority, fs::{Dir, File, OpenOptions}};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileAccessError {
    InvalidRelativePath,
    IsNotFile,
    ChecksumMismatch,
    WrongFileLength,
    Io,
    Entropy,
}

fn clean_relative(path: &Path) -> Result<(), FileAccessError> {
    if path.as_os_str().is_empty() || path.is_absolute()
        || !path.components().all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(FileAccessError::InvalidRelativePath);
    }
    Ok(())
}

fn io_error(_: io::Error) -> FileAccessError {
    FileAccessError::Io
}

pub struct SharedRoot {
    root: Dir,
}

impl SharedRoot {
    /// The user explicitly authorizes one local root directory. Do not
    /// obtain this from an untrusted remote command or invitation.
    pub fn authorize(root: &Path) -> Result<Self, FileAccessError> {
        let root = Dir::open_ambient_dir(root, ambient_authority()).map_err(io_error)?;
        Ok(Self { root })
    }

    /// Return an *already opened handle*, not a path that must later be
    /// reopened outside the sandbox.
    pub fn open_read(&self, relative: &Path) -> Result<File, FileAccessError> {
        clean_relative(relative)?;
        let file = self.root.open(relative).map_err(io_error)?;
        if !file.metadata().map_err(io_error)?.is_file() {
            return Err(FileAccessError::IsNotFile);
        }
        Ok(file)
    }

    /// List directory contents. Names are informational and do not grant
    /// remote callers access outside this root.
    pub fn list(&self, relative: Option<&Path>) -> Result<Vec<OsString>, FileAccessError> {
        let dir = match relative {
            None => self.root.open_dir(".").map_err(io_error)?,
            Some(path) => {
                clean_relative(path)?;
                self.root.open_dir(path).map_err(io_error)?
            }
        };
        let mut names = Vec::new();
        for entry in dir.entries().map_err(io_error)? {
            names.push(entry.map_err(io_error)?.file_name());
        }
        names.sort();
        Ok(names)
    }

    /// Enumerate entry names and file types without following symlinks.
    /// Reject unsupported/symlink entries rather than pretending a directory
    /// tree is complete when it is not.
    pub fn list_entry_kinds(
        &self,
        relative: Option<&Path>,
    ) -> Result<Vec<(OsString, bool)>, FileAccessError> {
        let dir = match relative {
            None => self.root.open_dir(".").map_err(io_error)?,
            Some(path) => {
                clean_relative(path)?;
                self.root.open_dir(path).map_err(io_error)?
            }
        };
        let mut entries = Vec::new();
        for entry in dir.entries().map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let ty = entry.file_type().map_err(io_error)?;
            if !ty.is_dir() && !ty.is_file() {
                return Err(FileAccessError::Io);
            }
            entries.push((entry.file_name(), ty.is_dir()));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }

    /// Create a destination directory beneath the explicitly authorized root.
    /// A remote request must first be authorized by the session's file
    /// sharing policy; this cannot be used to create outside the root.
    pub fn create_directory(&self, relative: &Path) -> Result<(), FileAccessError> {
        clean_relative(relative)?;
        self.root.create_dir_all(relative).map_err(io_error)
    }

    /// Start writing one target file, but do not create/replace its final
    /// name until expected length and SHA-256 have both been verified.
    pub fn receive_part(
        &self,
        relative: &Path,
        total_bytes: u64,
    ) -> Result<PartWriter, FileAccessError> {
        clean_relative(relative)?;
        let name = relative.file_name().ok_or(FileAccessError::InvalidRelativePath)?.to_os_string();
        let parent = relative.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let dir = self.root.open_dir(parent).map_err(io_error)?;

        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| FileAccessError::Entropy)?;
        let mut part = OsString::from(".p2p-");
        for byte in random {
            part.push(format!("{byte:02x}"));
        }
        part.push(".part");
        let mut flags = OpenOptions::new();
        flags.write(true).create_new(true);
        let file = dir.open_with(Path::new(&part), &flags).map_err(io_error)?;
        Ok(PartWriter {
            dir,
            file: Some(file),
            part,
            name,
            hash: Sha256::new(),
            written: 0,
            total_bytes,
            committed: false,
        })
    }
}

pub struct PartWriter {
    dir: Dir,
    file: Option<File>,
    part: OsString,
    name: OsString,
    hash: Sha256,
    written: u64,
    total_bytes: u64,
    committed: bool,
}

impl PartWriter {
    pub fn written_bytes(&self) -> u64 {
        self.written
    }

    /// ACK may advance only AFTER `append` successfully writes bytes into
    /// the destination file handle. It is not equivalent to crash durability.
    pub fn append(&mut self, block: &[u8]) -> Result<u64, FileAccessError> {
        let next = self.written.checked_add(block.len() as u64)
            .ok_or(FileAccessError::WrongFileLength)?;
        if next > self.total_bytes {
            return Err(FileAccessError::WrongFileLength);
        }
        let file = self.file.as_mut().ok_or(FileAccessError::Io)?;
        file.write_all(block).map_err(io_error)?;
        self.hash.update(block);
        self.written = next;
        Ok(self.written)
    }

    /// Fail-closed no-clobber commit: final hard_link fails if destination
    /// already exists. The file and its final name remain inside the same
    /// capability root and filesystem.
    pub fn verify_and_commit(mut self, expected_sha256: [u8; 32]) -> Result<(), FileAccessError> {
        if self.written != self.total_bytes {
            return Err(FileAccessError::WrongFileLength);
        }
        let observed: [u8; 32] = self.hash.clone().finalize().into();
        if observed != expected_sha256 {
            return Err(FileAccessError::ChecksumMismatch);
        }
        let file = self.file.as_mut().ok_or(FileAccessError::Io)?;
        file.flush().map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        drop(self.file.take());
        // Hard-link creation is atomic and does NOT replace existing
        // destination, unlike cross-platform rename() semantics.
        self.dir.hard_link(Path::new(&self.part), &self.dir, Path::new(&self.name))
            .map_err(io_error)?;
        self.committed = true;
        self.dir.remove_file(Path::new(&self.part)).map_err(io_error)?;
        Ok(())
    }

    /// Cancel without publishing the destination. Drop also attempts
    /// cleanup to avoid leaving partial files after ordinary errors.
    pub fn cancel(self) {}
}

impl Drop for PartWriter {
    fn drop(&mut self) {
        if !self.committed {
            drop(self.file.take());
            let _ = self.dir.remove_file(Path::new(&self.part));
        }
    }
}

/// Hash an already-authorized handle with bounded fixed-size memory, never
/// by reopening an unchecked filesystem path.
pub fn hash_open_file(mut file: File) -> Result<[u8; 32], FileAccessError> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(io_error)?;
        if n == 0 { break; }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (std::path::PathBuf, SharedRoot) {
        let mut bits = [0u8; 8];
        getrandom::fill(&mut bits).unwrap();
        let location = std::env::temp_dir().join(format!(
            "p2p-cap-{}-{}", std::process::id(), u64::from_le_bytes(bits)
        ));
        std::fs::create_dir_all(location.join("子目录")).unwrap();
        let root = SharedRoot::authorize(&location).unwrap();
        (location, root)
    }

    #[test]
    fn streams_file_and_commits_only_after_hash_check() {
        let (location, root) = fixture();
        let payload = "Hello, 文件 ✅".as_bytes();
        let expected: [u8; 32] = Sha256::digest(payload).into();
        let mut sink = root.receive_part(Path::new("子目录/名前.txt"), payload.len() as u64).unwrap();
        sink.append(&payload[..3]).unwrap();
        assert_eq!(sink.append(&payload[3..]).unwrap(), payload.len() as u64);
        assert!(!location.join("子目录/名前.txt").exists());
        sink.verify_and_commit(expected).unwrap();
        let opened = root.open_read(Path::new("子目录/名前.txt")).unwrap();
        assert_eq!(hash_open_file(opened).unwrap(), expected);
        assert!(location.join("子目录/名前.txt").exists());
        drop(root);
        std::fs::remove_dir_all(location).unwrap();
    }

    #[test]
    fn refuses_overrun_bad_hash_and_path_traversal() {
        let (location, root) = fixture();
        for path in ["../outside", "/etc/passwd", "子目录/../outside"] {
            assert!(matches!(
                root.receive_part(Path::new(path), 2),
                Err(FileAccessError::InvalidRelativePath)
            ));
        }
        let mut sink = root.receive_part(Path::new("check.bin"), 2).unwrap();
        assert_eq!(sink.append(b"abc"), Err(FileAccessError::WrongFileLength));
        sink.append(b"ab").unwrap();
        assert_eq!(sink.verify_and_commit([0; 32]), Err(FileAccessError::ChecksumMismatch));
        assert!(!location.join("check.bin").exists());
        drop(root);
        std::fs::remove_dir_all(location).unwrap();
    }

    #[test]
    fn lists_file_types_without_symlink_following() {
        let (location, root) = fixture();
        std::fs::write(location.join("plain.txt"), b"file").unwrap();
        let listed = root.list_entry_kinds(None).unwrap();
        assert!(listed.contains(&(OsString::from("plain.txt"), false)));
        assert!(listed.contains(&(OsString::from("子目录"), true)));
        drop(root);
        std::fs::remove_dir_all(location).unwrap();
    }

    #[test]
    fn creates_nested_directory_but_rejects_outside_authorized_root() {
        let (location, root) = fixture();
        root.create_directory(Path::new("目录/嵌套/空")).unwrap();
        assert!(location.join("目录/嵌套/空").is_dir());
        for bad in ["../outside", "/etc", "目录/../outside"] {
            assert_eq!(
                root.create_directory(Path::new(bad)),
                Err(FileAccessError::InvalidRelativePath)
            );
        }
        drop(root);
        std::fs::remove_dir_all(location).unwrap();
    }

    #[test]
    fn never_overwrites_an_existing_file() {
        let (location, root) = fixture();
        std::fs::write(location.join("existing.bin"), b"original").unwrap();
        let hash: [u8; 32] = Sha256::digest(b"replacement").into();
        let mut sink = root.receive_part(Path::new("existing.bin"), 11).unwrap();
        sink.append(b"replacement").unwrap();
        assert_eq!(sink.verify_and_commit(hash), Err(FileAccessError::Io));
        assert_eq!(std::fs::read(location.join("existing.bin")).unwrap(), b"original");
        drop(root);
        std::fs::remove_dir_all(location).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cannot_open_a_symlink_that_leads_outside_authorized_root() {
        use std::os::unix::fs::symlink;
        let (location, root) = fixture();
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let outside = std::env::temp_dir().join(format!(
            "outside-p2p-{}-{}", std::process::id(), u64::from_le_bytes(bytes)
        ));
        std::fs::write(&outside, b"secret").unwrap();
        symlink(&outside, location.join("escape")).unwrap();
        assert!(root.open_read(Path::new("escape")).is_err());
        std::fs::remove_file(outside).unwrap();
        drop(root);
        std::fs::remove_dir_all(location).unwrap();
    }
}
