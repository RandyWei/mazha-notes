//! # mazha-md
//!
//! Markdown 解析与序列化：GFM + 知识库扩展（wiki-link / callout / front-matter /
//! 标签），带字节偏移的块级增量解析。见 `SPEC.md` §5、§8、§11。

/// 支持的 Markdown 方言标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// GFM + 知识库扩展（默认）。
    #[default]
    GfmKb,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dialect() {
        assert_eq!(Dialect::default(), Dialect::GfmKb);
    }
}
