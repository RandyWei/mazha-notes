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
//! - 资源（图片）按 [`PdfExportOptions::base_dir`] 解析（D82 起由桌面传当前文档目录）；
//!   无法解析的图片（缺文件、远程、data URI、绝对路径）在导出前降级为占位文字，
//!   不再让整篇编译失败，但 PDF 中不会内嵌这些图片；
//! - 编译诊断指向**生成的 Typst 标记**而非 Markdown 源，排障时需对照 `to_typst`。

use std::cmp::Reverse;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use mazha_md::{Block, SpanKind, SpanRole, parse};

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

use crate::{AssetTarget, RenderAssets, to_typst};

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
    /// 排出的页数（至少 1）。
    pub pages: usize,
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

/// 把 Markdown 图片地址解析为 Typst 资源根目录（`base_dir`）下的文件路径。
///
/// [`ExportWorld`] 只能通过 `Project` 虚拟根读取 `base_dir` 下的**相对**路径资源：
/// 远程地址、内联 data URI、`file://` 与宿主机绝对路径都无法由其 `resolve` 定位，
/// 统一返回 `None` 让调用方降级为占位文字（D82）。本函数只做路径分类/拼接，
/// **不访问文件系统**，因此可无副作用单测。
///
/// 边界：dest 首尾空白会被裁掉；`%20` 等百分号转义不在此解码（Typst 也不会解码，
/// 这类路径会由文件存在性检查判为缺失并降级）。
pub fn pdf_image_path(dest: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let dest = dest.trim();
    if dest.is_empty() {
        return None;
    }
    let lower = dest.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("mailto:")
        || lower.starts_with("file://")
        || dest.starts_with("//")
    {
        return None;
    }
    let path = Path::new(dest);
    // 绝对路径会被 Typst 当成虚拟文件系统的根路径（而非宿主机绝对路径）解析，
    // 交给 base_dir.join 也得不到正确文件；直接判为不可解析。
    if path.is_absolute() {
        return None;
    }
    Some(base_dir?.join(path))
}

/// 递归收集所有图片 Widget 的源区间（含表格/列表/引用内的行内图片）。
///
/// 只认 `SpanKind::Widget + SpanRole::ImageMarker`：代码块/公式里的 `![...](...)`
/// 字面量是 `CodeContent`/`Math` 文本，不会被误当成图片替换。
fn collect_image_ranges(blocks: &[Block], out: &mut Vec<Range<usize>>) {
    for block in blocks {
        for span in &block.spans {
            if span.kind == SpanKind::Widget && span.role == SpanRole::ImageMarker {
                out.push(span.range.clone());
            }
        }
        collect_image_ranges(&block.children, out);
    }
}

