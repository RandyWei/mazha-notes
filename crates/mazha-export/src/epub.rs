//! Markdown 源文本 → EPUB 3（SPEC §8、§9，决策 D27）。
//!
//! 与 HTML 导出**同源**：正文由 [`crate::html::to_xhtml_body`] 渲染（XHTML 方言——
//! 空元素自闭合、布尔属性带值），再用一个 EPUB 3 容器把它包起来，最后打成 ZIP。
//!
//! 产出结构（EPUB 3 规定）：
//!
//! ```text
//! mimetype                      # 首个条目且**不压缩**，内容固定
//! META-INF/container.xml        # 指向包文档
//! OEBPS/content.opf             # 包文档：metadata / manifest / spine
//! OEBPS/nav.xhtml               # EPUB3 导航文档（必需）
//! OEBPS/toc.ncx                 # EPUB2 兼容回退（可选）
//! OEBPS/style.css               # 复用 HTML 导出的默认 CSS
//! OEBPS/text.xhtml              # 正文（单一内容文档）
//! ```
//!
//! MVP 取舍：整篇作为**单一内容文档**，导航仅一条"全文"；`dcterms:modified` 用固定
//! 时间戳（避免引入时间依赖，保证输出确定/可测试）；`dc:identifier` 为空时由源文本
//! 内容哈希派生。

use crate::html::{DEFAULT_CSS, escape_attr, to_xhtml_body};
use std::io::{Cursor, Write as _};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipWriter};

/// EPUB 元数据（对应包文档里的 `dc:title` / `dc:creator` / `dc:language` / `dc:identifier`）。
#[derive(Debug, Clone)]
pub struct EpubMeta {
    /// 书名（`dc:title`）；空串按"未命名"处理。
    pub title: String,
    /// 作者（`dc:creator`）；空串则不写该字段。
    pub author: String,
    /// 语言（`dc:language`，如 `zh`、`en`）；空串按 `zh` 处理。
    pub language: String,
    /// 唯一标识（`dc:identifier`）；空串时由源文本内容哈希派生。
    pub identifier: String,
}

impl Default for EpubMeta {
    fn default() -> Self {
        Self {
            title: "未命名".to_string(),
            author: String::new(),
            language: "zh".to_string(),
            identifier: String::new(),
        }
    }
}

impl EpubMeta {
    /// 以标题构造，其余字段取默认值。
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }
}

/// EPUB 导出错误。
#[derive(Debug, thiserror::Error)]
pub enum EpubError {
    /// ZIP 打包失败（含底层 I/O）。
    #[error("EPUB 打包失败：{0}")]
    Zip(#[from] zip::result::ZipError),
    /// 写入字节流失败。
    #[error("EPUB 写入失败：{0}")]
    Io(#[from] std::io::Error),
}

/// 把 Markdown 源文本打包为 EPUB 3 字节流（仅指定标题）。
///
/// # Errors
///
/// 当 ZIP 打包或底层写入失败时返回 [`EpubError`]。
pub fn to_epub(source: &str, title: &str) -> Result<Vec<u8>, EpubError> {
    to_epub_with_meta(source, &EpubMeta::new(title))
}

/// 把 Markdown 源文本打包为 EPUB 3 字节流（完整元数据）。
///
/// 标题/语言/标识为空时分别回退到"未命名"、`zh`、内容哈希派生值；正文经 XHTML
/// 渲染（与 HTML 导出同源），因此标题/作者等元数据均经 XML 转义。
///
/// # Errors
///
/// 当 ZIP 打包或底层写入失败时返回 [`EpubError`]。
pub fn to_epub_with_meta(source: &str, meta: &EpubMeta) -> Result<Vec<u8>, EpubError> {
    to_epub_impl(source, meta, DEFAULT_CSS)
}

/// 用 [`crate::Theme`] 打包 EPUB 3：主题 CSS 作为 `OEBPS/style.css`。
///
/// # Errors
///
/// 当 ZIP 打包或底层写入失败时返回 [`EpubError`]。
pub fn to_epub_themed(
    source: &str,
    meta: &EpubMeta,
    theme: &crate::Theme,
) -> Result<Vec<u8>, EpubError> {
    to_epub_impl(source, meta, &theme.css)
}

/// EPUB 打包主体；`css` 决定 `OEBPS/style.css` 的内容。
fn to_epub_impl(source: &str, meta: &EpubMeta, css: &str) -> Result<Vec<u8>, EpubError> {
    // 元数据回退：空标题/空语言/空标识都有确定默认
    let title = if meta.title.trim().is_empty() {
        "未命名"
    } else {
        meta.title.as_str()
    };
    let language = if meta.language.trim().is_empty() {
        "zh"
    } else {
        meta.language.as_str()
    };
    let identifier = if meta.identifier.trim().is_empty() {
        format!("urn:mazha:{:016x}", content_hash(source))
    } else {
        meta.identifier.clone()
    };

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));

