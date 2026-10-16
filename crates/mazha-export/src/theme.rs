//! 导出**主题**（HTML / EPUB 的 CSS）与**模板**（PDF / Typst 的版式前置）。
//!
//! 两者都属"外观/版式预设"，但作用面不同：
//!
//! - [`Theme`]：一段 CSS。HTML 导出注入 `<style>`，EPUB 导出写入 `style.css`。
//!   内置 `light`（默认，等价 [`crate::DEFAULT_CSS`]）/ `dark` / `sepia`，也可 [`Theme::custom`]。
//! - [`Template`]：置于 Typst 标记**正文之前**的版式设置（`#set page(...)` /
//!   `#set text(...)` 等），供 PDF 导出使用。内置 `plain`（无前置，等价旧行为）/
//!   `article`（A4 + 页边距 + 字号 + 两端对齐），也可 [`Template::custom`]。
//!
//! 内置主题/模板均为**确定性常量**：同一输入产出逐字节一致的结果，便于测试。
//! `Template::article` 刻意**不覆盖字体族**，以免破坏 Typst 对 CJK 的字体回退。

use crate::html::DEFAULT_CSS;

/// 导出主题：一段可注入 HTML `<style>` / EPUB `style.css` 的 CSS。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// 主题名（仅用于标识/调试）。
    pub name: String,
    /// CSS 文本。
    pub css: String,
}

impl Theme {
    /// 自定义主题。
    #[must_use]
    pub fn custom(name: impl Into<String>, css: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            css: css.into(),
        }
    }

    /// 内置浅色主题（默认；与 [`crate::DEFAULT_CSS`] 相同）。
    #[must_use]
    pub fn light() -> Self {
        Self::custom("light", DEFAULT_CSS)
    }

    /// 内置深色主题（GitHub Dark 风格）。
    #[must_use]
    pub fn dark() -> Self {
        Self::custom("dark", DARK_CSS)
    }

    /// 内置护眼（羊皮纸）主题。
    #[must_use]
    pub fn sepia() -> Self {
        Self::custom("sepia", SEPIA_CSS)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::light()
    }
}

/// 深色主题 CSS。
const DARK_CSS: &str = r#"body{max-width:42em;margin:2em auto;padding:0 1em;
font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,"PingFang SC","Hiragino Sans GB","Microsoft YaHei",sans-serif;
line-height:1.6;color:#c9d1d9;background:#0d1117}
pre{background:#161b22;padding:1em;overflow:auto}
code,pre{font-family:ui-monospace,SFMono-Regular,Consolas,"Liberation Mono",monospace;font-size:.9em}
code{background:#161b22;padding:.2em .4em;border-radius:4px}
pre code{background:none;padding:0}
blockquote{margin:1em 0;padding:.5em 1em;color:#8b949e;border-left:.25em solid #30363d;background:#161b22}
img{max-width:100%}
table{border-collapse:collapse}
th,td{border:1px solid #30363d;padding:.4em .8em}
a{color:#58a6ff;text-decoration:none}
a:hover{text-decoration:underline}
"#;

/// 护眼（羊皮纸）主题 CSS。
const SEPIA_CSS: &str = r#"body{max-width:42em;margin:2em auto;padding:0 1em;
font-family:Georgia,"Songti SC","PingFang SC","Microsoft YaHei",serif;
line-height:1.7;color:#3b2f2f;background:#f8f4e8}
pre{background:#efe7d3;padding:1em;overflow:auto}
code,pre{font-family:ui-monospace,SFMono-Regular,Consolas,"Liberation Mono",monospace;font-size:.9em}
code{background:#efe7d3;padding:.2em .4em;border-radius:4px}
pre code{background:none;padding:0}
blockquote{margin:1em 0;padding:.5em 1em;color:#6b5a4a;border-left:.25em solid #cbb994;background:#f1ead6}
img{max-width:100%}
table{border-collapse:collapse}
th,td{border:1px solid #cbb994;padding:.4em .8em}
a{color:#9a6b2f;text-decoration:none}
a:hover{text-decoration:underline}
"#;

/// PDF / Typst 文档模板：置于正文之前的版式设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// 模板名（仅用于标识/调试）。
    pub name: String,
    /// Typst 版式前置（可为空；非空时会以空行与正文分隔）。
    pub preamble: String,
}

impl Template {
    /// 自定义模板。
    #[must_use]
    pub fn custom(name: impl Into<String>, preamble: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            preamble: preamble.into(),
        }
    }

    /// 无前置（等价旧行为：直接输出正文标记）。
    #[must_use]
    pub fn plain() -> Self {
        Self::custom("plain", "")
    }

    /// 内置文章模板：A4、2.5cm 页边距、11pt 字号、两端对齐、行距 0.7em。
    ///
    /// 不设置字体族，保留 Typst 默认的回退链（否则 CJK 可能缺字形）。
    #[must_use]
    pub fn article() -> Self {
        Self::custom("article", ARTICLE_PREAMBLE)
    }
}

impl Default for Template {
    fn default() -> Self {
        Self::plain()
    }
}

/// 文章模板的 Typst 版式前置。
const ARTICLE_PREAMBLE: &str = "#set page(paper: \"a4\", margin: (x: 2.5cm, y: 2.5cm))\n\
                                 #set text(size: 11pt)\n\
                                 #set par(justify: true, leading: 0.7em)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_differ_and_match_default_css() {
        assert_eq!(Theme::light().css, DEFAULT_CSS);
        assert_eq!(Theme::default(), Theme::light());
        assert_ne!(Theme::light().css, Theme::dark().css);
        assert_ne!(Theme::light().css, Theme::sepia().css);
        assert_eq!(Theme::dark().name, "dark");
    }

    #[test]
    fn custom_theme_keeps_values() {
        let t = Theme::custom("mine", "body{color:red}");
        assert_eq!(t.name, "mine");
        assert_eq!(t.css, "body{color:red}");
    }

    #[test]
    fn templates_plain_is_empty_article_is_not() {
        assert_eq!(Template::plain().preamble, "");
        assert_eq!(Template::default(), Template::plain());
        let a = Template::article();
        assert!(a.preamble.contains("#set page("));
        assert!(a.preamble.contains("#set text("));
        assert_eq!(a.name, "article");
    }

    #[test]
    fn custom_template_keeps_values() {
        let t = Template::custom("t", "#set text(size: 9pt)");
        assert_eq!(t.name, "t");
        assert_eq!(t.preamble, "#set text(size: 9pt)");
    }
}