/// 转义占位文字 alt 中会被 Markdown 当成行内标记的字符。
///
/// 占位文字要走一遍 `to_typst`（会重新解析 Markdown），若 alt 里含 `*`/`[` 等，
/// 不转义就会被当成强调/链接标记，破坏生成的 Typst 标记。
fn escape_placeholder_alt(alt: &str) -> String {
    let mut out = String::with_capacity(alt.len());
    for ch in alt.chars() {
        if matches!(ch, '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '#' | '|') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 由图片 Widget 源文本生成占位文字（保留 alt 信息供读者辨认）。
fn image_placeholder(raw: &str) -> String {
    let Some((alt, _)) = crate::typst::parse_image_typst(raw) else {
        return "（图片）".to_string();
    };
    let alt = alt.trim();
    if alt.is_empty() {
        "（图片）".to_string()
    } else {
        format!("（图片：{}）", escape_placeholder_alt(alt))
    }
}

/// 把源文本中 Typst 无法加载的图片替换为占位文字，返回**导出专用**的源文本副本。
///
/// `is_available` 接收去空白后的图片地址，决定是否保留该图片；用回调注入
/// 「文件是否存在」的判断，使替换本身无 I/O、可直接单测。替换按源偏移**倒序**
/// 进行，避免先改前面片段导致后面区间失效。只改写图片 Widget，不影响编辑器
/// OffsetMap / 源文本（导出处理的是独立字符串副本）。
pub fn replace_unresolvable_images(source: &str, is_available: impl Fn(&str) -> bool) -> String {
    let blocks = parse(source);
    let mut ranges = Vec::new();
    collect_image_ranges(&blocks, &mut ranges);
    if ranges.is_empty() {
        return source.to_string();
    }
    ranges.sort_by_key(|range| Reverse(range.start));
    let mut out = source.to_string();
    for range in ranges {
        // 区间来自原始 source，倒序替换保证尚未处理的区间在两者中都有效。
        let Some(raw) = source.get(range.clone()) else {
            continue;
        };
        let Some((_, dest)) = crate::typst::parse_image_typst(raw) else {
            continue;
        };
        if is_available(dest.trim()) {
            continue;
        }
        out.replace_range(range, &image_placeholder(raw));
    }
    out
}

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

    /// 导出前把无法由当前 `base_dir` 定位的图片降级为占位文字（D82）。
    ///
    /// 文件存在性检查放在回调里，替换算法本身保持纯函数；`base_dir` 为 `None`
    /// 时所有本地图片都判为不可解析，`to_pdf` 因此不再因缺资源中断编译。
    fn sanitize_images(&self, markdown: &str) -> String {
        let base = self.base_dir.as_deref();
        replace_unresolvable_images(markdown, |dest| {
            pdf_image_path(dest, base).is_some_and(|path| path.is_file())
        })
    }

    /// 把 Markdown 导出为 PDF。
    ///
    /// # Errors
    ///
    /// 返回 [`PdfError::Compile`]（Typst 编译失败）或 [`PdfError::Export`]
    /// （PDF 序列化失败），均带格式化诊断信息。缺图/远程图片会先降级为占位文字，
    /// 不再作为编译错误返回。
    pub fn export(&self, markdown: &str) -> Result<PdfDocument, PdfError> {
        // Typst 遇到缺失资源会中断整篇编译；先在独立副本里剔除不可解析的图片。
        let markdown = self.sanitize_images(markdown);
        self.compile_typst(to_typst(&markdown))
    }

    /// 用 [`crate::Template`] 版式前置导出 PDF（页面/字号等由模板决定）。
    ///
    /// # Errors
    ///
    /// 同 [`PdfExporter::export`]。
    pub fn export_with(
        &self,
        markdown: &str,
        template: &crate::Template,
    ) -> Result<PdfDocument, PdfError> {
        // 与 `export` 同一预处理，避免带模板导出时缺图仍能令编译失败。
        let markdown = self.sanitize_images(markdown);
        self.compile_typst(crate::typst::to_typst_with_template(&markdown, template))
    }

    /// 用桌面渲染好的 SVG 资产导出 PDF（D84）。
    ///
    /// 顺序很关键：先按既有规则把**真实缺图**降级，再把公式/图表替换成虚拟图片 Widget；
    /// 若反过来，新注入的 `__mz_asset_N.svg` 会被缺图判定当成不存在的本地文件而降级。
    ///
    /// # Errors
    ///
    /// 同 [`PdfExporter::export`]；注入的 SVG 非法时 Typst 会给出编译诊断。
    pub fn export_with_assets(
        &self,
        markdown: &str,
        assets: &RenderAssets,
    ) -> Result<PdfDocument, PdfError> {
        self.export_with_assets_and_template(markdown, &crate::Template::plain(), assets)
    }

    /// 带版式模板的 [`export_with_assets`](Self::export_with_assets)。
    ///
    /// # Errors
    ///
    /// 同 [`PdfExporter::export_with_assets`]。
    pub fn export_with_assets_and_template(
        &self,
        markdown: &str,
        template: &crate::Template,
        assets: &RenderAssets,
    ) -> Result<PdfDocument, PdfError> {
        // 与 `export` 同一预处理，避免带资产导出时缺图仍能令编译失败。
        let markdown = self.sanitize_images(markdown);
        let prepared = crate::prepare_renderables(&markdown, assets, AssetTarget::VirtualFile);
        self.compile_typst_with_files(
            crate::typst::to_typst_with_template(&prepared.source, template),
            &prepared.files,
        )
    }

    /// 编译一段已经生成好的 Typst 标记为 PDF（[`export`](Self::export) 的公共内核）。
    fn compile_typst(&self, typst_source: String) -> Result<PdfDocument, PdfError> {
        self.compile_typst_with_files(typst_source, &HashMap::new())
    }

    /// 带内存虚拟资源表的编译内核（D84）。
    ///
    /// `files` 的键是 Typst 虚拟路径（如 `__mz_asset_0.svg`），值为 SVG 文本；
    /// `ExportWorld` 读文件时优先命中该表，因此无需把 SVG 写到磁盘或污染文档目录。
    fn compile_typst_with_files(
        &self,
        typst_source: String,
        files: &HashMap<String, String>,
    ) -> Result<PdfDocument, PdfError> {
        // Markdown → Typst 标记（主文件即这段标记，无真实文件系统入口）
        let source = Source::detached(typst_source);
        let main = source.id();
        let world = ExportWorld {
            exporter: self,
            main,
            source,
            assets: files,
        };

        let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);
        let document = output.map_err(|errors| PdfError::Compile(format_diagnostics(&errors)))?;
        let pages = document.pages().len();
        let bytes = typst_pdf_export(&document, &TypstPdfOptions::default())
            .map_err(|errors| PdfError::Export(format_diagnostics(&errors)))?;

        Ok(PdfDocument {
            bytes,
            warnings: warnings.iter().map(format_diagnostic).collect(),
            pages,
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

/// 一次性导出（带 [`crate::Template`] 版式模板）：Markdown → PDF。
///
/// 内部新建一个 [`PdfExporter`]，多次导出请复用 [`PdfExporter`]。
///
/// # Errors
///
/// 同 [`PdfExporter::export`]。
pub fn to_pdf_with(markdown: &str, template: &crate::Template) -> Result<PdfDocument, PdfError> {
    PdfExporter::new().export_with(markdown, template)
}

/// 一次性导出（带桌面渲染好的公式/图表 SVG，D84）。
///
/// 内部新建一个 [`PdfExporter`]（含系统字体扫描），多次导出请复用导出器。
///
/// # Errors
///
/// 同 [`PdfExporter::export_with_assets`]。
pub fn to_pdf_with_assets(markdown: &str, assets: &RenderAssets) -> Result<PdfDocument, PdfError> {
    PdfExporter::new().export_with_assets(markdown, assets)
}

/// 一次导出对应的 Typst `World`：主文件内容即 Markdown 转出的 Typst 标记，
/// 其余文件（图片等）从资源根目录读取。
struct ExportWorld<'a> {
    exporter: &'a PdfExporter,
    main: FileId,
    source: Source,
    /// D84 注入的内存 SVG 表：键为 Typst 虚拟路径（如 `__mz_asset_0.svg`）。
    /// 命中时不访问磁盘，也不要求调用方设置 `base_dir`。
    assets: &'a HashMap<String, String>,
}

impl ExportWorld<'_> {
    /// 按 Typst 请求的虚拟路径查找 D84 注入的渲染资产。
    ///
    /// 这里先于 `resolve` 命中，保证虚拟文件即使在无资源根目录时也可用；
    /// 路径统一用 `as_rootless_path` 归一化为正斜杠形式，与生成端键一致。
    fn virtual_asset(&self, id: FileId) -> Option<&str> {
        let key = id.vpath().get_without_slash();
        self.assets.get(key).map(String::as_str)
    }

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
        // D84：注入的 SVG 先于磁盘资源命中，以虚拟文件形式供 Typst `#image` 读取。
        if let Some(svg) = self.virtual_asset(id) {
            return Ok(Source::new(id, svg.to_string()));
        }
        let path = self.resolve(id)?;
        let text = fs::read_to_string(&path).map_err(|err| FileError::from_io(err, &path))?;
        Ok(Source::new(id, text))
    }

    fn file(&self, id: FileId) -> Result<Bytes, FileError> {
        if id == self.main {
            return Ok(Bytes::from_string(self.source.text().to_string()));
        }
        // D84：图片最终以字节读取，这里同样优先返回内存 SVG。
        if let Some(svg) = self.virtual_asset(id) {
            return Ok(Bytes::from_string(svg.to_string()));
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
    fn pdf_compiles_with_injected_virtual_svg() {
        // D84：无 base_dir 也能由内存虚拟文件提供 SVG，公式不再落到缺图降级。
        let mut assets = RenderAssets::new();
        assets.insert_math("x", "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"><rect width=\"8\" height=\"8\"/></svg>");
        let doc = to_pdf_with_assets("行内 $x$\n", &assets).expect("注入 SVG 应可编译");
        assert!(doc.bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn pdf_export_missing_asset_falls_back_to_placeholder() {
        // D82：无 base_dir 时图片无法解析，应降级为占位文字而不是让整篇编译失败
        let doc = to_pdf("![图](missing.png)\n").expect("缺资源应降级，不应编译失败");
        assert!(doc.bytes.starts_with(b"%PDF-"));
        assert!(doc.pages >= 1);

        // 间接验证 Typst 标记里已无 #image 引用缺失资源
        let clean = replace_unresolvable_images("![图](missing.png)\n", |_| false);
        let typst = crate::to_typst(&clean);
        assert!(!typst.contains("#image("), "{typst}");
        assert!(typst.contains("图片：图"), "{typst}");
    }

    #[test]
    fn pdf_image_path_classifies_resources_without_fs() {
        let base = Path::new("/docs/note");
        assert_eq!(
            pdf_image_path("img/a.png", Some(base)),
            Some(PathBuf::from("/docs/note/img/a.png"))
        );
        assert_eq!(
            pdf_image_path("  img/a.png  ", Some(base)),
            Some(PathBuf::from("/docs/note/img/a.png"))
        );
        // Typst `ExportWorld` 无法读取远程/内联/绝对路径与包外根，统一返回 None
        assert_eq!(pdf_image_path("/abs/a.png", Some(base)), None);
        assert_eq!(pdf_image_path("file:///abs/a.png", Some(base)), None);
        assert_eq!(pdf_image_path("https://x.test/a.png", Some(base)), None);
        assert_eq!(pdf_image_path("//host/a.png", Some(base)), None);
        assert_eq!(pdf_image_path("data:image/png;base64,xx", Some(base)), None);
        assert_eq!(pdf_image_path("mailto:a@b.c", Some(base)), None);
        // 没有 base_dir 时任何相对资源都不可解析
        assert_eq!(pdf_image_path("a.png", None), None);
        assert_eq!(pdf_image_path("   ", Some(base)), None);
    }

    #[test]
    fn replace_unresolvable_images_keeps_available_and_skips_code_literals() {
        let src = "甲![好图](ok.png)乙\n\n```\n![代码里的](bad.png)\n```\n\n丙![缺图](bad.png)丁\n";
        let out = replace_unresolvable_images(src, |dest| dest == "ok.png");
        // 可用图片原样保留（只有缺图才降级）
        assert!(out.contains("甲![好图](ok.png)乙"), "{out}");
        assert!(out.contains("丙（图片：缺图）丁"), "{out}");
        // 代码块里的相同文本不是图片 Widget，必须原样保留
        assert!(out.contains("![代码里的](bad.png)"), "{out}");
        assert_eq!(out.matches("![好图](ok.png)").count(), 1);
    }

    #[test]
    fn replace_unresolvable_images_escapes_alt_markup() {
        let out = replace_unresolvable_images("![a*b_c](bad.png)\n", |_| false);
        assert!(out.contains("（图片：a\\*b\\_c）"), "{out}");
        // 空 alt 也要有可读占位
        let empty = replace_unresolvable_images("![](bad.png)\n", |_| false);
        assert!(empty.contains("（图片）"), "{empty}");
    }

    #[test]
    fn pdf_export_mixed_existing_and_missing_images() {
        // 真实文档场景：base_dir 下的图片保留，同篇的缺图降级，整篇仍可编译
        let dir = temp_dir("mixed");
        write_test_svg(&dir, "ok.svg");
        let options = PdfExportOptions {
            base_dir: Some(dir.clone()),
            ..Default::default()
        };
        let doc = PdfExporter::with_options(&options)
            .export("![在](ok.svg)\n\n![缺](missing.svg)\n")
            .expect("缺图应降级，整篇仍可编译");
        assert!(doc.bytes.starts_with(b"%PDF-"));
        let _ = fs::remove_dir_all(&dir);
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

    #[test]
    fn pdf_export_with_article_template_compiles() {
        // 模板含中文正文，验证不覆盖字体族 → CJK 仍可回退渲染
        let doc = to_pdf_with("# 标题\n\n正文**粗体**。\n", &crate::Template::article())
            .expect("带模板导出");
        assert!(doc.bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn pdf_plain_template_matches_default() {
        // plain 模板无前置，应与不带模板的导出逐字节一致
        let a = to_pdf("# 复现\n").expect("默认导出");
        let b = to_pdf_with("# 复现\n", &crate::Template::plain()).expect("plain 模板导出");
        assert_eq!(a.bytes, b.bytes);
    }
}
