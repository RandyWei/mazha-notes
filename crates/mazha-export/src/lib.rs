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

/// 去掉表格单元格首尾的空白片段。
///
/// 投影层为了让 WYSIWYG 表格有"单元格内边距"，会保留源码里单元格两端的空格；
/// 导出 HTML/DOCX 时这些空格只是排版噪声，需要去掉。只裁切首尾的**文本**片段，
/// 行内标记（`**`、链接括号等）与单元格中间的空格保持不变。
pub(crate) fn trim_cell_spans(
    source: &str,
    spans: &[mazha_md::SyntaxSpan],
) -> Vec<mazha_md::SyntaxSpan> {
    use mazha_md::{SpanKind, SyntaxSpan};
    let mut out: Vec<SyntaxSpan> = spans.to_vec();
    // 头部：只有最前面的文本片段可能带前导空格
    while let Some(first) = out.first_mut() {
        if first.kind != SpanKind::Text {
            break;
        }
        let text = source.get(first.range.clone()).unwrap_or("");
        let trimmed = text.trim_start();
        if trimmed.len() == text.len() {
            break;
        }
        first.range.start += text.len() - trimmed.len();
        if first.range.is_empty() {
            out.remove(0);
        } else {
            break;
        }
    }
    // 尾部同理
    while let Some(last) = out.last_mut() {
        if last.kind != SpanKind::Text {
            break;
        }
        let text = source.get(last.range.clone()).unwrap_or("");
        let trimmed = text.trim_end();
        if trimmed.len() == text.len() {
            break;
        }
        last.range.end -= text.len() - trimmed.len();
        if last.range.is_empty() {
            out.pop();
        } else {
            break;
        }
    }
    out
}

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
