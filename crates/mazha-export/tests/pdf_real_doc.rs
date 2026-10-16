//! R8：PDF 导出的**真实文档回归**。
//!
//! 用一份贴近实际的中文技术报告（`tests/data/real_report.md`）驱动导出，
//! 覆盖排版质量（多页）、中文字体回退与标记转义完备性。任何改动都不应让
//! 这份基线文档编译失败或体积/页数显著退化。

use std::path::PathBuf;

use mazha_export::{PdfExportOptions, PdfExporter, Template};

/// 真实文档基线（编译期内联，避免运行期读文件的不确定性）。
const DOC: &str = include_str!("data/real_report.md");

/// 资源根目录：`tests/data`，供文档中的相对路径图片 `diagram.svg` 解析。
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// 构造带资源根、扫描系统字体的导出器（与桌面端默认行为一致）。
fn exporter() -> PdfExporter {
    PdfExporter::with_options(&PdfExportOptions {
        base_dir: Some(data_dir()),
        system_fonts: true,
    })
}

#[test]
fn real_document_renders_multi_page() {
    let doc = exporter()
        .export_with(DOC, &Template::article())
        .expect("真实文档应能导出为 PDF");

    // 合法 PDF 头 + 非平凡体积。
    assert!(doc.bytes.starts_with(b"%PDF-"), "缺少 PDF 文件头");
    assert!(
        doc.bytes.len() > 5_000,
        "体积过小（{} 字节），排版可能中断",
        doc.bytes.len()
    );
    // 这份报告在 A4/11pt 下必然超过一页：验证分页确实发生。
    assert!(doc.pages >= 2, "长文档应排出多页，实际 {} 页", doc.pages);
}

#[test]
fn real_document_is_reproducible() {
    // 无 today() → 不写创建时间，同输入应逐字节一致（HTML/EPUB 同款确定性契约）。
    let a = exporter()
        .export_with(DOC, &Template::article())
        .expect("导出 A");
    let b = exporter()
        .export_with(DOC, &Template::article())
        .expect("导出 B");
    assert_eq!(a.pages, b.pages);
    assert_eq!(a.bytes, b.bytes);
}

/// 标记转义完备性单独成组：每个样本都含 Typst 结构字符，逐条独立编译，
/// 任何一条失败都能立刻定位到具体字符类别。
#[test]
fn markup_specials_never_break_compilation() {
    let samples = [
        "标题符号 # 与井号 #100 混排",
        "方括号 [内容] 与 [链接](https://example.com)",
        "反引号 `code` 与美元 $x + y$",
        "星号 *强调* 与下划线 a_b_c",
        "波浪 ~删除~ 与等号行首 = 不该成标题",
        "斜杠 a//b 与注释 /* c */ 与 @ 邮箱 x@y.z",
        "反斜杠 \\ 与尖括号 <标签> 与 & 符号",
        "中文标点：引号“双”‘单’、破折号—、省略号……",
        "emoji 😀 与组合字符 é 与全角ＡＢＣ",
    ];
    let ex = exporter();
    for s in samples {
        let doc = ex
            .export(s)
            .unwrap_or_else(|e| panic!("样本编译失败：{s}\n{e}"));
        assert!(doc.bytes.starts_with(b"%PDF-"), "样本产出非法 PDF：{s}");
    }
}
