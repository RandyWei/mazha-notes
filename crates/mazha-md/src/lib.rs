//! # mazha-md
//!
//! Markdown 解析与序列化：GFM + 知识库扩展（wiki-link / callout / front-matter /
//! 标签），带字节偏移的块级增量解析。见 `SPEC.md` §5、§8（决策 D23）、§11。
//!
//! 解析结果是一棵 [`Block`] 森林，每个块包含若干 [`SyntaxSpan`]（可见文本 vs 语法标记），
//! 供 `mazha-core` 的 `OffsetMap` 计算显示↔源映射。本 crate **不依赖任何 GUI**，
//! 也不依赖 `mazha-core`（依赖方向为 `mazha-core → mazha-md`）。

mod block;
mod incremental;
mod inline;
mod scanner;
mod span;

pub use block::{Align, Block, BlockKind};
pub use incremental::reparse;
pub use inline::parse_inline;
pub use scanner::parse;
pub use span::{SpanKind, SpanRole, SyntaxSpan, visible_runs};

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
mod serialize;

pub use serialize::{serialize, serialize_source, to_md};
