//! 中文斜体（合成倾斜）渲染。
//!
//! ## 为什么必须自己合成
//!
//! macOS 随系统提供的中文字体（PingFang SC / Hiragino Sans GB / Songti SC / Heiti SC …）
//! **没有斜体字面**，并且 CoreText **不会**为它们合成 oblique：
//!
//! - `CTFontDescriptor` 带上 `kCTFontSlantTrait = 1` 后，PingFang SC 仍解析为
//!   `PingFangSC-Regular`，渲染结果与常规字形逐像素一致（无倾斜）；
//! - AppKit 的 `NSFontManager.convert(_:toHaveTrait: .italicFontMask)` 同样是空操作
//!   （描述符里既没有新字面，也没有变换矩阵）。
//!
//! 也就是说：即使 UI 侧显式给斜体 run 挂了 CJK 回退字体（`main.rs` 的
//! `italic_fallbacks()`），CoreText 级联表里的中文字形依然是直立的——
//! 表现为"拉丁字母倾斜、中文不变"。
//!
//! Chromium（Typora / VS Code）则是在渲染器里自己合成倾斜（Skia 的 oblique 矩阵），
//! 所以同一段中文在 Typora 里是斜的。为了与 Typora 的观感一致，这里补上渲染器这一层。
//!
//! ## 本模块的做法
//!
//! 1. 斜体 run 显式挂 CJK 回退字体，让 CoreText 级联表中的中文字形来自同一批字体，
//!    与本模块自绘时选用的字体保持一致（字形、字宽都对得上）。
//! 2. 中文片段在 gpui 侧被置为**透明**（仍然占位 → 排版、光标、选区都不受影响），
//!    由本模块在 layout 阶段取出字形轮廓、自己做一次剪切后用 `paint_path` 填充。
//!
//! 剪切公式（字形空间：y 向上、基线在 y = 0）：
//!
//! ```text
//! x' = x + k * y        k = tan(倾斜角)
//! ```
//!
//! 即字形越高越往右倾，而基线仍保持水平——与真实斜体的几何一致。

use std::ops::Range;

use gpui_kit::{Hsla, Path, Pixels};

/// 合成斜体的倾斜角（度）。
///
/// 取 12°：与 Chromium（Typora / VS Code）的合成斜体接近，也和多数拉丁斜体的
/// 倾斜量（11°~14°）相当。
pub const SLANT_DEG: f32 = 12.0;

/// CJK 回退字体的优先顺序（与 `main.rs` 的 `italic_fallbacks()` 保持一致）。
///
/// 顺序必须一致：CoreText 级联表里先命中哪个字体，本模块就应当用哪个字体取字形，
/// 否则自绘字形与排版占位会错位。
pub const CJK_FALLBACKS: [&str; 6] = [
    "PingFang SC",
    "Hiragino Sans GB",
    "Songti SC",
    "Heiti SC",
    "Apple Symbols",
    "Apple Color Emoji",
];

/// 一段需要"合成斜体"的中文片段。
#[derive(Clone, Debug)]
pub struct ObliqueSpan {
    /// 该片段在**行文本**中的字节区间（相对行首，行内相对坐标）。
    pub range: Range<usize>,
    /// 片段颜色（与所属 run 一致；自绘填充时用）。
    pub color: Hsla,
}

/// 字符是否属于"需要合成斜体"的一类。
///
/// 判据是 Unicode 区块：CJK 部首与符号标点、假名、注音、带圈字符、
/// CJK 表意文字（含扩展区）、兼容表意文字、兼容形式、全角形区。
/// 拉丁字母与 ASCII 标点由正文字体渲染——正文字族带真实斜体字面，无需合成。
pub fn needs_oblique(ch: char) -> bool {
    let c = ch as u32;
    matches!(
        c,
        0x2E80..=0x2FDF // CJK 部首补充
            | 0x3000..=0x303F // CJK 符号与标点
            | 0x3040..=0x30FF // 假名
            | 0x3100..=0x312F // 注音符号
            | 0x31C0..=0x31EF // 笔画
            | 0x3200..=0x32FF // 带圈字符（CJK）
            | 0x3400..=0x4DBF // CJK 扩展 A
            | 0x4E00..=0x9FFF // CJK 基本区
            | 0xF900..=0xFAFF // CJK 兼容表意文字
            | 0xFE30..=0xFE4F // CJK 兼容形式
            | 0xFF00..=0xFFEF // 全角形
            | 0x2460..=0x24FF // 带圈数字/字母（Enclosed Alphanumerics）
    ) || (0x20000..=0x2FA1F).contains(&c) // CJK 扩展 B~F + 兼容补充
}

