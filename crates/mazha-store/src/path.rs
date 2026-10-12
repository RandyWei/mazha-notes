//! 工作区内的文档路径（`DocPath`，SPEC §6.1）。
//!
//! 统一使用 `/` 分隔、相对工作区根、**不可**包含 `..`/`.`/空段/绝对前缀/反斜杠。
//! 规范化保证 Provider 之间路径语义一致（本地文件、云端、插件）。

use crate::error::{StoreError, StoreResult};

/// 工作区内的相对路径。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocPath(String);

impl DocPath {
    /// 工作区根路径（内部为空串，仅用于列根目录）。
    pub fn root() -> Self {
        Self(String::new())
    }

    /// 是否为工作区根。
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// 构造并校验路径。
    ///
    /// # Errors
    /// 空串 → [`StoreError::EmptyPath`]；含绝对前缀、`..`/`.`/空段或反斜杠 →
    /// [`StoreError::InvalidPath`]。
    pub fn new(path: impl Into<String>) -> StoreResult<Self> {
        let path = path.into();
        if path.is_empty() {
            return Err(StoreError::EmptyPath);
        }
        if path.starts_with('/') || path.contains('\\') {
            return Err(StoreError::InvalidPath(path));
        }
        for seg in path.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." {
                return Err(StoreError::InvalidPath(path));
            }
        }
        Ok(Self(path))
    }

    /// 字符串视图（根路径返回空串）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 文件名（最后一段；根路径为空串）。
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// 父目录；顶层文件返回根，根本身返回 `None`。
    pub fn parent(&self) -> Option<DocPath> {
        if self.is_root() {
            return None;
        }
        Some(match self.0.rsplit_once('/') {
            Some((p, _)) => DocPath(p.to_string()),
            None => DocPath::root(),
        })
    }

    /// 以本路径为目录拼接子项。
    ///
    /// # Errors
    /// 子名为空或含分隔符时返回 [`StoreError::InvalidPath`]。
    pub fn join(&self, name: &str) -> StoreResult<DocPath> {
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return Err(StoreError::InvalidPath(name.to_string()));
        }
        if self.is_root() {
            return DocPath::new(name);
        }
        DocPath::new(format!("{}/{}", self.0, name))
    }

    /// 是否为 Markdown 文档（`.md` / `.markdown`，不区分大小写）。
    pub fn is_markdown(&self) -> bool {
        match std::path::Path::new(self.as_str())
            .extension()
            .and_then(|e| e.to_str())
        {
            Some(ext) => ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"),
            None => false,
        }
    }
}

impl std::fmt::Display for DocPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_relative_paths() {
        assert_eq!(DocPath::new("notes/a.md").unwrap().as_str(), "notes/a.md");
        assert!(DocPath::new("a.md").unwrap().is_markdown());
        assert!(!DocPath::new("a.txt").unwrap().is_markdown());
    }

    #[test]
    fn rejects_illegal_paths() {
        assert!(matches!(DocPath::new(""), Err(StoreError::EmptyPath)));
        assert!(DocPath::new("/abs").is_err());
        assert!(DocPath::new("a/../b").is_err());
        assert!(DocPath::new("a//b").is_err());
        assert!(DocPath::new("a\\b").is_err());
        assert!(DocPath::new("./a").is_err());
    }

    #[test]
    fn parent_and_file_name() {
        let p = DocPath::new("notes/sub/a.md").unwrap();
        assert_eq!(p.file_name(), "a.md");
        assert_eq!(p.parent().unwrap().as_str(), "notes/sub");
        // 顶层文件的父是根；根本身没有父
        let top = DocPath::new("a.md").unwrap();
        assert!(top.parent().unwrap().is_root());
        assert!(DocPath::root().parent().is_none());
    }

    #[test]
    fn root_join_and_validate() {
        assert!(DocPath::root().is_root());
        assert_eq!(DocPath::root().join("a.md").unwrap().as_str(), "a.md");
        let dir = DocPath::new("notes").unwrap();
        assert_eq!(dir.join("a.md").unwrap().as_str(), "notes/a.md");
        assert!(dir.join("bad/name").is_err());
        assert!(dir.join("").is_err());
    }
}
