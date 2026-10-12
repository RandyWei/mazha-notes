//! 显示偏移 ↔ 源偏移 双向映射（`OffsetMap`，SPEC §6.1）。
//!
//! 这是 WYSIWYG 的核心数据结构：Markdown 语法标记（`**`、`#`、`` ` `` 等）被隐藏后，
//! 显示文本与源文本不再一一对应，光标落点需要在两种坐标之间换算。
//!
//! ## 模型
//! 一行文本被划分为若干**可见区间**（连续可见字符的源字节范围），相邻可见区间之间是
//! 被隐藏的语法标记，在显示上**宽度为 0**。可见区间在显示空间连续铺满，因此：
//!
//! - 任意显示位置都能换算到（或吸附到最近的）一个可落点源边界；
//! - 光标永远落在源文本的**字符边界**上，绝不会插进隐藏标记内部；
//! - 纯文本区间内 1 个显示字符 = 1 个源字符，只需记录每个可见字符的起始源字节
//!   就能做精确双向映射，无需存整行文本。
//!
//! ## 交叉点的稳定规则
//! 显示空间的交叉点（一个可见区间结束、下一个开始，中间隔了隐藏标记）对应两个可能的
//! 源边界：前一区间终点（=隐藏标记起点）和后一区间起点。为让光标稳定（输入/删除不漂移），
//! 约定 `display_to_source` 取**源偏移更小**的那个，即隐藏标记起点。
//!
//! 实现：`src_marks`（每个可落点源边界的字节偏移，升序）与等长 `disp_marks`（显示位置，
//! 交叉点处允许重复），双向查找都是 O(log n) 二分；`display_len` 为可见字符总数快照。

use std::ops::Range;

/// 构造 `OffsetMap` 失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OffsetMapError {
    /// 可见区间越出文本长度。
    #[error("可见区间 {start}..{end} 越出文本长度 {len}")]
    OutOfBounds {
        /// 区间起点。
        start: usize,
        /// 区间终点。
        end: usize,
        /// 文本字节长度。
        len: usize,
    },

    /// 区间端点不是字符边界。
    #[error("偏移 {offset} 不是字符边界")]
    NotCharAligned {
        /// 非法的字节偏移。
        offset: usize,
    },

    /// 区间未排序或相互重叠。
    #[error("可见区间未排序或重叠：上一区间终点 {prev_end}，本区间起点 {start}")]
    Unordered {
        /// 本区间起点。
        start: usize,
        /// 上一区间的终点。
        prev_end: usize,
    },
}

/// 本模块的 `Result` 别名。
pub type MapResult<T> = std::result::Result<T, OffsetMapError>;

/// 显示位置：显示文本中的字符索引（newtype）。
///
/// `0` 是行首；第 k 个显示字符与其后的空位都表示为 `DisplayPos(k)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayPos(pub usize);

impl DisplayPos {
    /// 行首显示位置。
    pub const ZERO: DisplayPos = DisplayPos(0);

    /// 取出内部字符索引。
    pub fn get(self) -> usize {
        self.0
    }
}

/// 显示偏移 ↔ 源偏移 双向映射。
///
/// 快照结构：投影层每次按 `ChangeSummary.affected` 重建受影响的行即可，无需增量维护。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffsetMap {
    /// 每个可落点源边界（字节偏移，升序）。
    src_marks: Vec<usize>,
    /// 与 `src_marks` 等长的显示位置（升序，交叉点处重复）。
    disp_marks: Vec<usize>,
    /// 可见字符总数（= 最后一个显示位置）。
    display_len: usize,
}

