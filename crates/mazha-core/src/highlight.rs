//! 代码块语法高亮（SPEC §11.5、§12.2）。
//!
//! 用 [`syntect`]（Sublime 语法定义 + 纯 Rust fancy-regex 引擎）把**一段代码文本**按语言
//! 拆成带前景色的片段。本模块只做"源文本 → 着色区间"的纯逻辑换算，不依赖任何 GUI；
//! UI 层（桌面/移动/Web）拿 [`ColoredRun`] 的源字节区间去覆盖自身样式即可。
//!
//! 设计要点：
//! - [`Highlighter`] 持有**内置语法集**（`SyntaxSet` 很大、加载耗时，应全局复用同一实例）；
//! - 返回的区间**相对每行行首**（不含换行符），便于 UI 对应投影行的 `source_range`；
//! - 语言名先按扩展名/词元/语法名匹配，未命中回退**纯文本**语法（不报错）。

use std::ops::Range;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// 一段带前景色的代码片段（区间相对其所在行行首）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColoredRun {
    /// 该片段在**本行文本**中的字节范围（不含换行）。
    pub range: Range<usize>,
    /// 前景色 `(r, g, b)`。
    pub color: (u8, u8, u8),
}

/// 语法高亮器：持有语法集与主题，可跨多块代码复用。
pub struct Highlighter {
    /// 内置语法集（加载一次，全部代码块共用）。
    set: SyntaxSet,
    /// 当前主题。
    theme: Theme,
}

impl Highlighter {
    /// 用**内置主题**加载默认语法集。
    ///
    /// `theme` 是内置主题名（如 `"base16-ocean.dark"`、`"InspiredGitHub"`、`"Solarized (dark)"`）；
    /// 未知名回退到内置主题集的任意一个（保证非空）。`SyntaxSet` 较大，调用方应复用实例。
    ///
    /// # Panics
    /// 内置主题集为空时 panic（正常安装下不会发生：syntect 自带多个内置主题）。
    pub fn with_theme(theme: &str) -> Self {
        let set = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get(theme)
            .cloned()
            .or_else(|| themes.themes.values().next().cloned())
            .expect("内置主题集非空");
        Self { set, theme }
    }

    /// 语言名（扩展名/词元/语法名）→ 内置语法；未命中回退纯文本。
    fn find_syntax(&self, lang: Option<&str>) -> &SyntaxReference {
        let l = lang.unwrap_or("").trim();
        self.set
            .find_syntax_by_extension(l)
            .or_else(|| self.set.find_syntax_by_token(l))
            .or_else(|| self.set.find_syntax_by_name(l))
            .unwrap_or_else(|| self.set.find_syntax_plain_text())
    }

    /// 对整段代码逐行着色。
    ///
    /// 返回值按行分组，与 `code` 的逻辑行一一对应（**不含**结尾空行）；每行的片段按出现
    /// 顺序排列、区间互不重叠。语言未命中时整行用纯文本（单色）。
    pub fn highlight(&self, code: &str, lang: Option<&str>) -> Vec<Vec<ColoredRun>> {
        let syntax = self.find_syntax(lang);
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(code) {
            // 逐行着色（`highlight_line` 内部跨行维护状态）
            let ranges = h.highlight_line(line, &self.set).unwrap_or_default();
            let mut runs = Vec::new();
            let mut off = 0usize;
            for (style, text) in ranges {
                // 该切片的完整长度（含可能的行尾换行）；可见部分去掉行尾 \r\n
                let full_len = text.len();
                let visible_len = text.trim_end_matches(['\r', '\n']).len();
                if visible_len > 0 {
                    runs.push(ColoredRun {
                        range: off..off + visible_len,
                        color: (style.foreground.r, style.foreground.g, style.foreground.b),
                    });
                }
                // 偏移始终按完整长度推进，保证同片内后续切片对齐
                off += full_len;
            }
            out.push(runs);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言每行的着色区间**连续且恰好覆盖**该行可见文本（不含换行）。
    fn assert_covers(code: &str, hl: &[Vec<ColoredRun>]) {
        let lines: Vec<&str> = code.split('\n').collect();
        assert_eq!(hl.len(), lines.len(), "行数应与代码一致");
        for (runs, line) in hl.iter().zip(lines) {
            // 去掉行尾 \r（LinesWithEndings 不处理 \r\n，这里统一按可见长度比较）
            let content_len = line.trim_end_matches('\r').len();
            let mut off = 0usize;
            for run in runs {
                assert_eq!(run.range.start, off, "着色区间应连续");
                off = run.range.end;
            }
            assert_eq!(off, content_len, "着色区间应覆盖整行");
        }
    }

    #[test]
    fn rust_keyword_gets_distinct_color() {
        let h = Highlighter::with_theme("base16-ocean.dark");
        let code = "fn main() {\n    let x = 1;\n}";
        let hl = h.highlight(code, Some("rust"));
        assert_covers(code, &hl);
        // 第一行至少有两种颜色（关键字 vs 其余）
        assert!(
            hl[0].len() >= 2,
            "rust 首行应至少拆出两段着色，实际 {:?}",
            hl[0]
        );
        let colors: std::collections::HashSet<_> = hl[0].iter().map(|r| r.color).collect();
        assert!(colors.len() >= 2, "首行应有不同前景色");
    }

    #[test]
    fn unknown_language_falls_back_to_plain_text() {
        let h = Highlighter::with_theme("base16-ocean.dark");
        let code = "hello world\nsecond";
        let hl = h.highlight(code, Some("definitely_not_a_lang_xyz"));
        assert_covers(code, &hl);
        // 纯文本：每行最多一段、且只有一种颜色
        assert!(hl[0].len() <= 1 && hl[1].len() <= 1);
        let c = hl[0][0].color;
        assert!(hl[1].iter().all(|r| r.color == c), "纯文本各行颜色一致");
    }

    #[test]
    fn empty_and_multiline_state_are_handled() {
        let h = Highlighter::with_theme("base16-ocean.dark");
        // 空代码：不产出行
        assert!(h.highlight("", Some("rust")).is_empty());
        // 多行字符串/注释跨行：着色区间仍需覆盖每行
        let code = "/* comment\n   still comment */\nlet a = 1;";
        let hl = h.highlight(code, Some("rust"));
        assert_covers(code, &hl);
        assert_eq!(hl.len(), 3);
    }

    #[test]
    fn no_language_is_plain_text() {
        let h = Highlighter::with_theme("base16-ocean.dark");
        let code = "const x = 1;";
        let hl = h.highlight(code, None);
        assert_covers(code, &hl);
        assert!(hl[0].iter().all(|r| r.color == hl[0][0].color));
    }

    #[test]
    fn ranges_are_relative_to_line_start() {
        let h = Highlighter::with_theme("base16-ocean.dark");
        let code = "fn a() {}\nfn b() {}";
        let hl = h.highlight(code, Some("rust"));
        // 每行首个片段都从 0 开始
        assert_eq!(hl[0][0].range.start, 0);
        assert_eq!(hl[1][0].range.start, 0);
    }
}
