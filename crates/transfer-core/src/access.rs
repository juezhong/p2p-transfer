//! File read access scoped to a user-authorized directory root.
//!
//! This is a preflight path resolution guard, not an atomic openat-based
//! sandbox. Do not treat it as sufficient against local symlink races;
//! writes and security-sensitive reads must use descriptor-relative opens.

use std::{fs, io, path::{Component, Path, PathBuf}};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessError {
    InvalidPath,
    OutsideRoot,
    Symlink,
    Missing,
    Io,
}

#[derive(Clone, Debug)]
pub struct AuthorizedRoot {
    canonical_root: PathBuf,
}

impl AuthorizedRoot {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, AccessError> {
        let root = root.as_ref();
        let meta = fs::symlink_metadata(root).map_err(map_io)?;
        if meta.file_type().is_symlink() {
            return Err(AccessError::Symlink);
        }
        if !meta.is_dir() {
            return Err(AccessError::InvalidPath);
        }
        let canonical_root = root.canonicalize().map_err(map_io)?;
        Ok(Self { canonical_root })
    }

    pub fn path(&self) -> &Path {
        &self.canonical_root
    }

    /// Read-only preflight; no path escapes, absolute paths, parent components
    /// or symbolic links are accepted. Do not use for creating or writing files.
    pub fn resolve_existing_read(&self, relative: impl AsRef<Path>) -> Result<PathBuf, AccessError> {
        let relative = relative.as_ref();
        if relative.as_os_str().is_empty() || relative.is_absolute()
            || relative.components().any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(AccessError::InvalidPath);
        }
        let mut path = self.canonical_root.clone();
        for component in relative.components() {
            path.push(component.as_os_str());
            let metadata = fs::symlink_metadata(&path).map_err(map_io)?;
            if metadata.file_type().is_symlink() {
                return Err(AccessError::Symlink);
            }
        }
        let canonical = path.canonicalize().map_err(map_io)?;
        if !canonical.starts_with(&self.canonical_root) {
            return Err(AccessError::OutsideRoot);
        }
        Ok(canonical)
    }

    /// Root listing is a separate explicitly authorized operation.
    pub fn list_root(&self) -> io::Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.canonical_root)? {
            let entry = entry?;
            // Returned entries are only names for display, not access grants.
            entries.push(entry.path());
        }
        entries.sort();
        Ok(entries)
    }
}

fn map_io(error: io::Error) -> AccessError {
    match error.kind() {
        io::ErrorKind::NotFound => AccessError::Missing,
        _ => AccessError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, AuthorizedRoot) {
        let root = std::env::temp_dir().join(format!(
            "p2p-transfer-perms-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        fs::create_dir_all(root.join("hello")).unwrap();
        fs::write(root.join("hello").join("日本語.txt"), b"test").unwrap();
        let auth = AuthorizedRoot::new(&root).unwrap();
        (root, auth)
    }

    fn rand_suffix() -> u64 {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        u64::from_le_bytes(bytes)
    }

    #[test]
    fn accepts_unicode_paths_and_root_listing() {
        let (root, auth) = fixture();
        assert!(auth.resolve_existing_read("hello/日本語.txt").unwrap().is_file());
        assert_eq!(auth.list_root().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn denies_absolute_traversal_and_nonexistent_paths() {
        let (root, auth) = fixture();
        assert_eq!(auth.resolve_existing_read("../hello"), Err(AccessError::InvalidPath));
        assert_eq!(auth.resolve_existing_read("/etc/passwd"), Err(AccessError::InvalidPath));
        assert_eq!(auth.resolve_existing_read("hello/../hello/日本語.txt"), Err(AccessError::InvalidPath));
        assert_eq!(auth.resolve_existing_read("hello/missing"), Err(AccessError::Missing));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn denies_symlinks_even_when_pointing_inside_root() {
        use std::os::unix::fs::symlink;
        let (root, auth) = fixture();
        symlink(root.join("hello"), root.join("linked")).unwrap();
        assert_eq!(auth.resolve_existing_read("linked/日本語.txt"), Err(AccessError::Symlink));
        fs::remove_dir_all(root).unwrap();
    }
}
