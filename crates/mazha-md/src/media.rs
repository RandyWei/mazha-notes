//! HTML 媒体标签的轻量解析（D64）。
//!
//! 只识别**单行** `<video>` / `<audio>`：属性区与结束标签都不能跨行，跨行时退回普通
//! 文本，避免吞掉后续段落。支持的形态：
//!
//! - `<video src="..."></video>`、`<audio src='...'></audio>`
//! - `<video src="..." />`（自闭合）
//! - `<video controls><source src="..."></video>`（取第一个带 `src` 的 `<source>`）
//!
//! `poster` / `title` 原样提取；属性顺序与单双引号都不敏感。

/// 媒体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// 视频。
    Video,
    /// 音频。
    Audio,
}

impl MediaKind {
    /// 标签名（`video` / `audio`）。
    pub fn tag(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    /// 中文占位名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "视频",
            Self::Audio => "音频",
        }
    }
}

/// 解析出的媒体元素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaElement {
    /// 媒体类型。
    pub kind: MediaKind,
    /// 媒体地址（已做基础 HTML 实体解码）。
    pub src: String,
    /// 视频封面地址。
    pub poster: Option<String>,
    /// 标题（用于控制条/音频卡片）。
    pub title: Option<String>,
}

/// 解析一个**完整**媒体元素（允许首尾空白）。
pub fn parse_media_element(text: &str) -> Option<MediaElement> {
    let text = text.trim();
    let (consumed, element) = parse_media_element_at(text, 0)?;
    (consumed == text.len()).then_some(element)
}

/// 从 `source[start..]` 解析媒体元素；返回 `(消耗字节数, 元素)`。
///
/// 起始位置必须是 `<`。未闭合、跨行、缺少地址或标签名不匹配时返回 `None`。
pub fn parse_media_element_at(source: &str, start: usize) -> Option<(usize, MediaElement)> {
    let rest = source.get(start..)?;
    if !rest.starts_with('<') {
        return None;
    }
    let open_end = find_tag_end(rest, 0)?;
    let open_inner = &rest[1..open_end];
    let tag = open_inner.trim_start();
    if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
        return None;
    }
    let self_closing = tag.trim_end().ends_with('/');
    let tag = if self_closing {
        tag.trim_end().strip_suffix('/')?.trim_end()
    } else {
        tag
    };
    let (name, attrs) = split_tag_name(tag)?;
    let kind = match name.to_ascii_lowercase().as_str() {
        "video" => MediaKind::Video,
        "audio" => MediaKind::Audio,
        _ => return None,
    };
    let mut src = attr_value(attrs, "src").filter(|s| !s.is_empty());
    let poster = attr_value(attrs, "poster").filter(|s| !s.is_empty());
    let title = attr_value(attrs, "title").filter(|s| !s.is_empty());
    if self_closing {
        return Some((
            open_end + 1,
            MediaElement {
                kind,
                src: src?,
                poster,
                title,
            },
        ));
    }
    let (close_start, close_end) = find_closing_tag(rest, open_end + 1, kind.tag())?;
    let inner = &rest[open_end + 1..close_start];
    if inner.contains('\n') || inner.contains('\r') {
        return None;
    }
    if src.is_none() {
        src = find_source_src(inner);
    }
    Some((
        close_end,
        MediaElement {
            kind,
            src: src?,
            poster,
            title,
        },
    ))
}

/// 找 `from` 之后、引号外的 `>`；遇到换行返回 `None`。
fn find_tag_end(s: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (off, ch) in s[from..].char_indices() {
        if ch == '\n' || ch == '\r' {
            return None;
        }
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '>' => return Some(from + off),
                _ => {}
            },
        }
    }
    None
}

/// 从起始标签里拆出标签名与属性区。
fn split_tag_name(tag: &str) -> Option<(&str, &str)> {
    let start = tag
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)?;
    let rest = &tag[start..];
    let end = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || *c == '/')
        .map_or(rest.len(), |(i, _)| i);
    (end > 0).then(|| (&rest[..end], &rest[end..]))
}

