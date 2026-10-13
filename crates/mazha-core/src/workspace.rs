//! 工作区：把存储 Provider 与文档模型串起来（打开 / 保存 / 列目录）。
//!
//! 这是 UI（或 FFI 的 `Workspace`）使用的入口：以某个 [`StorageProvider`] 为后端，
//! `open` 读取文本并构造 [`Document`]，`save` 把源文本写回。CRDT 同步、文件监听等
//! 在后续 Phase 叠加。

use crate::Document;
use mazha_store::{DocPath, Entry, EntryKind, StorageProvider, StoreError};

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

    /// 新建一篇空文档（写入空内容并返回对应的 [`Document`]）。
    ///
    /// 存在性探测用 `read()`（`NotFound` 即不存在），因此**父目录不存在也能创建**
    /// （`save` 会自动建目录）。
    ///
    /// # Errors
    /// - 文件已存在 → [`StoreError::AlreadyExists`]；
    /// - 其它存储错误透传。
    pub fn create(&self, path: &DocPath) -> WorkspaceResult<Document> {
        match self.store.read(path) {
            Ok(_) => Err(StoreError::AlreadyExists(path.to_string()).into()),
            Err(StoreError::NotFound(_)) => {
                let doc = Document::new("");
                self.save(path, &doc)?;
                Ok(doc)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// 删除文档（或目录）。
    ///
    /// # Errors
    /// 透传 [`StoreError`]（不存在 → [`StoreError::NotFound`]）。
    pub fn remove(&self, path: &DocPath) -> WorkspaceResult<()> {
        self.store.remove(path)?;
        Ok(())
    }

    /// 重命名/移动文档。
    ///
    /// # Errors
    /// 透传 [`StoreError`]（源不存在等）。
    pub fn rename(&self, from: &DocPath, to: &DocPath) -> WorkspaceResult<()> {
        self.store.rename(from, to)?;
        Ok(())
    }

    /// 列出目录下的 **Markdown 文档**（`.md` / `.markdown`，不递归）。
    ///
    /// 文件管理界面直接用这个，无需自己过滤扩展名。
    ///
    /// # Errors
    /// 目录不存在 → [`StoreError`]。
    pub fn list_markdown(&self, dir: &DocPath) -> WorkspaceResult<Vec<Entry>> {
        Ok(self
            .store
            .list(dir)?
            .into_iter()
            .filter(|e| e.kind == EntryKind::File && e.path.is_markdown())
            .collect())
    }

    /// 在 `dir` 下按文件名查找 Markdown 文档，返回其完整路径。
    ///
    /// # Errors
    /// 目录不存在 → [`StoreError`]。
    pub fn find_markdown(&self, dir: &DocPath, name: &str) -> WorkspaceResult<Option<DocPath>> {
        Ok(self
            .list_markdown(dir)?
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.path))
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

    #[test]
    fn create_new_document_and_reject_duplicate() {
        let (_d, ws) = workspace();
        let doc = ws.create(&dp("new.md")).unwrap();
        assert_eq!(doc.text(), "");
        // 重复创建应报 AlreadyExists
        assert!(matches!(
            ws.create(&dp("new.md")),
            Err(WorkspaceError::Store(StoreError::AlreadyExists(_)))
        ));
    }

    #[test]
    fn create_makes_parent_dirs() {
        let (_d, ws) = workspace();
        ws.create(&dp("deep/nested/note.md")).unwrap();
        // 能打开说明路径正确落盘
        assert_eq!(ws.open(&dp("deep/nested/note.md")).unwrap().text(), "");
    }

    #[test]
    fn list_markdown_filters_non_markdown() {
        let (_d, ws) = workspace();
        ws.save(&dp("a.md"), &Document::new("x")).unwrap();
        ws.save(&dp("b.markdown"), &Document::new("y")).unwrap();
        ws.save(&dp("c.txt"), &Document::new("z")).unwrap();
        // 目录不被误认为文档
        ws.create(&dp("sub.md")).unwrap();
        let mds = ws.list_markdown(&DocPath::root()).unwrap();
        let names: Vec<_> = mds.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.md", "b.markdown", "sub.md"]);
        assert!(mds.iter().all(|e| e.path.is_markdown()));
    }

    #[test]
    fn find_markdown_by_name() {
        let (_d, ws) = workspace();
        ws.save(&dp("sub/a.md"), &Document::new("x")).unwrap();
        assert_eq!(
            ws.find_markdown(&dp("sub"), "a.md")
                .unwrap()
                .map(|p| p.as_str().to_string()),
            Some("sub/a.md".to_string())
        );
        assert!(ws.find_markdown(&dp("sub"), "nope.md").unwrap().is_none());
    }

    #[test]
    fn rename_and_remove() {
        let (_d, ws) = workspace();
        ws.save(&dp("a.md"), &Document::new("内容")).unwrap();
        ws.rename(&dp("a.md"), &dp("b.md")).unwrap();
        // 旧路径打不开、新路径能打开且内容保留
        assert!(ws.open(&dp("a.md")).is_err());
        assert_eq!(ws.open(&dp("b.md")).unwrap().text(), "内容");

        ws.remove(&dp("b.md")).unwrap();
        assert!(matches!(
            ws.remove(&dp("b.md")),
            Err(WorkspaceError::Store(StoreError::NotFound(_)))
        ));
    }
}
