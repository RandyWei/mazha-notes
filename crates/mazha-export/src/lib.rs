//! # mazha-export
//!
//! 导出管线：把 Markdown 源文本导出为 HTML / PDF（Typst 引擎）/ DOCX / EPUB，
//! 全部纯 Rust 实现。见 `SPEC.md` §8、§9。

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
