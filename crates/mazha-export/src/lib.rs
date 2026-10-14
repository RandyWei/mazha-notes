//! # mazha-export
//!
//! 导出管线：把 Markdown 源文本导出为 HTML / PDF（Typst 引擎）/ DOCX / EPUB，
//! 全部纯 Rust 实现。见 `SPEC.md` §8、§9。
//!
//! - **HTML**：[`to_html`] / [`to_html_document`]，基于 `mazha-md` 的块/片段模型渲染。
//! - **PDF**：[`to_typst`] 先转成 Typst 标记，再由 [`to_pdf`] 经 Typst 编译出字节。

mod html;
mod pdf;
mod typst;

pub use html::{DEFAULT_CSS, to_html, to_html_document};
pub use pdf::{PdfDocument, PdfError, PdfExportOptions, PdfExporter, to_pdf};
pub use typst::to_typst;

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
