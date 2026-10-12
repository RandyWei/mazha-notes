//! # mazha-export
//!
//! 导出管线：把 Markdown 源文本导出为 HTML / PDF（Typst 引擎）/ DOCX / EPUB，
//! 全部纯 Rust 实现。见 `SPEC.md` §8、§9。
//!
//! 当前已实现 **HTML**（[`to_html`]），基于 `mazha-md` 的块/片段模型渲染。

mod html;

pub use html::to_html;

/// 支持的导出目标格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Html,
    Pdf,
    Docx,
    Epub,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_distinct() {
        assert_ne!(ExportFormat::Html, ExportFormat::Pdf);
    }
}