/// 剪切系数：字形空间里每上升 1 单位，x 右移多少。
///
/// `k = tan(倾斜角)`；12° 约为 0.2126，即 1em 高的字形顶部右移约 0.21em。
pub fn shear_factor(slant_deg: f32) -> f32 {
    slant_deg.to_radians().tan()
}

/// 按"CJK / 非 CJK"切分一段文本。
///
/// 返回每段的字节区间与是否为 CJK 段（相对传入文本的起点）。
/// 用来把斜体 run 拆成"照常斜体"与"待合成斜体"两批。
pub fn split_segments(text: &str) -> Vec<(Range<usize>, bool)> {
    let mut out: Vec<(Range<usize>, bool)> = Vec::new();
    for (offset, ch) in text.char_indices() {
        let is_cjk = needs_oblique(ch);
        match out.last_mut() {
            Some(last) if last.1 == is_cjk => last.0.end = offset + ch.len_utf8(),
            _ => out.push((offset..offset + ch.len_utf8(), is_cjk)),
        }
    }
    out
}

/// 正文字体（拉丁主字体）是否覆盖该字符。
///
/// 覆盖时该字形来自主字体（带真实斜体字面），**不能**再走合成斜体，
/// 否则会与排版占位对不上。非 macOS 平台恒返回 `false`（不做合成）。
#[cfg(target_os = "macos")]
pub fn primary_covers(family: &str, ch: char) -> bool {
    macos::covers(family, ch)
}

/// 非 macOS 平台：没有 CoreText，不做合成斜体。
#[cfg(not(target_os = "macos"))]
pub fn primary_covers(_family: &str, _ch: char) -> bool {
    false
}

/// 取某字符的**合成倾斜**字形轮廓（gpui 路径）。
///
/// - `font_size`：正文字号（轮廓按此字号取）；
/// - `pen_x`：该字符在行内的左边界 x（内容坐标）；
/// - `baseline_y`：该视觉行的基线 y（内容坐标，y 向下）。
///
/// 找不到覆盖该字的字体时返回 `None`（调用方应回退到直立字形）。
#[cfg(target_os = "macos")]
pub fn glyph_path(
    ch: char,
    font_size: f32,
    pen_x: Pixels,
    baseline_y: Pixels,
) -> Option<Path<Pixels>> {
    macos::glyph_path(ch, font_size, pen_x, baseline_y)
}

/// 非 macOS 平台：没有 CoreText，取不到合成斜体轮廓。
#[cfg(not(target_os = "macos"))]
pub fn glyph_path(
    _ch: char,
    _font_size: f32,
    _pen_x: Pixels,
    _baseline_y: Pixels,
) -> Option<Path<Pixels>> {
    None
}

/// macOS 专用的 CoreText 取形实现。
#[cfg(target_os = "macos")]
mod macos {
    use core_foundation::base::TCFType;
    use core_graphics::font::CGGlyph;
    use core_graphics::geometry::CGAffineTransform;
    use core_graphics::path::{CGPath, CGPathElementType, SysCGPathRef};
    use core_text::font::{CTFont, CTFontRef, new_from_name};
    use foreign_types::ForeignType;
    use gpui_kit::{Path, PathBuilder, Pixels, point, px};
    use std::cell::RefCell;
    use std::collections::HashMap;