    // ZIP 条目时间戳**必须固定**：`FileOptions::default()` 取当前时间，会让同一份输入
    // 在跨 2 秒（DOS 时间粒度）时导出不同字节，破坏"相同输入 → 相同输出"的可复现性。
    // 用 `DateTime::default()`（1980-01-01，ZIP 纪元起点）保证确定性。
    let fixed_time = zip::DateTime::default();

    // 1) mimetype：必须是**第一个**条目且`Stored`（不压缩）——EPUB 规范硬性要求
    zip.start_file(
        "mimetype",
        FileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(fixed_time),
    )?;
    zip.write_all(b"application/epub+zip")?;

    // 其余条目一律 Deflate（FileOptions 是 Copy，可复用）
    let deflate = FileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(fixed_time);

    zip.start_file("META-INF/container.xml", deflate)?;
    zip.write_all(CONTAINER_XML.as_bytes())?;

    zip.start_file("OEBPS/content.opf", deflate)?;
    zip.write_all(package_opf(title, &identifier, language, &meta.author).as_bytes())?;

    zip.start_file("OEBPS/nav.xhtml", deflate)?;
    zip.write_all(nav_xhtml(title, language).as_bytes())?;

    zip.start_file("OEBPS/toc.ncx", deflate)?;
    zip.write_all(toc_ncx(title, &identifier).as_bytes())?;

    zip.start_file("OEBPS/style.css", deflate)?;
    zip.write_all(css.as_bytes())?;

    // 正文最后写入（渲染无关顺序，但语义上放末尾更直观）
    zip.start_file("OEBPS/text.xhtml", deflate)?;
    let body = to_xhtml_body(source);
    zip.write_all(content_xhtml(title, language, &body).as_bytes())?;

    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

/// `META-INF/container.xml`：无动态内容，固定字符串。
const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#;

/// 包文档 `content.opf`：metadata + manifest + spine。
///
/// `dcterms:modified` 用固定时间戳：EPUB3 要求该字段存在且为合法 `xs:dateTime`，
/// 固定值保证输出确定（不引入时间依赖）。
fn package_opf(title: &str, identifier: &str, language: &str, author: &str) -> String {
    let creator = if author.trim().is_empty() {
        String::new()
    } else {
        format!("    <dc:creator>{}</dc:creator>\n", escape_attr(author))
    };
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="pub-id" xml:lang="{lang}">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="pub-id">{id}</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:language>{lang}</dc:language>
{creator}    <meta property="dcterms:modified">2026-01-01T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="css" href="style.css" media-type="text/css"/>
    <item id="text" href="text.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx">
    <itemref idref="text"/>
  </spine>
</package>
"#,
        lang = escape_attr(language),
        id = escape_attr(identifier),
        title = escape_attr(title),
        creator = creator,
    )
}

/// EPUB3 导航文档 `nav.xhtml`（`epub:type="toc"`）。
fn nav_xhtml(title: &str, language: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="{lang}">
<head>
  <meta charset="utf-8"/>
  <title>{title}</title>
</head>
<body>
  <nav epub:type="toc" id="toc">
    <h1>{title}</h1>
    <ol>
      <li><a href="text.xhtml">全文</a></li>
    </ol>
  </nav>
</body>
</html>
"#,
        lang = escape_attr(language),
        title = escape_attr(title),
    )
}

