//! 工作区：把存储 Provider 与文档模型串起来（打开 / 保存 / 列目录）。
//!
//! 这是 UI（或 FFI 的 `Workspace`）使用的入口：以某个 [`StorageProvider`] 为后端，
//! `open` 读取文本并构造 [`Document`]，`save` 把源文本写回。CRDT 同步、文件监听等
//! 在后续 Phase 叠加。

use crate::Document;
use mazha_store::{DocPath, Entry, StorageProvider, StoreError};

/// 工作区错误。
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// 存储层错误。
    #[error(transparent)]
    Store(#[from] StoreError),

    /// 文件内容不是合法 UTF-8。
    #[error("文件不是合法 UTF-8：{0}")]
    NotUtf8(String),
}

/// 本模块的 `Result` 别名。
pub type WorkspaceResult<T> = std::result::Result<T, WorkspaceError>;

/// 以某个存储 Provider 为后端的工作区。
#[derive(Debug, Clone)]
pub struct Workspace<S> {
    store: S,
}

impl<S: StorageProvider> Workspace<S> {
    /// 以 `store` 为后端创建空工作区。
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// 底层存储 Provider。
    pub fn store(&self) -> &S {
        &self.store
    }

    /// 列出目录条目（不递归）。
    ///
    /// # Errors
    /// 透传 [`StoreError`]。
    pub fn list(&self, dir: &DocPath) -> WorkspaceResult<Vec<Entry>> {
        Ok(self.store.list(dir)?)
    }

    /// 打开文档：读取文本并构造 [`Document`]。
    ///
    /// # Errors
    /// - 读取失败透传 [`StoreError`]；
    /// - 内容非 UTF-8 → [`WorkspaceError::NotUtf8`]。
    pub fn open(&self, path: &DocPath) -> WorkspaceResult<Document> {
        let bytes = self.store.read(path)?;
        let text =
            String::from_utf8(bytes).map_err(|_| WorkspaceError::NotUtf8(path.to_string()))?;
        Ok(Document::new(&text))
    }

    /// 保存文档：把源文本写回（自动创建父目录）。
    ///
    /// # Errors
    /// 透传 [`StoreError`]。
    pub fn save(&self, path: &DocPath, doc: &Document) -> WorkspaceResult<()> {
        self.store.write(path, doc.text().as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazha_store::FileSystemProvider;
    use tempfile::TempDir;

    fn workspace() -> (TempDir, Workspace<FileSystemProvider>) {
        let dir = TempDir::new().unwrap();
        let store = FileSystemProvider::open(dir.path()).unwrap();
        (dir, Workspace::new(store))
    }

    fn dp(s: &str) -> DocPath {
        DocPath::new(s).unwrap()
    }

    #[test]
    fn save_then_open_roundtrip() {
        let (_d, ws) = workspace();
        let doc = Document::new("# 标题\n正文");
        ws.save(&dp("a.md"), &doc).unwrap();

        let mut opened = ws.open(&dp("a.md")).unwrap();
        assert_eq!(opened.text(), "# 标题\n正文");
        // 投影可用
        assert_eq!(opened.projection().lines[0].display_text(), "标题");
    }

    #[test]
    fn open_missing_is_store_error() {
        let (_d, ws) = workspace();
        assert!(matches!(
            ws.open(&dp("nope.md")),
            Err(WorkspaceError::Store(StoreError::NotFound(_)))
        ));
    }

    #[test]
    fn list_returns_saved_files() {
        let (_d, ws) = workspace();
        ws.save(&dp("a.md"), &Document::new("x")).unwrap();
        ws.save(&dp("b.md"), &Document::new("y")).unwrap();
        let entries = ws.list(&DocPath::root()).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.md", "b.md"]);
        assert!(entries.iter().all(|e| e.path.is_markdown()));
    }
}
