//! Markdown → PDF 导出（Typst 编译管线，决策 D21）。
//!
//! 管线三步：
//! 1. [`to_typst`]：Markdown → 可编译的 Typst 标记（本 crate `typst` 模块）；
//! 2. [`PdfExporter`] 实现 Typst 的 `World`，提供标准库、字体与资源文件；
//! 3. `typst::compile` 排版出 `PagedDocument`，再由 `typst_pdf::pdf` 输出字节。
//!
//! 依赖较重（整个 Typst 编译器），只用于 PDF 导出；HTML 导出不经过本模块。
//!
//! 已知限制（MVP）：
//! - 资源（图片）按 [`PdfExportOptions::base_dir`] 解析，未提供时引用相对路径
//!   资源会编译失败；
//! - 编译诊断指向**生成的 Typst 标记**而非 Markdown 源，排障时需对照 `to_typst`。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::PathBuf;

use typst::diag::{FileError, Severity, SourceDiagnostic, Warned};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, Source, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::fonts::{self, FontStore};
use typst_layout::PagedDocument;
use typst_pdf::PdfOptions as TypstPdfOptions;
use typst_pdf::pdf as typst_pdf_export;

use crate::to_typst;

/// PDF 导出配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfExportOptions {
    /// 图片等外部资源的解析根目录。
    ///
    /// Markdown 中的相对路径资源基于该目录查找（Typst 虚拟路径 → 真实路径）。
    /// 为 [`None`] 时引用任何外部资源都会编译失败，并以诊断错误返回。
    pub base_dir: Option<PathBuf>,
    /// 构建导出器时是否扫描系统字体。
    ///
    /// 内嵌字体不含 CJK 字形；扫描系统字体后 Typst 可通过字形回退渲染中文。
    /// 关闭可换取更快的构造与完全确定的字体集合（仅适用于纯 ASCII 文档）。
    pub system_fonts: bool,
}

impl Default for PdfExportOptions {
    /// 默认配置：扫描系统字体（保证 CJK 可渲染）、不设资源根目录。
    fn default() -> Self {
        Self {
            base_dir: None,
            system_fonts: true,
        }
    }
}

/// 一次成功的 PDF 导出结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfDocument {
    /// PDF 文件字节（以 `%PDF-` 开头）。
    pub bytes: Vec<u8>,
    /// 编译期警告（字体回退、未知语言标签等），已格式化为可读文本。
    pub warnings: Vec<String>,
}

/// PDF 导出错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfError {
    /// Typst 编译失败（语法 / 求值 / 资源缺失），值为格式化后的诊断信息。
    Compile(Vec<String>),
    /// PDF 序列化阶段失败，值为格式化后的诊断信息。
    Export(Vec<String>),
}

impl Display for PdfError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let (stage, errors) = match self {
            Self::Compile(errors) => ("编译", errors),
            Self::Export(errors) => ("导出", errors),
        };
        write!(f, "PDF {stage}失败")?;
        if errors.is_empty() {
            return write!(f, "（无诊断信息）");
        }
        for error in errors {
            write!(f, "\n{error}")?;
        }
        Ok(())
    }
}

impl Error for PdfError {}

/// PDF 导出器：持有 Typst 标准库与字体集合，可跨多次导出复用。
///
/// 系统字体扫描只在构造时执行一次，因此桌面端应启动时创建一个实例并复用，
/// 避免每次导出都重扫字体。
pub struct PdfExporter {
    /// Typst 标准库（`Library::default()`，惰性哈希供 `World::library` 使用）。
    library: LazyHash<Library>,
    /// 字体存储：内嵌字体（+ 可选系统字体），按需惰性加载字形数据。
    fonts: FontStore,
    /// 资源根目录，见 [`PdfExportOptions::base_dir`]。
    base_dir: Option<PathBuf>,
}

impl PdfExporter {
    /// 用默认配置创建（内嵌字体 + 系统字体扫描，无资源根目录）。
    pub fn new() -> Self {
        Self::with_options(&PdfExportOptions::default())
    }

    /// 用指定配置创建。
    pub fn with_options(options: &PdfExportOptions) -> Self {
        let mut fonts = FontStore::new();
        // 内嵌字体（Libertinus Serif / New Computer Modern / DejaVu Sans Mono）
        // 保证任何环境都能编译，放在最前使默认字族稳定可用
        fonts.extend(fonts::embedded());
        if options.system_fonts {
            fonts.extend(fonts::system());
        }
        Self {
            library: LazyHash::new(Library::default()),
            fonts,
            base_dir: options.base_dir.clone(),
        }
    }