impl OffsetMap {
    /// 由"可见源区间"构造；隐藏区间 = 补集。
    ///
    /// 要求 `visible_runs`：升序、互不重叠、端点落在字符边界上、区间非空；
    /// 相邻区间（`prev.end == next.start`）会合并成一个。
    ///
    /// # Errors
    /// - 越出文本 → [`OffsetMapError::OutOfBounds`]；
    /// - 端点非字符边界 → [`OffsetMapError::NotCharAligned`]；
    /// - 未排序或重叠 → [`OffsetMapError::Unordered`]。
    pub fn from_visible_runs(source: &str, visible_runs: &[Range<usize>]) -> MapResult<Self> {
        let mut runs: Vec<Range<usize>> = Vec::with_capacity(visible_runs.len());
        for r in visible_runs {
            if r.start >= r.end {
                continue; // 忽略空区间
            }
            if r.end > source.len() {
                return Err(OffsetMapError::OutOfBounds {
                    start: r.start,
                    end: r.end,
                    len: source.len(),
                });
            }
            if !source.is_char_boundary(r.start) || !source.is_char_boundary(r.end) {
                return Err(OffsetMapError::NotCharAligned {
                    offset: if source.is_char_boundary(r.start) {
                        r.end
                    } else {
                        r.start
                    },
                });
            }
            if let Some(prev) = runs.last_mut() {
                if r.start < prev.end {
                    return Err(OffsetMapError::Unordered {
                        start: r.start,
                        prev_end: prev.end,
                    });
                }
                if r.start == prev.end {
                    // 相邻区间：合并，避免在真实文本中间造出假交叉点
                    prev.end = r.end;
                    continue;
                }
            }
            runs.push(r.clone());
        }

        let mut src_marks = Vec::new();
        let mut disp_marks = Vec::new();
        // 已铺排的可见字符数 = 下一个空位使用的显示位置
        let mut display_cursor = 0usize;
        for run in &runs {
            // 区间起点边界；若前面有隐藏标记，此显示位置与上一区间终点重合 → 交叉点
            src_marks.push(run.start);
            disp_marks.push(display_cursor);
            // 已通过范围校验，get 必命中；用 unwrap_or_default 保持无 panic 路径
            let src_text = source.get(run.clone()).unwrap_or_default();
            // char_indices 给出每个可见字符的起点偏移与其真身，直接算"该字符之后"的
            // 源边界（after），天然处理多字节 CJK；循环结束 after == run.end 已在表中。
            for (rel, ch) in src_text.char_indices() {
                let after = run.start + rel + ch.len_utf8();
                display_cursor += 1;
                src_marks.push(after);
                disp_marks.push(display_cursor);
            }
        }

        Ok(Self {
            src_marks,
            disp_marks,
            display_len: display_cursor,
        })
    }

    /// 空映射（空文本或全部隐藏）。
    pub fn empty() -> Self {
        Self {
            src_marks: Vec::new(),
            disp_marks: Vec::new(),
            display_len: 0,
        }
    }

    /// 源字节偏移 → 显示位置。
    ///
    /// 落进隐藏标记内部时，吸附到**标记起点**（即前一个可见区间的显示终点）；
    /// 超出文档末尾同理吸附到最后一个可落点。
    pub fn source_to_display(&self, src: usize) -> DisplayPos {
        let Some(idx) = self.src_marks.partition_point(|&b| b <= src).checked_sub(1) else {
            return DisplayPos::ZERO; // 没有标记（空映射）
        };
        DisplayPos(self.disp_marks[idx])
    }

    /// 显示位置 → 源字节偏移。
    ///
    /// 交叉点（隐藏标记两侧共享同一显示位置）取**源偏移更小**者 = 隐藏标记起点，保证
    /// 光标落点稳定：输入字符永远落在可见文本末尾、隐藏标记之前。
    pub fn display_to_source(&self, disp: DisplayPos) -> usize {
        let rank = self.disp_marks.partition_point(|&d| d < disp.0);
        // rank 是第一个 display == disp 的下标；交叉点处可能连续多个，取最小的 src。
        self.src_marks.get(rank).copied().unwrap_or_else(|| {
            // 超出显示范围：钳制到最后一个可落点
            *self.src_marks.last().unwrap_or(&0)
        })
    }

