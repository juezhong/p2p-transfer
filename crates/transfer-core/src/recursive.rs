//! Bounded, capability-confined recursive directory discovery.
//!
//! Traversal never follows directory symlinks outside a SharedRoot; all
//! paths are relative to an explicitly authorized root. Files are opened
//! through capability handles. This module prepares a manifest for future
//! atomic batch PUT/GET, not a claim that network recursion is wired up.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use crate::secure_io::{FileAccessError, SharedRoot};

pub const MAX_MANIFEST_ITEMS: usize = 100_000;
pub const MAX_MANIFEST_DEPTH: usize = 64;
pub const MAX_MANIFEST_NAME_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestEntry {
    Directory(PathBuf),
    File { relative: PathBuf, bytes: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    InvalidPath,
    TooManyEntries,
    TooDeep,
    NameTooLong,
    NonUnicodeName,
    Filesystem(FileAccessError),
}

impl From<FileAccessError> for ManifestError {
    fn from(err: FileAccessError) -> Self { Self::Filesystem(err) }
}

pub fn gather_directory(root: &SharedRoot, base: &Path) -> Result<Vec<ManifestEntry>, ManifestError> {
    if base.as_os_str().is_empty() || base.is_absolute()
        || !base.components().all(|p| matches!(p, std::path::Component::Normal(_)))
    {
        return Err(ManifestError::InvalidPath);
    }
    // Validate base as a directory through a capability handle before
    // sending any manifest data to an untrusted remote peer.
    root.list(Some(base))?;
    let mut todo = VecDeque::from([(base.to_path_buf(), 0usize)]);
    let mut found = Vec::new();
    while let Some((parent, depth)) = todo.pop_front() {
        if found.len() >= MAX_MANIFEST_ITEMS {
            return Err(ManifestError::TooManyEntries);
        }
        found.push(ManifestEntry::Directory(parent.clone()));
        let names = root.list(Some(&parent))?;
        for name in names {
            let value = name.to_str().ok_or(ManifestError::NonUnicodeName)?;
            if value.len() > MAX_MANIFEST_NAME_BYTES {
                return Err(ManifestError::NameTooLong);
            }
            let path = parent.join(value);
            if found.len() + todo.len() >= MAX_MANIFEST_ITEMS {
                return Err(ManifestError::TooManyEntries);
            }
            // Capability-safe directory open proves it can be traversed;
            // the file open path independently checks the file type.
            if root.list(Some(&path)).is_ok() {
                if depth >= MAX_MANIFEST_DEPTH {
                    return Err(ManifestError::TooDeep);
                }
                todo.push_back((path, depth + 1));
            } else if let Ok(file) = root.open_read(&path) {
                let bytes = file.metadata().map_err(|_| ManifestError::Filesystem(FileAccessError::Io))?.len();
                found.push(ManifestEntry::File { relative: path, bytes });
            } else {
                // Unreadable, symlink or special entries fail closed. Do not
                // silently lose files from a claimed complete directory.
                return Err(ManifestError::Filesystem(FileAccessError::Io));
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn recursive_manifest_preserves_unicode_empty_dirs_and_sizes() {
        let dir = std::env::temp_dir().join(format!("p2p-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("共享/子目录/空")).unwrap();
        fs::write(dir.join("共享/文件.txt"), b"abc").unwrap();
        fs::write(dir.join("共享/子目录/大.txt"), b"hello").unwrap();
        let root = SharedRoot::authorize(&dir).unwrap();
        let got = gather_directory(&root, Path::new("共享")).unwrap();
        assert!(got.contains(&ManifestEntry::Directory("共享".into())));
        assert!(got.contains(&ManifestEntry::Directory("共享/子目录/空".into())));
        assert!(got.contains(&ManifestEntry::File { relative: "共享/文件.txt".into(), bytes: 3 }));
        assert!(got.contains(&ManifestEntry::File { relative: "共享/子目录/大.txt".into(), bytes: 5 }));
        drop(root);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = std::env::temp_dir().join(format!("p2p-manifest-invalid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let root = SharedRoot::authorize(&dir).unwrap();
        for path in ["../outside", "/etc/passwd", "x/../y", "."] {
            assert_eq!(gather_directory(&root, Path::new(path)), Err(ManifestError::InvalidPath));
        }
        drop(root);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escape() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("p2p-manifest-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("共享")).unwrap();
        symlink("/etc", dir.join("共享/escape")).unwrap();
        let root = SharedRoot::authorize(&dir).unwrap();
        assert!(gather_directory(&root, Path::new("共享")).is_err());
        drop(root);
        fs::remove_dir_all(dir).unwrap();
    }
}