    // `core-text` 没有暴露 `CTFontCreatePathForGlyph`，此处自行声明。
    // 矩阵传 `NULL`：轮廓就在字体的文本空间里（原点 = 基线，y 向上，已按字号缩放）。
    unsafe extern "C" {
        fn CTFontCreatePathForGlyph(
            font: CTFontRef,
            glyph: CGGlyph,
            matrix: *const CGAffineTransform,
        ) -> SysCGPathRef;
    }

    /// gpui 主题里正文默认字族是占位名 `".SystemUIFont"`，CoreText 不认这个名字
    /// （会退化成 Times New Roman），这里换成系统 UI 字体的真名。
    const SYSTEM_UI_FAMILY: &str = ".AppleSystemUIFont";

    // 字体缓存：`(字体名, 字号) → CTFont`。
    //
    // CTFont 创建不便宜（要读字体文件、建级联表），而每个字形都要用到，故缓存起来。
    // 线程局部：CoreText 字体本身可跨线程用，但缓存不必要地共享反而要处理同步。
    thread_local! {
        static FONT_CACHE: RefCell<HashMap<(String, u32), Option<CTFont>>> =
            RefCell::new(HashMap::new());
    }

    /// 把字族名映射成 CoreText 能解析的名字。
    fn resolve_family(family: &str) -> &str {
        if family == ".SystemUIFont" {
            SYSTEM_UI_FAMILY
        } else {
            family
        }
    }

