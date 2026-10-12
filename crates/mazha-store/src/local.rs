//! 本地文件系统 Provider（默认实现，SPEC §6.2）。

use crate::error::{StoreError, StoreResult};
use crate::path::DocPath;
use crate::provider::{Entry, EntryKind, StorageCaps, StorageProvider};
use std::fs;
use std::path::{Path, PathBuf};

/// 以某个目录为工作区根的本地文件系统 Provider。
#[derive(Debug, Clone)]
pub struct FileSystemProvider {
    root: PathBuf,
}

impl FileSystemProvider {
    /// 以 `root` 为工作区根打开 Provider（不存在则创建）。
    ///
    /// # Errors
    /// 创建目录失败 → [`StoreError::Io`]。
    pub fn open(root: impl Into<PathBuf>) -> StoreResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// 工作区根路径。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 把相对 [`DocPath`] 解析为绝对路径。
    fn resolve(&self, path: &DocPath) -> PathBuf {
        // DocPath 已校验：无绝对前缀、无 `..`，`/` 在 Windows 上 Path 亦接受。
        self.root.join(path.as_str())
    }
}

impl StorageProvider for FileSystemProvider {
    fn id(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> StorageCaps {
        StorageCaps {
            writable: true,
            streaming: false,
            versioning: false,
            collaboration: false,
        }
    }

    fn list(&self, dir: &DocPath) -> StoreResult<Vec<Entry>> {
        let abs = self.resolve(dir);
        if !abs.is_dir() {
            return Err(StoreError::NotDirectory(dir.to_string()));
        }
        let mut entries = Vec::new();
        for item in fs::read_dir(&abs).map_err(|e| StoreError::from_io(e, dir))? {
            let item = item.map_err(|e| StoreError::from_io(e, dir))?;
            let name = item.file_name().to_string_lossy().into_owned();
            let meta = item.metadata().map_err(|e| StoreError::from_io(e, dir))?;
            let kind = if meta.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let path = dir.join(&name)?;
            entries.push(Entry {
                path,
                name,
                kind,
                size: meta.len(),
                modified: meta.modified().ok(),
            });
        }
        // 目录在前，其次按名称排序，保证跨平台稳定
        entries.sort_by(|a, b| {
            let key = |e: &Entry| (e.kind == EntryKind::File, e.name.clone());
            key(a).cmp(&key(b))
        });
        Ok(entries)
    }

    fn read(&self, path: &DocPath) -> StoreResult<Vec<u8>> {
        fs::read(self.resolve(path)).map_err(|e| StoreError::from_io(e, path))
    }

    fn write(&self, path: &DocPath, data: &[u8]) -> StoreResult<()> {
        let abs = self.resolve(path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&abs, data).map_err(|e| StoreError::from_io(e, path))
    }

    fn rename(&self, from: &DocPath, to: &DocPath) -> StoreResult<()> {
        let abs_to = self.resolve(to);
        if let Some(parent) = abs_to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(self.resolve(from), &abs_to).map_err(|e| StoreError::from_io(e, from))
    }

    fn remove(&self, path: &DocPath) -> StoreResult<()> {
        let abs = self.resolve(path);
        let meta = fs::metadata(&abs).map_err(|e| StoreError::from_io(e, path))?;
        if meta.is_dir() {
            fs::remove_dir_all(&abs).map_err(|e| StoreError::from_io(e, path))
        } else {
            fs::remove_file(&abs).map_err(|e| StoreError::from_io(e, path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn provider() -> (TempDir, FileSystemProvider) {
        let dir = TempDir::new().unwrap();
        let p = FileSystemProvider::open(dir.path()).unwrap();
        (dir, p)
    }

    fn dp(s: &str) -> DocPath {
        DocPath::new(s).unwrap()
    }

    #[test]
    fn write_read_roundtrip() {
        let (_d, p) = provider();
        p.write(&dp("a.md"), "你好".as_bytes()).unwrap();
        assert_eq!(p.read(&dp("a.md")).unwrap(), "你好".as_bytes());
    }

    #[test]
    fn write_creates_parent_dirs() {
        let (_d, p) = provider();
        p.write(&dp("notes/sub/a.md"), b"x").unwrap();
        assert!(p.read(&dp("notes/sub/a.md")).is_ok());
    }

    #[test]
    fn list_top_level_sorts_dirs_first_then_name() {
        let (_d, p) = provider();
        p.write(&dp("b.md"), b"bb").unwrap();
        p.write(&dp("a.md"), b"a").unwrap();
        p.write(&dp("dir/x.md"), b"").unwrap();
        let entries = p.list(&DocPath::root()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["dir", "a.md", "b.md"]);
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert_eq!(entries[1].size, 1);
    }

    #[test]
    fn list_subdir() {
        let (_d, p) = provider();
        p.write(&dp("dir/x.md"), b"").unwrap();
        let entries = p.list(&dp("dir")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "x.md");
        assert_eq!(entries[0].path.as_str(), "dir/x.md");
    }

    #[test]
    fn list_missing_dir_errors() {
        let (_d, p) = provider();
        assert!(matches!(
            p.list(&dp("nope")),
            Err(StoreError::NotDirectory(_))
        ));
    }

    #[test]
    fn rename_and_remove() {
        let (_d, p) = provider();
        p.write(&dp("a.md"), b"x").unwrap();
        p.rename(&dp("a.md"), &dp("sub/b.md")).unwrap();
        assert!(p.read(&dp("a.md")).is_err());
        assert_eq!(p.read(&dp("sub/b.md")).unwrap(), b"x");
        p.remove(&dp("sub/b.md")).unwrap();
        assert!(p.read(&dp("sub/b.md")).is_err());
    }

    #[test]
    fn remove_dir_recursive() {
        let (_d, p) = provider();
        p.write(&dp("dir/a.md"), b"x").unwrap();
        p.write(&dp("dir/sub/b.md"), b"y").unwrap();
        p.remove(&dp("dir")).unwrap();
        assert!(p.list(&dp("dir")).is_err());
    }

    #[test]
    fn missing_file_is_not_found() {
        let (_d, p) = provider();
        assert!(matches!(
            p.read(&dp("nope.md")),
            Err(StoreError::NotFound(_))
        ));
    }
}