    /// 资源根目录（`None` 表示不解析外部资源）。
    pub fn base_dir(&self) -> Option<&PathBuf> {
        self.base_dir.as_ref()
    }

    /// 把 Markdown 导出为 PDF。
    ///
    /// # Errors
    ///
    /// 返回 [`PdfError::Compile`]（Typst 编译失败，含资源缺失）或
    /// [`PdfError::Export`]（PDF 序列化失败），均带格式化诊断信息。
    pub fn export(&self, markdown: &str) -> Result<PdfDocument, PdfError> {
        // Markdown → Typst 标记（主文件即这段标记，无真实文件系统入口）
        let source = Source::detached(to_typst(markdown));
        let main = source.id();
        let world = ExportWorld {
            exporter: self,
            main,
            source,
        };

        let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);
        let document = output.map_err(|errors| PdfError::Compile(format_diagnostics(&errors)))?;
        let bytes = typst_pdf_export(&document, &TypstPdfOptions::default())
            .map_err(|errors| PdfError::Export(format_diagnostics(&errors)))?;

        Ok(PdfDocument {
            bytes,
            warnings: warnings.iter().map(format_diagnostic).collect(),
        })
    }
}

impl Default for PdfExporter {
    fn default() -> Self {
        Self::new()
    }
}

/// 一次性导出：Markdown → PDF。
///
/// 内部新建一个 [`PdfExporter`]（含系统字体扫描），因此调用方若需多次导出，
/// 应自行持有 [`PdfExporter`] 复用。
///
/// # Errors
///
/// 同 [`PdfExporter::export`]。
pub fn to_pdf(markdown: &str) -> Result<PdfDocument, PdfError> {
    PdfExporter::new().export(markdown)
}

/// 一次导出对应的 Typst `World`：主文件内容即 Markdown 转出的 Typst 标记，
/// 其余文件（图片等）从资源根目录读取。
struct ExportWorld<'a> {
    exporter: &'a PdfExporter,
    main: FileId,
    source: Source,
}

impl ExportWorld<'_> {
    /// 把 Typst 虚拟路径解析为资源根目录下的真实路径。
    ///
    /// 未设置资源根目录、或目标位于 Typst 包（`@preview/…`，MVP 不支持）时
    /// 返回 [`FileError`]，由编译器转成诊断错误。
    fn resolve(&self, id: FileId) -> Result<PathBuf, FileError> {
        if !matches!(id.root(), VirtualRoot::Project) {
            return Err(FileError::Other(Some(
                "package imports are not supported".into(),
            )));
        }
        let base = self
            .exporter
            .base_dir
            .as_ref()
            .ok_or_else(|| FileError::NotFound(PathBuf::from(id.vpath().get_with_slash())))?;
        id.vpath().realize(base).map_err(FileError::Realize)
    }
}

impl World for ExportWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.exporter.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.exporter.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> Result<Source, FileError> {
        if id == self.main {
            return Ok(self.source.clone());
        }
        let path = self.resolve(id)?;
        let text = fs::read_to_string(&path).map_err(|err| FileError::from_io(err, &path))?;
        Ok(Source::new(id, text))
    }

    fn file(&self, id: FileId) -> Result<Bytes, FileError> {
        if id == self.main {
            return Ok(Bytes::from_string(self.source.text().to_string()));
        }
        let path = self.resolve(id)?;
        let data = fs::read(&path).map_err(|err| FileError::from_io(err, &path))?;
        Ok(Bytes::new(data))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.exporter.fonts.font(index)
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        // MVP 不提供当前日期：由此 PDF 不写入创建日期，同一输入可产出
        // 逐字节一致的 PDF；我们生成的标记也不会调用 datetime.today()。
        None
    }
}

/// 把单条 Typst 诊断格式化为可读文本：级别 + 消息 + 提示。
///
/// Typst 的 `SourceDiagnostic` 不实现 `Display`（完整渲染需要终端样式输出），
/// 这里给出足够排障的最小文本；位置信息因指向生成的 Typst 标记，MVP 不带。
fn format_diagnostic(diagnostic: &SourceDiagnostic) -> String {
    let level = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let mut text = format!("{level}: {}", diagnostic.message);
    for hint in &diagnostic.hints {
        text.push('\n');
        text.push_str("  hint: ");
        text.push_str(&hint.v);
    }
    text
}

