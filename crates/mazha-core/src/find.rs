//! 查找与替换（纯逻辑，Phase 3 编辑器扩展）。
//!
//! 与 [`crate::commands`] / [`crate::table`] 一样是**纯函数**：只读一段源文本、返回
//! 匹配区间或新的文本，可脱离 rope/UI 单测。桌面端把结果直接转成 [`crate::Edit`]
//! 应用（替换全量时一次性 `Edit::Replace`，纳入 undo/redo）。
//!
//! 大小写不敏感匹配按**字符** `to_lowercase` 比较，覆盖 CJK 与非 ASCII（不做语言特化
//! 折叠，如德语 `ß`→`ss`、土耳其 `İ`），这对编辑器查找已足够。

use crate::edit::Edit;
use std::ops::Range;

/// 逐字符小写比较（`to_lowercase` 可能产出多个字符，这里取迭代器整体相等）。
fn char_lower_eq(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// 在 `text` 中查找 `pattern` 的所有**非重叠**匹配（源字节区间 `start..end`，按出现顺序）。
///
/// - `case_sensitive == false` 时按字符小写比较（支持 CJK / 非 ASCII）。
/// - `pattern` 为空时返回空（避免退化成逐字符匹配）。
/// - 未找到任何匹配时返回空。
pub fn find_all(text: &str, pattern: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if pattern.is_empty() {
        return out;
    }
    if case_sensitive {
        for (idx, _) in text.match_indices(pattern) {
            out.push(idx..idx + pattern.len());
        }
        return out;
    }
    // 大小写不敏感：逐字符边界扫描，比较同样字符数的一段
    let pat: Vec<char> = pattern.chars().collect();
    let pat_len = pat.len();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for i in 0..=chars.len().saturating_sub(pat_len) {
        let window = &chars[i..i + pat_len];
        if pat
            .iter()
            .zip(window.iter().map(|&(_, c)| c))
            .all(|(p, c)| char_lower_eq(*p, c))
        {
            let start = chars[i].0;
            // 窗口之后若还有字符，其起点即匹配终点；否则到文本末尾
            let end = chars.get(i + pat_len).map_or(text.len(), |&(s, _)| s);
            out.push(start..end);
        }
    }
    out
}

/// 把 `text` 中所有 `pattern` 替换为 `replacement`，返回新文本。
///
/// `pattern` 为空或无匹配时原样返回 `text`。不支持捕获组（无正则），`replacement` 为字面量。
pub fn replace_all(text: &str, pattern: &str, replacement: &str, case_sensitive: bool) -> String {
    let matches = find_all(text, pattern, case_sensitive);
    if matches.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in matches {
        out.push_str(&text[last..m.start]);
        out.push_str(replacement);
        last = m.end;
    }
    out.push_str(&text[last..]);
    out
}

/// 把全文中的 `pattern` 替换为 `replacement`，产出一个覆盖全文的 [`Edit::Replace`]。
///
/// 无匹配（或 `pattern` 为空、替换后无变化）时返回 `None`，调用方据此忽略命令。
/// 用单次全文替换：`Document` 会把它作为**一条**撤销记录。
pub fn replace_all_edit(
    text: &str,
    pattern: &str,
    replacement: &str,
    case_sensitive: bool,
) -> Option<Edit> {
    if pattern.is_empty() {
        return None;
    }
    let new_text = replace_all(text, pattern, replacement, case_sensitive);
    if new_text == text {
        return None;
    }
    Some(Edit::Replace {
        range: 0..text.len(),
        text: new_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_all_case_sensitive_matches() {
        let hits = find_all("Hello world", "o", true);
        assert_eq!(hits, vec![4..5, 7..8]);
    }

    #[test]
    fn find_all_case_insensitive_matches() {
        // 大小写不敏感：'H' 与 'h' 都命中；大小写敏感时只有 'o' 命中
        assert_eq!(find_all("Hello", "h", false), vec![0..1]);
        assert_eq!(find_all("Hello", "h", true), Vec::<Range<usize>>::new());
        assert_eq!(find_all("Hello", "HELLO", false), vec![0..5]);
        assert_eq!(find_all("Hello", "HELLO", true), Vec::<Range<usize>>::new());
    }

    #[test]
    fn find_all_non_overlapping_consecutive() {
        // "aaa" 中 "aa" 非重叠命中一次（0..2），不会重叠命中 1..2
        assert_eq!(find_all("aaaa", "aa", true), vec![0..2, 2..4]);
    }

    #[test]
    fn find_all_empty_pattern_and_no_match() {
        assert!(find_all("abc", "", true).is_empty());
        assert!(find_all("abc", "", false).is_empty());
        assert!(find_all("abc", "z", true).is_empty());
    }

    #[test]
    fn find_all_cjk_case_insensitive() {
        // 带重音拉丁：大小写不敏感按小写折叠后相等（'É'→'é'；"ÉCOLE" 为 6 字节）
        assert_eq!(find_all("ÉCOLE", "école", false), vec![0..6]);
        assert_eq!(find_all("ÉCOLE", "école", true), Vec::<Range<usize>>::new());
        // 中文无大小写：相等匹配（大小写无关，两种模式结果一致）
        assert_eq!(find_all("中文中文", "中文", true), vec![0..6, 6..12]);
        assert_eq!(find_all("中文中文", "中文", false), vec![0..6, 6..12]);
    }

    #[test]
    fn find_all_multibyte_offsets() {
        // "中文" 为 3 字节/字，"中" 在 0 与 3
        assert_eq!(find_all("中文中", "中", true), vec![0..3, 6..9]);
    }

    #[test]
    fn replace_all_basic() {
        assert_eq!(replace_all("aaa", "a", "b", true), "bbb");
        assert_eq!(replace_all("Hello world", "o", "0", true), "Hell0 w0rld");
        // 无匹配原样返回
        assert_eq!(replace_all("abc", "z", "y", true), "abc");
        // 空模式原样返回
        assert_eq!(replace_all("abc", "", "y", true), "abc");
    }

    #[test]
    fn replace_all_case_insensitive() {
        assert_eq!(replace_all("Hello", "h", "J", false), "Jello");
        assert_eq!(replace_all("Hello", "h", "J", true), "Hello");
    }

    #[test]
    fn replace_all_cjk() {
        // "中文中文" 里两个 "中" 都换成 "华" → "华文华文"
        assert_eq!(replace_all("中文中文", "中", "华", true), "华文华文");
    }

    #[test]
    fn replace_all_edit_yields_full_replace() {
        let src = "aXa";
        let edit = replace_all_edit(src, "a", "b", true).expect("应产生替换编辑");
        match edit {
            Edit::Replace { range, text } => {
                assert_eq!(range, 0..src.len());
                assert_eq!(text, "bXb");
            }
            other => panic!("应为 Edit::Replace，实际 {other:?}"),
        }
    }

    #[test]
    fn replace_all_edit_none_when_no_change() {
        assert!(replace_all_edit("abc", "z", "y", true).is_none());
        assert!(replace_all_edit("abc", "", "y", true).is_none());
        // 替换为相同内容 → 无变化 → None
        assert!(replace_all_edit("abc", "b", "b", true).is_none());
    }
}