/// EPUB2 兼容导航 `toc.ncx`。
fn toc_ncx(title: &str, identifier: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="{id}"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle><text>{title}</text></docTitle>
  <navMap>
    <navPoint id="navPoint-1" playOrder="1">
      <navLabel><text>全文</text></navLabel>
      <content src="text.xhtml"/>
    </navPoint>
  </navMap>
</ncx>
"#,
        id = escape_attr(identifier),
        title = escape_attr(title),
    )
}

/// 内容文档 `text.xhtml`：XHTML5 外壳 + 渲染好的正文。
fn content_xhtml(title: &str, language: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="{lang}">
<head>
  <meta charset="utf-8"/>
  <title>{title}</title>
  <link rel="stylesheet" type="text/css" href="style.css"/>
</head>
<body>
{body}
</body>
</html>
"#,
        lang = escape_attr(language),
        title = escape_attr(title),
        body = body,
    )
}

/// FNV-1a 64 位哈希：为缺省 `dc:identifier` 派生稳定标识，避免引入随机/`uuid` 依赖。
fn content_hash(s: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// 把导出的字节打开为 ZIP 归档，便于逐条目断言。
    fn archive(bytes: &[u8]) -> zip::ZipArchive<Cursor<&[u8]>> {
        zip::ZipArchive::new(Cursor::new(bytes)).expect("应是合法 ZIP")
    }

    /// 读取归档中某条目的文本内容。
    fn entry_text(bytes: &[u8], name: &str) -> String {
        let mut ar = archive(bytes);
        let mut f = ar.by_name(name).expect("条目应存在");
        let mut s = String::new();
        f.read_to_string(&mut s).expect("应可读为 UTF-8");
        s
    }

    #[test]
    fn output_is_zip() {
        let bytes = to_epub("# 标题\n", "测试").expect("导出应成功");
        assert_eq!(&bytes[..4], b"PK\x03\x04");
    }

    #[test]
    fn mimetype_is_first_and_stored() {
        let bytes = to_epub("正文\n", "T").expect("导出应成功");
        let mut ar = archive(&bytes);
        let f = ar.by_index(0).expect("应有首个条目");
        assert_eq!(f.name(), "mimetype");
        assert_eq!(f.compression(), CompressionMethod::Stored);
        drop(f);
        assert_eq!(entry_text(&bytes, "mimetype"), "application/epub+zip");
    }

    #[test]
    fn container_points_to_opf() {
        let bytes = to_epub("x\n", "T").expect("导出应成功");
        let c = entry_text(&bytes, "META-INF/container.xml");
        assert!(c.contains("full-path=\"OEBPS/content.opf\""));
        assert!(c.contains("application/oebps-package+xml"));
    }

    #[test]
    fn opf_has_metadata_manifest_spine() {
        let bytes = to_epub("正文\n", "我的书").expect("导出应成功");
        let opf = entry_text(&bytes, "OEBPS/content.opf");
        assert!(opf.contains("<dc:title>我的书</dc:title>"));
        assert!(opf.contains("<dc:language>zh</dc:language>"));
        assert!(opf.contains("properties=\"nav\""));
        assert!(opf.contains("<itemref idref=\"text\"/>"));
        // 无作者时不应写 dc:creator
        assert!(!opf.contains("dc:creator"));
    }

    #[test]
    fn opf_includes_author_when_present() {
        let mut meta = EpubMeta::new("书");
        meta.author = "张三".to_string();
        let bytes = to_epub_with_meta("x\n", &meta).expect("导出应成功");
        let opf = entry_text(&bytes, "OEBPS/content.opf");
        assert!(opf.contains("<dc:creator>张三</dc:creator>"));
    }

    #[test]
    fn opf_uses_explicit_identifier() {
        let mut meta = EpubMeta::new("书");
        meta.identifier = "urn:isbn:123".to_string();
        let bytes = to_epub_with_meta("x\n", &meta).expect("导出应成功");
        let opf = entry_text(&bytes, "OEBPS/content.opf");
        assert!(opf.contains("<dc:identifier id=\"pub-id\">urn:isbn:123</dc:identifier>"));
    }

    #[test]
    fn identifier_is_deterministic() {
        // 相同输入 → 相同输出（含派生标识），保证可复现/可测试
        let a = to_epub("同一份内容\n", "T").expect("导出应成功");
        let b = to_epub("同一份内容\n", "T").expect("导出应成功");
        assert_eq!(a, b);
    }

    #[test]
    fn content_is_xhtml_with_rendered_markdown() {
        let bytes = to_epub("# 标题\n\n**粗** 体\n", "T").expect("导出应成功");
        let xhtml = entry_text(&bytes, "OEBPS/text.xhtml");
        assert!(xhtml.contains("xmlns=\"http://www.w3.org/1999/xhtml\""));
        assert!(xhtml.contains("<h1>标题</h1>"));
        assert!(xhtml.contains("<strong>粗</strong>"));
        assert!(xhtml.contains("href=\"style.css\""));
    }

    #[test]
    fn empty_input_still_valid() {
        let bytes = to_epub("", "空").expect("导出应成功");
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        let xhtml = entry_text(&bytes, "OEBPS/text.xhtml");
        assert!(xhtml.contains("<body>"));
    }

    #[test]
    fn void_elements_self_close_in_content() {
        // 分割线 / 硬换行 / 图片：XHTML 里必须自闭合
        let bytes = to_epub("---\n\na  \nb\n\n![图](a.png)\n", "T").expect("导出应成功");
        let xhtml = entry_text(&bytes, "OEBPS/text.xhtml");
        assert!(xhtml.contains("<hr/>"));
        assert!(xhtml.contains("<br/>"));
        assert!(xhtml.contains("<img src=\"a.png\" alt=\"图\"/>"));
    }

    #[test]
    fn task_checkbox_is_xhtml_valid() {
        let bytes = to_epub("- [x] 完成\n- [ ] 待办\n", "T").expect("导出应成功");
        let xhtml = entry_text(&bytes, "OEBPS/text.xhtml");
        assert!(xhtml.contains("disabled=\"disabled\" checked=\"checked\"/>"));
        assert!(xhtml.contains("disabled=\"disabled\"/>"));
    }

    #[test]
    fn escapes_title_in_opf_and_content() {
        let bytes = to_epub("x\n", "A<B>&C").expect("导出应成功");
        let opf = entry_text(&bytes, "OEBPS/content.opf");
        assert!(opf.contains("<dc:title>A&lt;B&gt;&amp;C</dc:title>"));
        let xhtml = entry_text(&bytes, "OEBPS/text.xhtml");
        assert!(xhtml.contains("<title>A&lt;B&gt;&amp;C</title>"));
    }

    #[test]
    fn nav_and_ncx_present() {
        let bytes = to_epub("x\n", "书").expect("导出应成功");
        let nav = entry_text(&bytes, "OEBPS/nav.xhtml");
        assert!(nav.contains("epub:type=\"toc\""));
        assert!(nav.contains("<a href=\"text.xhtml\">全文</a>"));
        let ncx = entry_text(&bytes, "OEBPS/toc.ncx");
        assert!(ncx.contains("<content src=\"text.xhtml\"/>"));
    }

    #[test]
    fn style_css_is_default() {
        let bytes = to_epub("x\n", "T").expect("导出应成功");
        assert_eq!(entry_text(&bytes, "OEBPS/style.css"), DEFAULT_CSS);
    }

    #[test]
    fn themed_epub_uses_theme_css() {
        let theme = crate::Theme::dark();
        let bytes = to_epub_themed("x\n", &EpubMeta::new("T"), &theme).expect("导出应成功");
        assert_eq!(entry_text(&bytes, "OEBPS/style.css"), theme.css);
    }

    #[test]
    fn empty_title_falls_back_and_empty_language_defaults() {
        let meta = EpubMeta {
            title: "   ".to_string(),
            language: String::new(),
            identifier: String::new(),
            ..EpubMeta::default()
        };
        let bytes = to_epub_with_meta("x\n", &meta).expect("导出应成功");
        let opf = entry_text(&bytes, "OEBPS/content.opf");
        assert!(opf.contains("<dc:title>未命名</dc:title>"));
        assert!(opf.contains("<dc:language>zh</dc:language>"));
        assert!(opf.contains("urn:mazha:"));
    }
}