/// 把 Typst 诊断列表格式化为可读文本。
fn format_diagnostics(errors: &[SourceDiagnostic]) -> Vec<String> {
    errors.iter().map(format_diagnostic).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全语法特性文档（ASCII + 中文），用于端到端冒烟。
    const ALL_FEATURES: &str = "\
---
title: 测试
---

# 标题一

这是**粗体**、*斜体*、~~删除~~、`代码`、[链接](https://example.com)、\
![图](pic.svg)、[[页面|别名]]、#标签。

> 引用一段话

> [!NOTE]
> 提示内容

- 甲
  - 乙
- [x] 已完成
- [ ] 未完成

| 左 | 右 |
| :-- | --: |
| 1 | 2 |

```rust
fn main() {}
```

---

尾注文字 # 与 [方括号] 等特殊字符。
";

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时钟早于 Unix 纪元")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("mazha-pdf-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    /// 在给定目录写入一个最小可用的 SVG 测试图片。
    fn write_test_svg(dir: &std::path::Path, name: &str) {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">\
                   <rect width=\"8\" height=\"8\" fill=\"#333\"/></svg>";
        fs::write(dir.join(name), svg).expect("写入测试图片");
    }

    #[test]
    fn pdf_export_basic_has_pdf_header() {
        let doc = to_pdf("# 标题\n\n正文 **粗体**。\n").expect("导出 PDF");
        assert!(doc.bytes.starts_with(b"%PDF-"), "非 PDF 字节");
        assert!(doc.bytes.len() > 1000, "PDF 过小：{}", doc.bytes.len());
    }

    #[test]
    fn pdf_export_all_features_compiles() {
        // 文档内含图片引用，必须提供真实资源根目录才能编译
        let dir = temp_dir("all");
        write_test_svg(&dir, "pic.svg");
        let options = PdfExportOptions {
            base_dir: Some(dir.clone()),
            ..Default::default()
        };
        let doc = PdfExporter::with_options(&options)
            .export(ALL_FEATURES)
            .expect("全特性文档应可编译");
        assert!(doc.bytes.starts_with(b"%PDF-"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_export_escapes_markup_in_text() {
        // 这些字符若不转义会破坏 Typst 标记（哈希表达式 / 内容块 / 引用…）
        let doc = to_pdf("价格 #100，见 [说明] 与 a_b；邮箱 x@y.z；路径 a//b。\n")
            .expect("特殊字符文本应可编译");
        assert!(doc.bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn pdf_export_image_resolved_against_base_dir() {
        let dir = temp_dir("asset");
        write_test_svg(&dir, "pic.svg");
        let options = PdfExportOptions {
            base_dir: Some(dir.clone()),
            ..Default::default()
        };
        let exporter = PdfExporter::with_options(&options);
        let doc = exporter.export("![图](pic.svg)\n").expect("带资源导出");
        assert!(doc.bytes.starts_with(b"%PDF-"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pdf_export_missing_asset_returns_error() {
        // 未提供资源根目录时，图片资源无法解析 → 编译诊断错误
        let err = to_pdf("![图](missing.png)\n").expect_err("缺资源应失败");
        match &err {
            PdfError::Compile(errors) => assert!(!errors.is_empty()),
            PdfError::Export(errors) => panic!("应为编译错误，得到导出错误：{errors:?}"),
        }
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn pdf_exporter_can_be_reused() {
        let exporter = PdfExporter::new();
        let first = exporter.export("# 一\n").expect("第一次导出");
        let second = exporter.export("# 二\n").expect("第二次导出");
        assert!(first.bytes.starts_with(b"%PDF-"));
        assert!(second.bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn pdf_options_default_scans_system_fonts() {
        let options = PdfExportOptions::default();
        assert!(options.system_fonts);
        assert!(options.base_dir.is_none());
    }

    #[test]
    fn pdf_error_display_mentions_stage() {
        let err = PdfError::Compile(vec!["消息甲".into(), "消息乙".into()]);
        let text = err.to_string();
        assert!(text.contains("编译失败"), "{text}");
        assert!(text.contains("消息甲"));
        assert!(text.contains("消息乙"));

        let empty = PdfError::Export(vec![]).to_string();
        assert!(empty.contains("导出失败"));
    }

    #[test]
    fn pdf_export_is_reproducible() {
        // today() 返回 None → PDF 不写创建日期，同一输入应逐字节一致
        let exporter = PdfExporter::new();
        let a = exporter.export("# 复现\n\n正文。\n").expect("导出 A");
        let b = exporter.export("# 复现\n\n正文。\n").expect("导出 B");
        assert_eq!(a.bytes, b.bytes);
    }
}
