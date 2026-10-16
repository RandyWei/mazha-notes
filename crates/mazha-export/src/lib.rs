//! # mazha-export
//!
//! 导出管线：把 Markdown 源文本导出为 HTML / PDF（Typst 引擎）/ DOCX / EPUB，
//! 全部纯 Rust 实现。见 `SPEC.md` §8、§9。
//!
//! - **HTML**：[`to_html`] / [`to_html_document`]，基于 `mazha-md` 的块/片段模型渲染。
//! - **PDF**：[`to_typst`] 先转成 Typst 标记，再由 [`to_pdf`] 经 Typst 编译出字节。
//! - **DOCX**：[`to_docx`]，基于 `docx-rs` 生成 OOXML。
//! - **EPUB**：[`to_epub`] / [`to_epub_with_meta`]，复用 HTML 渲染的 XHTML 方言打包为 EPUB 3。
//! - **主题/模板**：[`Theme`]（HTML/EPUB 的 CSS）与 [`Template`]（PDF/Typst 的版式前置）。

mod docx;
mod epub;
mod html;
mod pdf;
mod theme;
mod typst;

pub use docx::{DocxError, to_docx};
pub use epub::{EpubError, EpubMeta, to_epub, to_epub_themed, to_epub_with_meta};
pub use html::{DEFAULT_CSS, to_html, to_html_document, to_html_themed};
pub use pdf::{PdfDocument, PdfError, PdfExportOptions, PdfExporter, to_pdf, to_pdf_with};
pub use theme::{Template, Theme};
pub use typst::{to_typst, to_typst_with_template};

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