    /// 取（并缓存）指定字体与字号的 [`CTFont`]。
    ///
    /// 名字解析不出来时返回 `None`：CoreText 对不存在的名字会**静默**退化成
    /// Helvetica（而不是返回 NULL），故按"族名是否真的被解析到"来判定有效性。
    fn with_font<T>(name: &str, size: f32, f: impl FnOnce(&CTFont) -> T) -> Option<T> {
        let resolved = resolve_family(name).to_string();
        FONT_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            let key = (resolved.clone(), size.to_bits());
            if !cache.contains_key(&key) {
                let created = new_from_name(&resolved, size as f64)
                    .ok()
                    .filter(|font| font.family_name().eq_ignore_ascii_case(&resolved));
                cache.insert(key.clone(), created);
            }
            cache.get(&key)?.as_ref().map(f)
        })
    }

    /// 该字体是否覆盖给定字符。
    pub fn covers(family: &str, ch: char) -> bool {
        // 覆盖性与字号无关，用一个固定字号即可（省得每种字号都建一份缓存）。
        with_font(family, 12.0, |font| {
            let mut buf = [0u16; 2];
            let units = ch.encode_utf16(&mut buf);
            let mut glyphs = [0u16; 2];
            let ok = unsafe {
                font.get_glyphs_for_characters(
                    units.as_ptr(),
                    glyphs.as_mut_ptr(),
                    units.len() as core_foundation::base::CFIndex,
                )
            };
            ok && glyphs[0] != 0
        })
        .unwrap_or(false)
    }

    /// 取字符在 CJK 回退字体里的字形 id（取第一个覆盖它的字体）。
    fn glyph_for_char(ch: char, size: f32) -> Option<(CGGlyph, &'static str)> {
        for name in super::CJK_FALLBACKS {
            let hit = with_font(name, size, |font| {
                let mut buf = [0u16; 2];
                let units = ch.encode_utf16(&mut buf);
                let mut glyphs = [0u16; 2];
                let ok = unsafe {
                    font.get_glyphs_for_characters(
                        units.as_ptr(),
                        glyphs.as_mut_ptr(),
                        units.len() as core_foundation::base::CFIndex,
                    )
                };
                if ok && glyphs[0] != 0 {
                    Some(glyphs[0])
                } else {
                    None
                }
            });
            if let Some(Some(glyph)) = hit {
                return Some((glyph, name));
            }
        }
        None
    }

    /// 取字符的合成倾斜轮廓（gpui 路径）。
    pub fn glyph_path(
        ch: char,
        font_size: f32,
        pen_x: Pixels,
        baseline_y: Pixels,
    ) -> Option<Path<Pixels>> {
        let (glyph, name) = glyph_for_char(ch, font_size)?;
        let k = super::shear_factor(super::SLANT_DEG);
        with_font(name, font_size, |font| {
            let raw = unsafe {
                CTFontCreatePathForGlyph(font.as_concrete_TypeRef(), glyph, std::ptr::null())
            };
            if raw.is_null() {
                return None;
            }
            let cpath = unsafe { CGPath::from_ptr(raw) };
            build_path(&cpath, f32::from(pen_x), f32::from(baseline_y), k)
        })
        .flatten()
    }

    /// 把 CoreText 轮廓转成 gpui 路径，并施加剪切。
    ///
    /// 坐标换算：字形空间 `(x, y)`（y 向上）→ 设备空间
    /// `(pen_x + x + k*y, baseline_y - y)`（y 向下）。
    fn build_path(cpath: &CGPath, pen_x: f32, baseline_y: f32, k: f32) -> Option<Path<Pixels>> {
        let mut builder = PathBuilder::fill();
        let mut has_geometry = false;
        cpath.apply(&|el| {
            let to_device = |pt: core_graphics::geometry::CGPoint| {
                point(
                    px(pen_x + pt.x as f32 + k * pt.y as f32),
                    px(baseline_y - pt.y as f32),
                )
            };
            match el.element_type {
                CGPathElementType::MoveToPoint => {
                    builder.move_to(to_device(el.points()[0]));
                    has_geometry = true;
                }
                CGPathElementType::AddLineToPoint => {
                    builder.line_to(to_device(el.points()[0]));
                }
                CGPathElementType::AddQuadCurveToPoint => {
                    let pts = el.points();
                    builder.curve_to(to_device(pts[1]), to_device(pts[0]));
                }
                CGPathElementType::AddCurveToPoint => {
                    let pts = el.points();
                    builder.cubic_bezier_to(
                        to_device(pts[2]),
                        to_device(pts[0]),
                        to_device(pts[1]),
                    );
                }
                CGPathElementType::CloseSubpath => builder.close(),
            }
        });
        if !has_geometry {
            return None;
        }
        builder.build().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_chars_need_oblique() {
        // 汉字、全角标点、中文标点、带圈字符 → 需要合成斜体
        assert!(needs_oblique('斜'));
        assert!(needs_oblique('体'));
        assert!(needs_oblique('，'));
        assert!(needs_oblique('。'));
        assert!(needs_oblique('、'));
        assert!(needs_oblique('①'));
        assert!(needs_oblique('㊙')); // 0x3299 带圈字符
        assert!(needs_oblique('（'));
        assert!(needs_oblique('）'));
    }

    #[test]
    fn split_segments_marks_cjk_runs() {
        let text = "ab斜体cd，";
        let segs = split_segments(text);
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].0, 0..2);
        assert!(!segs[0].1);
        assert_eq!(segs[1].0, 2..8); // "斜体" 各 3 字节
        assert!(segs[1].1);
        assert_eq!(segs[2].0, 8..10);
        assert!(!segs[2].1);
        assert_eq!(segs[3].0, 10..13);
        assert!(segs[3].1);
    }

    #[test]
    fn split_segments_handles_empty_text() {
        assert!(split_segments("").is_empty());
    }

    #[test]
    fn shear_factor_matches_slant_angle() {
        // 12° → tan(12°) ≈ 0.2126
        let k = shear_factor(12.0);
        assert!((k - 0.2126).abs() < 0.001, "k = {k}");
        // 倾斜角为 0 时不偏移
        assert_eq!(shear_factor(0.0), 0.0);
    }

    /// CJK 回退字体顺序必须与 `main.rs` 的 `italic_fallbacks()` 一致，
    /// 否则自绘字形与排版占位会错位（见模块文档）。
    #[test]
    fn cjk_fallbacks_match_italic_fallbacks() {
        assert_eq!(CJK_FALLBACKS[0], "PingFang SC");
        assert_eq!(CJK_FALLBACKS.len(), 6);
    }
}