/// 查找结束标签；内容必须与起始标签在同一行。
fn find_closing_tag(s: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let mut i = from;
    while i < s.len() {
        let rel = s[i..].find('<')?;
        let start = i + rel;
        if s[start + 1..].starts_with('/') {
            let end = find_tag_end(s, start)?;
            let close = s[start + 2..end].trim();
            if close
                .split_whitespace()
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
            {
                return Some((start, end + 1));
            }
            i = end + 1;
        } else {
            let end = find_tag_end(s, start)?;
            i = end + 1;
        }
    }
    None
}

/// 在 `<video>` / `<audio>` 的子内容里找第一个带 `src` 的 `<source>`。
fn find_source_src(inner: &str) -> Option<String> {
    let mut i = 0usize;
    while i < inner.len() {
        let rel = inner[i..].find('<')?;
        let start = i + rel;
        let end = find_tag_end(inner, start)?;
        let tag = &inner[start + 1..end];
        let trimmed = tag.trim_start();
        if !trimmed.starts_with('/') && !trimmed.starts_with('!') {
            let without_slash = trimmed
                .trim_end()
                .strip_suffix('/')
                .unwrap_or(trimmed)
                .trim_end();
            if let Some((name, attrs)) = split_tag_name(without_slash)
                && name.eq_ignore_ascii_case("source")
                && let Some(src) = attr_value(attrs, "src").filter(|s| !s.is_empty())
            {
                return Some(src);
            }
        }
        i = end + 1;
    }
    None
}

/// 取属性值（大小写不敏感；值可单/双引号或裸写）。
fn attr_value(attrs: &str, wanted: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let name = &attrs[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                ""
            } else if matches!(bytes[i], b'"' | b'\'') {
                let quote = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let value = &attrs[start..i];
                if i < bytes.len() {
                    i += 1;
                }
                value
            } else {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                &attrs[start..i]
            }
        } else {
            ""
        };
        if name.eq_ignore_ascii_case(wanted) {
            return Some(decode_entities(value));
        }
    }
    None
}

/// 基础 HTML 实体解码（只处理 URL/标题里常见的几个）。
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_with_attrs() {
        let e = parse_media_element(
            r#"<video controls poster="cover.jpg" src="https://x.test/a.mp4" title="演示"></video>"#,
        )
        .unwrap();
        assert_eq!(e.kind, MediaKind::Video);
        assert_eq!(e.src, "https://x.test/a.mp4");
        assert_eq!(e.poster.as_deref(), Some("cover.jpg"));
        assert_eq!(e.title.as_deref(), Some("演示"));
    }

    #[test]
    fn audio_single_quotes_and_self_closing() {
        assert_eq!(
            parse_media_element("<audio src='a.mp3'></audio>")
                .unwrap()
                .kind,
            MediaKind::Audio
        );
        assert_eq!(
            parse_media_element("<video src=a.mp4 />").unwrap().src,
            "a.mp4"
        );
    }

    #[test]
    fn source_child_supplies_src() {
        let e =
            parse_media_element(r#"<video controls><source src="a.mp4" type="video/mp4"></video>"#)
                .unwrap();
        assert_eq!(e.src, "a.mp4");
    }

    #[test]
    fn multiline_is_rejected() {
        assert!(parse_media_element("<video src=\"a.mp4\">\n</video>").is_none());
    }

    #[test]
    fn missing_src_or_close_is_rejected() {
        assert!(parse_media_element("<video controls></video>").is_none());
        assert!(parse_media_element("<video src=\"a.mp4\">").is_none());
    }

    #[test]
    fn entities_in_url() {
        assert_eq!(
            parse_media_element("<video src=\"a.mp4?x=1&amp;y=2\"></video>")
                .unwrap()
                .src,
            "a.mp4?x=1&y=2"
        );
    }
}