    /// 整行显示宽度（可见字符总数）。
    pub fn display_len(&self) -> usize {
        self.display_len
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // 测试里大量单区间数组是刻意写法，非笔误
mod tests {
    use super::*;

    fn map_of(source: &str, visible: &[Range<usize>]) -> OffsetMap {
        OffsetMap::from_visible_runs(source, visible).unwrap()
    }

    #[test]
    fn plain_text_is_identity() {
        let src = "hello";
        let m = map_of(src, &[0..5]);
        assert_eq!(m.display_len(), 5);
        for i in 0..=5 {
            assert_eq!(m.source_to_display(i), DisplayPos(i));
            assert_eq!(m.display_to_source(DisplayPos(i)), i);
        }
    }

    #[test]
    fn heading_marker_hidden() {
        // `# 标题`：隐藏 "# "（2 字节），可见 "标题"
        let src = "# 标题";
        let m = map_of(src, &[2..8]);
        assert_eq!(m.display_len(), 2);
        assert_eq!(m.source_to_display(2), DisplayPos(0));
        assert_eq!(m.source_to_display(5), DisplayPos(1)); // "标" 起点
        assert_eq!(m.source_to_display(8), DisplayPos(2)); // 结尾
        assert_eq!(m.display_to_source(DisplayPos(0)), 2);
        assert_eq!(m.display_to_source(DisplayPos(1)), 5);
        assert_eq!(m.display_to_source(DisplayPos(2)), 8);
    }

    #[test]
    fn cjk_uses_char_not_byte_for_display() {
        // `**你好**`：标=2 字节，内容 "你好" 各 3 字节
        let src = "**你好**";
        let m = map_of(src, &[2..8]);
        assert_eq!(m.display_len(), 2);
        assert_eq!(m.source_to_display(2), DisplayPos(0));
        assert_eq!(
            m.source_to_display(3),
            DisplayPos(0),
            "落进字符内部吸附到起点"
        );
        assert_eq!(m.source_to_display(5), DisplayPos(1), "5 是字符边界");
        assert_eq!(m.source_to_display(8), DisplayPos(2));
        assert_eq!(m.display_to_source(DisplayPos(1)), 5);
    }

    #[test]
    fn marker_interior_clamps_to_marker_start() {
        // 可见: hello 0..5, bold 7..11, world 13..18
        let src = "hello**bold**world";
        let m = map_of(src, &[0..5, 7..11, 13..18]);
        assert_eq!(m.display_len(), 14);
        // "**" 的第 2 个 '*' 在字节 6，落在隐藏标记内 → 吸附到标记起点 5
        assert_eq!(m.source_to_display(6), DisplayPos(5));
        // 关闭标记 "**" 的第 2 个 '*' 在字节 12 → 吸附到 11
        assert_eq!(m.source_to_display(12), DisplayPos(9));
        // 越界 → 吸附末尾
        assert_eq!(m.source_to_display(999), DisplayPos(m.display_len()));
    }

    #[test]
    fn junction_is_stable_and_prefers_smaller_src() {
        let src = "hello**bold**world";
        let m = map_of(src, &[0..5, 7..11, 13..18]);
        // 交叉点 display 5 = "hello" 之后（隐藏 "**" 处）
        assert_eq!(m.display_to_source(DisplayPos(5)), 5, "取隐藏标记起点");
        // 交叉点 display 9 = "bold" 之后（隐藏 "**" 处）
        assert_eq!(m.display_to_source(DisplayPos(9)), 11);
        // source→display→source 在可见字符中点保持
        assert_eq!(m.display_to_source(m.source_to_display(8)), 8);
        // 在交叉点两侧的源偏移，落到同一稳定落点 = 隐藏标记起点
        assert_eq!(m.display_to_source(m.source_to_display(7)), 5);
        assert_eq!(m.display_to_source(m.source_to_display(11)), 11);
    }

    #[test]
    fn adjacent_runs_are_merged() {
        let src = "abcdef";
        let m = map_of(src, &[0..3, 3..6]);
        assert_eq!(m.display_len(), 6);
        assert_eq!(m.source_to_display(3), DisplayPos(3)); // 无隐藏标记 → 无交叉点
        assert_eq!(m.display_to_source(DisplayPos(3)), 3);
    }

    #[test]
    fn trailing_hidden_marker_clamps_eof() {
        let src = "x**b**";
        let m = map_of(src, &[0..1, 3..4]);
        assert_eq!(m.display_len(), 2);
        assert_eq!(
            m.source_to_display(6),
            DisplayPos(2),
            "EOF 在尾部标记内钳到末尾"
        );
    }

    #[test]
    fn all_hidden_maps_to_zero() {
        let m = map_of("**", &[]);
        assert_eq!(m.display_len(), 0);
        assert_eq!(m.source_to_display(0), DisplayPos(0));
        assert_eq!(m.source_to_display(2), DisplayPos(0));
        assert_eq!(m.display_to_source(DisplayPos(0)), 0);
        assert_eq!(m.display_to_source(DisplayPos(9)), 0);
    }

    #[test]
    fn empty_text() {
        let m = map_of("", &[]);
        assert_eq!(m.display_len(), 0);
        assert_eq!(m.source_to_display(0), DisplayPos(0));
        assert_eq!(m.display_to_source(DisplayPos(0)), 0);
    }

    #[test]
    fn empty_runs_are_ignored() {
        let m = map_of("ab", &[0..0, 0..2]);
        assert_eq!(m.display_len(), 2);
        assert_eq!(m.source_to_display(1), DisplayPos(1));
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert_eq!(
            OffsetMap::from_visible_runs("abc", &[1..9]),
            Err(OffsetMapError::OutOfBounds {
                start: 1,
                end: 9,
                len: 3
            })
        );
        assert_eq!(
            OffsetMap::from_visible_runs("你好", &[0..1]),
            Err(OffsetMapError::NotCharAligned { offset: 1 })
        );
        assert_eq!(
            OffsetMap::from_visible_runs("abcdef", &[0..4, 2..6]),
            Err(OffsetMapError::Unordered {
                start: 2,
                prev_end: 4
            })
        );
        assert_eq!(
            OffsetMap::from_visible_runs("abcdef", &[3..6, 0..3]),
            Err(OffsetMapError::Unordered {
                start: 0,
                prev_end: 6
            })
        );
    }

    #[test]
    fn display_pos_newtype_semantics() {
        assert_eq!(DisplayPos::ZERO, DisplayPos(0));
        assert_eq!(DisplayPos(2).get(), 2);
        assert!(DisplayPos(1) < DisplayPos(3));
    }
}
