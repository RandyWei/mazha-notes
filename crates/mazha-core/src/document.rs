//! 文档：源文本的单一真相（SPEC §4.3）。
//!
//! 这是整个编辑器的最核心数据结构。所有编辑都通过 [`Document::apply`] 进入，
//! 先做**字符边界校验**，再以 `ropey::Rope` 应用修改（O(log n)），并按
//! `Edit → 逆 Edit` 的形式维护 undo/redo 历史。
//!
//! 设计要点：
//! - 对外一律用**字节偏移**（UTF-8），内部换算成 rope 的字符索引；
//! - 渲染投影（`Projection`）、CRDT（`Y.Text`）、文件保存都派生自这里的文本；
//! - 本模块**不做任何 UI 语义**（如行/列/隐藏标记），那是投影层的事。

use crate::edit::{ChangeSummary, Edit};
use crate::error::{EditError, Result};
use ropey::Rope;
use std::borrow::Cow;
use std::ops::Range;

/// undo/redo 栈的默认上限，防止长会话内存无限增长。
const DEFAULT_MAX_HISTORY: usize = 1000;

/// 一条历史记录：保存原始编辑与其逆编辑。
///
/// - `undo` 重放 `inverse` 并从撤销栈弹出；被撤销的 `original` 推入重做栈。
/// - `redo` 重放 `original` 并推回撤销栈。
/// - 逆编辑在 `apply` 时生成（此时还能拿到被删除的原文），之后无需再次访问文本。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    /// 用户原始编辑（redo 时重放）。
    original: Edit,
    /// 逆向编辑（undo 时重放）。
    inverse: Edit,
}

/// 码札文档：持有一份 Markdown 源文本。
pub struct Document {
    /// 源文本（rope，支持 O(log n) 的插入/删除）。
    text: Rope,
    /// 撤销栈：栈顶是最近一次编辑。
    undo_stack: Vec<Change>,
    /// 重做栈：栈顶是可重做的最近一次撤销。
    redo_stack: Vec<Change>,
    /// 撤销栈最大深度。
    max_history: usize,
    /// 块森林缓存（与 `text` 保持一致：每次编辑后增量重解析）。
    /// 供 [`Document::projection`] 与 [`Document::blocks`] 复用，避免重复全量解析。
    blocks: Vec<mazha_md::Block>,
    /// 渲染投影缓存（与 `text` 保持一致：编辑后按脏行增量刷新）。
    /// 供 [`Document::projection`] 复用，避免每次渲染都重建全部行。
    projection: Option<crate::Projection>,
}

/// 编辑在**旧文本**坐标中的字节范围（增量重解析/投影刷新定位脏区用）。
///
/// 注意 [`Edit::source_range`] 对插入返回的是**新文本**坐标（`at..at+len`），
/// 不能直接用于旧坐标定位；这里给出严格的旧坐标版本：插入为空区间。
fn old_range_of(edit: &Edit) -> Range<usize> {
    match edit {
        Edit::Insert { at, .. } => *at..*at,
        Edit::Delete { range }
        | Edit::Replace { range, .. }
        | Edit::ToggleInline { range, .. }
        | Edit::ToggleBlock { range, .. } => range.clone(),
    }
}

/// 编辑在新文本坐标中的字节范围（供投影/缓存增量刷新计算脏区）。
///
/// 与 [`old_range_of`] 成对使用：
/// 插入时旧区间为空、新区间为插入内容；删除时反之。
fn new_range_of(edit: &Edit) -> Range<usize> {
    match edit {
        Edit::Insert { at, text } => *at..at.saturating_add(text.len()),
        Edit::Delete { range } => range.start..range.start,
        Edit::Replace { range, text } => range.start..range.start.saturating_add(text.len()),
        // 高层命令在 apply 前已解析成基础编辑，这里不直接出现
        Edit::ToggleInline { range, .. } | Edit::ToggleBlock { range, .. } => range.clone(),
    }
}

impl Document {
    /// 用初始文本新建文档。
    pub fn new(text: &str) -> Self {
        Self {
            text: Rope::from_str(text),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_history: DEFAULT_MAX_HISTORY,
            blocks: mazha_md::parse(text),
            projection: None,
        }
    }

    /// 文档当前字节长度。
    pub fn len(&self) -> usize {
        self.text.len_bytes()
    }

    /// 文档是否为空（零字节）。
    pub fn is_empty(&self) -> bool {
        self.text.len_bytes() == 0
    }

    /// 返回整篇文档文本（**拷贝**）。只适合低频场景（保存、小文档读取）。
    /// 逐行渲染用 [`Document::slice`]，避免整体拷贝。
    pub fn text(&self) -> String {
        self.text.to_string()
    }

    /// 当前块森林（缓存，与源文本同步；每次编辑后增量重解析）。
    pub fn blocks(&self) -> &[mazha_md::Block] {
        &self.blocks
    }

    /// 生成/获取整篇渲染投影（隐藏语法标记，供 UI 直接渲染）。
    ///
    /// 返回缓存引用（UI 逐帧渲染无需拷贝）：编辑后缓存会被**增量刷新**
    /// （只重建编辑影响的行），因此不会因多次读取而重复付出全量代价。
    ///
    /// Note: 需要 `&mut self` 以填充/刷新缓存；纯读取场景请先复制
    /// （`doc.projection().clone()`）。
    ///
    /// # Panics
    /// 内部不变式：缓存填充后即为 `Some`。该 expect 不应触发。
    pub fn projection(&mut self) -> &crate::Projection {
        if self.projection.is_none() {
            // 暂借块缓存，构建完立刻还回，避免与 self.text 的借用打架
            let blocks = std::mem::take(&mut self.blocks);
            let text = self.text.to_string();
            self.projection = Some(crate::projection::build_from(&text, &blocks));
            self.blocks = blocks;
        }
        self.projection.as_ref().expect("投影缓存刚被填充")
    }

    /// 返回字节范围 `range` 内的文本（`Cow`：连续 chunk 时零拷贝借用，否则拷贝）。
    ///
    /// # Panics
    /// `range` 两端不是字符边界时会 panic；调用方应先通过
    /// [`Document::snap_to_boundary`] 或本次编辑的返回保证合法性。
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        // ropey 的 `slice`/`insert`/`remove` 都以"字符索引"为参数，
        // 这里把字节边界换算成字符边界：end 落在边界上时，
        // byte_to_char(end) 恰好是"第一个被排除的字符"。
        let cstart = self.text.byte_to_char(range.start);
        let cend = self.text.byte_to_char(range.end);
        self.text.slice(cstart..cend).into()
    }

    /// 尝试返回字节范围文本。
    ///
    /// # Errors
    /// - 范围两端不是字符边界 → [`EditError::InvalidBoundary`]。
    /// - 范围越界或倒序 → [`EditError::InvalidRange`]。
    pub fn try_slice(&self, range: Range<usize>) -> Result<Cow<'_, str>> {
        self.validate_byte_range(&range)?;
        Ok(self.slice(range))
    }

    /// 暴露底层 rope，供投影/导出层做高效逐行读取。
    pub fn rope(&self) -> &Rope {
        &self.text
    }

    /// 把任意字节偏移吸附到最近的字符边界（超出长度 clamp 到末尾）。
    pub fn snap_to_boundary(&self, byte: usize) -> usize {
        if byte >= self.text.len_bytes() {
            return self.text.len_bytes();
        }
        let char_idx = self.text.byte_to_char(byte);
        // byte_to_char 返回"包含该字节的字符"的索引，char_to_byte 取该字符起点，
        // 因此总能得到边界；若 byte 本身是边界，两者相同。
        self.text.char_to_byte(char_idx)
    }

    /// 包含字节偏移 `at` 的**逻辑行**源范围（不含行尾换行）。
    ///
    /// `at` 恰为换行符位置时归**前一行**（与 `line_index_of` 的约定一致：
    /// 让"删行尾换行"这类编辑把两行都纳入影响范围）。越界会 clamp。
    pub fn line_range_of(&self, at: usize) -> Range<usize> {
        let at = self.snap_to_boundary(at);
        let before = self.slice(0..at);
        let start = before.rfind('\n').map_or(0, |i| i + 1);
        let rest = self.slice(at..self.len());
        let end = rest.find('\n').map_or(at + rest.len(), |i| at + i);
        start..end
    }

    /// 字节偏移 `at` 所在的**词**范围（CJK 感知；空区间表示不在词内）。
    ///
    /// 词的定义（用于双击选中/词级移动）：
    /// - ASCII 词 = 连续的 `[0-9A-Za-z_'-]`；
    /// - CJK 词 = 连续的中日韩表意文字/假名/谚文；
    /// - 其它符号各自成词（单击标点只选中它自己）；
    /// - 落在两个词之间的空白返回空区间 `at..at`。
    pub fn word_range_at(&self, at: usize) -> Range<usize> {
        let at = self.snap_to_boundary(at);
        let line = self.line_range_of(at);
        if at < line.start || at > line.end {
            return at..at;
        }
        let local = at - line.start;
        let text = self.slice(line.clone());
        let bounds = word_bounds(&text, local);
        line.start + bounds.start..line.start + bounds.end
    }

    /// 从字节偏移向前（左）移动**一个字符**，落在字符边界上。
    pub fn prev_char_boundary(&self, byte: usize) -> usize {
        if byte == 0 {
            return 0;
        }
        let char_idx = self.text.byte_to_char(byte);
        let is_boundary = self.text.char_to_byte(char_idx) == byte;
        if is_boundary && char_idx > 0 {
            // 当前就在边界：上一个字符的起点
            self.text.char_to_byte(char_idx - 1)
        } else {
            // 落在字符内部：吸附到该字符起点
            self.text.char_to_byte(char_idx)
        }
    }

    /// 从字节偏移向后（右）移动**一个字符**，落在字符边界上。
    pub fn next_char_boundary(&self, byte: usize) -> usize {
        if byte >= self.text.len_bytes() {
            return self.text.len_bytes();
        }
        let char_idx = self.text.byte_to_char(byte);
        let next = (char_idx + 1).min(self.text.len_chars());
        self.text.char_to_byte(next)
    }

    /// 应用一次编辑，更新 undo 历史并返回变更摘要。
    ///
    /// 若偏移非法（非字符边界 / 越界），返回错误且**不修改文档**。
    ///
    /// # Errors
    /// - 插入偏移不是字符边界 → [`EditError::InvalidBoundary`]。
    /// - 删除/替换范围越界或倒序 → [`EditError::InvalidRange`]。
    pub fn apply(&mut self, edit: Edit) -> Result<ChangeSummary> {
        // 先把高层命令（ToggleInline / ToggleBlock）解析成对源文本的基础编辑，
        // 之后 undo/redo 只需记录基础编辑的源级副作用。
        let edit = self.resolve(edit);
        self.validate_edit(&edit)?;
        // 在修改前生成逆编辑：此时才能取到被覆盖的原文。
        let inverse = self.inverse_of(&edit);
        let summary = self.apply_and_refresh(&edit);
        // 任何新编辑都会使重做栈失效。
        self.redo_stack.clear();
        self.undo_stack.push(Change {
            original: edit,
            inverse,
        });
        if self.undo_stack.len() > self.max_history {
            self.undo_stack.remove(0);
        }
        Ok(summary)
    }

    /// 应用一次编辑并同步到 CRDT（`CrdtDoc`）。
    ///
    /// 与 [`Document::apply`] 的区别：apply 成功后立即把解析后的基础编辑同步到
    /// `crdt`，保证本地文档与 CRDT 状态一致（供后续增量同步给远端）。
    ///
    /// # Errors
    /// - 编辑非法（同 [`Document::apply`]）时返回错误，**不会**修改 `crdt`。
    ///
    /// # Panics
    /// 若同步阶段报告越界则 panic；由于 `apply` 已对同一组偏移做过校验，
    /// 这在正常情况下不可能发生。
    pub fn apply_and_sync(
        &mut self,
        edit: Edit,
        crdt: &mut mazha_crdt::CrdtDoc,
    ) -> Result<ChangeSummary> {
        let resolved = self.resolve(edit);
        let summary = self.apply(resolved.clone())?;
        // apply 已成功，同步阶段理论上不会失败（同一组已校验的偏移）
        crate::crdt::apply_edit_to_crdt(crdt, &resolved)
            .expect("CRDT 同步失败（apply 已校验偏移，不应发生）");
        Ok(summary)
    }

    /// 把高层编辑命令解析为具体的基础编辑（Insert/Delete/Replace）。
    pub(crate) fn resolve(&self, edit: Edit) -> Edit {
        match edit {
            Edit::ToggleInline { range, mark } => {
                crate::commands::toggle_inline(&self.text(), range, mark)
            }
            Edit::ToggleBlock { range, target } => {
                crate::commands::set_block(&self.text(), range, &target)
            }
            base => base,
        }
    }

    /// 撤销最近一次编辑。无历史时返回 `None`。
    pub fn undo(&mut self) -> Option<ChangeSummary> {
        let change = self.undo_stack.pop()?;
        let summary = self.apply_and_refresh(&change.inverse);
        self.redo_stack.push(change);
        Some(summary)
    }

    /// 重做最近一次撤销。无可重做项返回 `None`。
    pub fn redo(&mut self) -> Option<ChangeSummary> {
        let change = self.redo_stack.pop()?;
        let summary = self.apply_and_refresh(&change.original);
        self.undo_stack.push(change);
        Some(summary)
    }

    /// 是否可以撤销/重做。
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// 是否可以重做。
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// 清空 undo/redo 历史（如打开新文件时）。
    pub fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    /// 设置撤销栈最大深度。
    pub fn set_max_history(&mut self, max: usize) {
        self.max_history = max;
        while self.undo_stack.len() > max {
            self.undo_stack.remove(0);
        }
    }

    // ---- 内部实现 ----

    /// 校验编辑合法性：全部偏移落在边界内。
    fn validate_edit(&self, edit: &Edit) -> Result<()> {
        match edit {
            Edit::Insert { at, .. } => self.validate_byte_offset(*at),
            Edit::Delete { range } | Edit::Replace { range, .. } => self.validate_byte_range(range),
            // 切换命令：校验目标范围（apply 前已解析，这里兜底保证自洽）
            Edit::ToggleInline { range, .. } | Edit::ToggleBlock { range, .. } => {
                self.validate_byte_range(range)
            }
        }
    }

    /// 校验单个字节偏移是否合法且为字符边界。
    fn validate_byte_offset(&self, byte: usize) -> Result<()> {
        if byte > self.text.len_bytes() {
            return Err(EditError::InvalidRange {
                start: byte,
                end: byte,
                doc_len: self.text.len_bytes(),
            });
        }
        let char_idx = self.text.byte_to_char(byte);
        if self.text.char_to_byte(char_idx) != byte {
            return Err(EditError::InvalidBoundary { offset: byte });
        }
        Ok(())
    }

    /// 校验字节范围是否有序且包围合法边界。
    fn validate_byte_range(&self, range: &Range<usize>) -> Result<()> {
        if range.start > range.end || range.end > self.text.len_bytes() {
            return Err(EditError::InvalidRange {
                start: range.start,
                end: range.end,
                doc_len: self.text.len_bytes(),
            });
        }
        self.validate_byte_offset(range.start)?;
        self.validate_byte_offset(range.end)?;
        Ok(())
    }

    /// 在**修改前**依据当前文本生成逆编辑。
    fn inverse_of(&self, edit: &Edit) -> Edit {
        match edit {
            Edit::Insert { at, text } => Edit::Delete {
                range: *at..at.saturating_add(text.len()),
            },
            Edit::Delete { range } => Edit::Insert {
                at: range.start,
                text: self.slice(range.clone()).into_owned(),
            },
            Edit::Replace { range, text } => Edit::Replace {
                // 替换后的范围以新文本长度计算
                range: range.start..range.start.saturating_add(text.len()),
                text: self.slice(range.clone()).into_owned(),
            },
            // 切换命令：先解析成基础编辑再求逆（apply 前已解析，这里兜底）
            Edit::ToggleInline { .. } | Edit::ToggleBlock { .. } => {
                let concrete = self.resolve(edit.clone());
                self.inverse_of(&concrete)
            }
        }
    }

    /// 直接对文本应用编辑（不触碰任何历史栈），返回变更摘要。
    /// 应用编辑并增量刷新缓存（apply/undo/redo 的统一入口）。
    ///
    /// 依次维护两份缓存，使二者始终与 `self.text` 同步：
    /// 1. 块森林：按本次编辑的源范围做 **增量重解析**（[`mazha_md::reparse`]）；
    /// 2. 投影：按本次编辑的脏行做 **增量刷新**（[`crate::Projection::refresh`]）。
    fn apply_and_refresh(&mut self, edit: &Edit) -> ChangeSummary {
        let old_len = self.text.len_bytes();
        let affected_old = old_range_of(edit);
        let affected_new = new_range_of(edit);
        let summary = self.apply_raw(edit);
        // 暂借块缓存，避免与 self.text 的借用冲突
        let blocks = std::mem::take(&mut self.blocks);
        let text = self.text.to_string();
        self.blocks = mazha_md::reparse(&text, old_len, &blocks, affected_old.clone());
        // 投影缓存若存在，按脏行增量刷新（不存在则下次读取时懒构建）
        if let Some(proj) = self.projection.as_mut() {
            proj.refresh(&text, affected_old, affected_new, &self.blocks);
        }
        summary
    }

    fn apply_raw(&mut self, edit: &Edit) -> ChangeSummary {
        let affected = match edit {
            Edit::Insert { at, text } => {
                let char_idx = self.text.byte_to_char(*at);
                self.text.insert(char_idx, text);
                *at..at.saturating_add(text.len())
            }
            Edit::Delete { range } => {
                let start = self.text.byte_to_char(range.start);
                let end = self.text.byte_to_char(range.end);
                self.text.remove(start..end);
                range.start..range.start
            }
            Edit::Replace { range, text } => {
                let start = self.text.byte_to_char(range.start);
                let end = self.text.byte_to_char(range.end);
                // ropey 没有 replace，等价于 remove + insert（两者都是 O(log n)）。
                self.text.remove(start..end);
                self.text.insert(start, text);
                range.start..range.start.saturating_add(text.len())
            }
            // 切换命令：先解析成基础编辑再应用（apply 前已解析，这里兜底）
            Edit::ToggleInline { .. } | Edit::ToggleBlock { .. } => {
                let concrete = self.resolve(edit.clone());
                self.apply_raw(&concrete).affected
            }
        };
        let doc_len = self.text.len_bytes();
        ChangeSummary { affected, doc_len }
    }
}

/// 单行文本内，字节偏移 `at` 处的**词范围**（CJK 感知）。
///
/// 约定：
/// - 光标左右任一侧挨着词字符 → 返回该词（优先左侧，符合"光标停在词尾属于该词"）；
/// - 两侧都是空白/标点 → 返回空区间 `at..at`（不把单个标点当词）；
/// - ASCII 词与 CJK 词互不粘连（`abc你好` 会被切成 `abc` 与 `你好`）。
fn word_bounds(line: &str, at: usize) -> Range<usize> {
    let left = line.get(..at).and_then(|s| s.chars().next_back());
    let right = line.get(at..).and_then(|s| s.chars().next());
    let anchor = match (left, right) {
        (Some(l), _) if is_word_char(l) => l,
        (_, Some(r)) if is_word_char(r) => r,
        _ => return at..at,
    };

    // 基准字符决定"同类"：ASCII 词只吃 ASCII 词字符，CJK 只吃 CJK
    let same_class = |c: char| {
        if is_ascii_word(anchor) {
            is_ascii_word(c)
        } else {
            is_cjk(c)
        }
    };

    let mut start = at;
    for (i, c) in line.get(..at).unwrap_or_default().char_indices().rev() {
        if same_class(c) {
            start = i;
        } else {
            break;
        }
    }
    let mut end = at;
    for (i, c) in line.get(at..).unwrap_or_default().char_indices() {
        if same_class(c) {
            end = at + i + c.len_utf8();
        } else {
            break;
        }
    }
    start..end
}

/// 是否 ASCII 词字符（`[0-9A-Za-z_'-]`）。
fn is_ascii_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '\'')
}

/// 是否 CJK 表意/假名/谚文字符（按 Unicode 区段粗略判断）。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2EFF   // 部首补充
        | 0x3000..=0x303F // CJK 符号与标点
        | 0x3040..=0x30FF // 平/片假名
        | 0x3100..=0x312F // 注音
        | 0x3130..=0x318F // 谚文兼容
        | 0x3400..=0x4DBF // 扩展 A
        | 0x4E00..=0x9FFF // 基本区
        | 0xA960..=0xA97F // 谚文扩展
        | 0xAC00..=0xD7FF // 谚文音节
        | 0xF900..=0xFAFF // 兼容表意
        | 0x20000..=0x2FA1F // 扩展 B..F + 兼容补充
    )
}

/// 是否词字符（ASCII 词或 CJK）。
fn is_word_char(c: char) -> bool {
    is_ascii_word(c) || is_cjk(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Selection;

    fn doc(s: &str) -> Document {
        Document::new(s)
    }

    #[test]
    fn empty_and_text() {
        let d = doc("你好，世界!");
        assert_eq!(d.len(), "你好，世界!".len());
        assert!(!d.is_empty());
        assert_eq!(d.text(), "你好，世界!");
        assert!(doc("").is_empty());
    }

    #[test]
    fn insert_at_edges_and_middle() {
        let mut d = doc("abcd");
        d.apply(Edit::Insert {
            at: 0,
            text: "Z".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "Zabcd");
        d.apply(Edit::Insert {
            at: 5,
            text: "E".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "ZabcdE");
        d.apply(Edit::Insert {
            at: 2,
            text: "X".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "ZaXbcdE");
    }

    #[test]
    fn insert_multi_byte_char_boundary() {
        // 「你」= 3 字节；1 不是边界，3 是边界
        let mut d = doc("你");
        assert_eq!(
            d.apply(Edit::Insert {
                at: 1,
                text: "x".into()
            }),
            Err(EditError::InvalidBoundary { offset: 1 })
        );
        assert_eq!(d.text(), "你", "非法编辑不应修改文档");
        d.apply(Edit::Insert {
            at: 3,
            text: "a".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "你a");
    }

    #[test]
    fn delete_and_replace() {
        let mut d = doc("hello world");
        d.apply(Edit::Delete { range: 5..6 }).unwrap();
        assert_eq!(d.text(), "helloworld");
        d.apply(Edit::Replace {
            range: 0..5,
            text: "HELLO".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "HELLOworld");
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)] // 刻意构造倒序区间以验证错误路径
    fn delete_invalid_ranges_error() {
        let mut d = doc("abc");
        assert_eq!(
            d.apply(Edit::Delete { range: 2..1 }),
            Err(EditError::InvalidRange {
                start: 2,
                end: 1,
                doc_len: 3
            })
        );
        assert_eq!(
            d.apply(Edit::Delete { range: 1..9 }),
            Err(EditError::InvalidRange {
                start: 1,
                end: 9,
                doc_len: 3
            })
        );
        assert_eq!(d.text(), "abc");
    }

    #[test]
    fn toggle_inline_through_document_and_undo() {
        let mut d = doc("hello world");
        d.apply(Edit::ToggleInline {
            range: 6..11,
            mark: crate::InlineMark::Strong,
        })
        .unwrap();
        assert_eq!(d.text(), "hello **world**");
        // 再次切换 → 解包裹
        d.apply(Edit::ToggleInline {
            range: 8..13,
            mark: crate::InlineMark::Strong,
        })
        .unwrap();
        assert_eq!(d.text(), "hello world");
        // undo 两次回到原文
        d.undo().unwrap();
        assert_eq!(d.text(), "hello **world**");
        d.undo().unwrap();
        assert_eq!(d.text(), "hello world");
        // redo 恢复
        d.redo().unwrap();
        assert_eq!(d.text(), "hello **world**");
    }

    #[test]
    fn toggle_block_heading_through_document() {
        let mut d = doc("title\nbody");
        d.apply(Edit::ToggleBlock {
            range: 0..5,
            target: crate::BlockTarget::Heading(1),
        })
        .unwrap();
        assert_eq!(d.text(), "# title\nbody");
        d.apply(Edit::ToggleBlock {
            range: 0..7,
            target: crate::BlockTarget::Paragraph,
        })
        .unwrap();
        assert_eq!(d.text(), "title\nbody");
    }

    #[test]
    fn toggle_invalid_range_rejected() {
        let mut d = doc("abc");
        assert!(
            d.apply(Edit::ToggleInline {
                range: 0..9,
                mark: crate::InlineMark::Code,
            })
            .is_err()
        );
        assert_eq!(d.text(), "abc");
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut d = doc("abc");
        d.apply(Edit::Insert {
            at: 3,
            text: "d".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "abcd");
        assert!(d.can_undo());
        d.undo().unwrap();
        assert_eq!(d.text(), "abc");
        assert!(d.can_redo());
        d.redo().unwrap();
        assert_eq!(d.text(), "abcd");
    }

    #[test]
    fn undo_clears_redo_on_new_edit() {
        let mut d = doc("a");
        d.apply(Edit::Insert {
            at: 1,
            text: "b".into(),
        })
        .unwrap(); // ab
        d.undo().unwrap(); // a
        assert!(d.can_redo());
        d.apply(Edit::Insert {
            at: 1,
            text: "c".into(),
        })
        .unwrap(); // ac
        assert!(!d.can_redo(), "新编辑应使重做栈失效");
        assert_eq!(d.text(), "ac");
    }

    #[test]
    fn undo_multiple_then_redo_all() {
        let mut d = doc("start");
        d.apply(Edit::Replace {
            range: 0..5,
            text: "hello".into(),
        })
        .unwrap();
        d.apply(Edit::Insert {
            at: 5,
            text: " world".into(),
        })
        .unwrap();
        d.apply(Edit::Replace {
            range: 0..11,
            text: "mazha".into(),
        })
        .unwrap();
        assert_eq!(d.text(), "mazha");
        d.undo().unwrap();
        assert_eq!(d.text(), "hello world");
        d.undo().unwrap();
        assert_eq!(d.text(), "hello");
        d.undo().unwrap();
        assert_eq!(d.text(), "start");
        assert!(!d.can_undo());
        d.redo().unwrap();
        d.redo().unwrap();
        d.redo().unwrap();
        assert_eq!(d.text(), "mazha");
    }

    #[test]
    fn undo_with_cjk() {
        let mut d = doc("你好");
        d.apply(Edit::Insert {
            at: 3,
            text: "到".into(),
        })
        .unwrap(); // 你到好
        assert_eq!(d.text(), "你到好");
        d.undo().unwrap();
        assert_eq!(d.text(), "你好");
        d.redo().unwrap();
        assert_eq!(d.text(), "你到好");
    }

    #[test]
    fn history_is_capped() {
        let mut d = doc("");
        d.set_max_history(2);
        for i in 0..5 {
            d.apply(Edit::Insert {
                at: i,
                text: i.to_string().as_str().into(),
            })
            .unwrap();
        }
        // 5 次插入但只保留最近 2 条
        d.undo().unwrap();
        d.undo().unwrap();
        assert!(!d.can_undo(), "超出上限的历史应被丢弃");
        assert_eq!(d.text(), "012");
    }

    #[test]
    fn snap_to_boundary_clamps() {
        let d = doc("你好abc");
        assert_eq!(d.snap_to_boundary(0), 0);
        assert_eq!(d.snap_to_boundary(1), 0); // 「你」中间 → 起点
        assert_eq!(d.snap_to_boundary(3), 3); // 正好是边界
        assert_eq!(d.snap_to_boundary(999), d.len());
    }

    #[test]
    fn char_movement_respects_boundaries() {
        let d = doc("你好a");
        // 字节: 你=0..3, 好=3..6, a=6..7
        assert_eq!(d.prev_char_boundary(6), 3);
        assert_eq!(d.next_char_boundary(3), 6);
        assert_eq!(d.prev_char_boundary(7), 6);
        assert_eq!(d.next_char_boundary(7), 7);
        assert_eq!(d.prev_char_boundary(0), 0);
        assert_eq!(d.prev_char_boundary(1), 0, "落在字符中间应吸附回起点");
        assert_eq!(d.next_char_boundary(1), 3, "落在字符中间应跳到字符末尾");
    }

    #[test]
    fn slice_and_try_slice() {
        let d = doc("你好世界");
        assert_eq!(d.slice(3..9), "好世");
        assert_eq!(
            d.try_slice(1..9),
            Err(EditError::InvalidBoundary { offset: 1 })
        );
        assert_eq!(
            d.try_slice(0..99),
            Err(EditError::InvalidRange {
                start: 0,
                end: 99,
                doc_len: 12
            })
        );
    }

    #[test]
    fn summary_reports_affected_range() {
        let mut d = doc("abcdef");
        let s = d
            .apply(Edit::Replace {
                range: 2..4,
                text: "XY".into(),
            })
            .unwrap();
        assert_eq!(s.affected, 2..4);
        assert_eq!(s.doc_len, 6);
        let s = d.apply(Edit::Delete { range: 0..1 }).unwrap();
        assert_eq!(s.affected, 0..0, "删除点为空区间");
    }

    #[test]
    fn clear_history() {
        let mut d = doc("a");
        d.apply(Edit::Insert {
            at: 1,
            text: "b".into(),
        })
        .unwrap();
        d.clear_history();
        assert!(!d.can_undo());
        assert!(d.undo().is_none());
    }

    #[test]
    fn apply_and_sync_integrates_crdt() {
        use crate::crdt::{crdt_to_document, document_to_crdt};
        let mut d = doc("# 标题\n正文");
        let mut crdt = document_to_crdt(&d);
        d.apply_and_sync(
            Edit::Insert {
                at: 0,
                text: "> ".into(),
            },
            &mut crdt,
        )
        .unwrap();
        assert_eq!(d.text(), crdt.text());
        let mut d2 = crdt_to_document(&crdt);
        assert_eq!(d2.text(), d.text());
        assert_eq!(d2.projection().lines.len(), d.projection().lines.len());
    }

    /// 断言块缓存与全量解析一致（校验增量重解析的正确性）。
    fn assert_blocks_match(d: &Document, step: &str) {
        let full = mazha_md::parse(&d.text());
        let shape = |blocks: &[mazha_md::Block]| -> Vec<String> {
            blocks
                .iter()
                .map(|b| format!("{:?}@{}..{}", b.kind, b.range.start, b.range.end))
                .collect()
        };
        let cached = shape(d.blocks());
        let full = shape(&full);
        assert_eq!(
            cached,
            full,
            "[{step}] 块缓存应与全量解析一致\ntext={:?}\ncached={:?}\nfull={:?}",
            d.text(),
            cached,
            full,
        );
    }

    #[test]
    fn blocks_stay_synced_after_edits() {
        let mut d = doc("# 标题\n\n第一段\n\n第二段\n\n第三段\n");
        assert_blocks_match(&d, "初始");

        // 1) 段中插入（CJK；字节 10 是“第一段”起点，字符边界安全）
        d.apply(Edit::Insert {
            at: 10,
            text: "新增".into(),
        })
        .unwrap();
        assert_blocks_match(&d, "段中插入");

        // 2) 前缀化：把段落变成标题
        d.apply(Edit::Insert {
            at: 0,
            text: "# ".into(),
        })
        .unwrap();
        assert_blocks_match(&d, "前缀化");

        // 3) 删除（含块间空行 → 可能合并块）
        d.apply(Edit::Delete { range: 0..2 }).unwrap();
        assert_blocks_match(&d, "删除空行");

        // 4) 末尾追加
        d.apply(Edit::Insert {
            at: d.len(),
            text: "\n末尾追加\n".into(),
        })
        .unwrap();
        assert_blocks_match(&d, "末尾追加");

        // 5) 撤销/重做后缓存仍应一致
        d.undo().unwrap();
        assert_blocks_match(&d, "撤销");
        d.redo().unwrap();
        assert_blocks_match(&d, "重做");
    }

    #[test]
    fn blocks_synced_for_code_block_edit() {
        let mut d = doc("```rust\nfn a() {}\nfn b() {}\n```\n\n结尾段\n");
        d.apply(Edit::Insert {
            at: 22,
            text: "fn c() {}\n".into(),
        })
        .unwrap();
        assert_blocks_match(&d, "代码块插入");
        // 投影应看到 3 个代码内容行
        let proj = d.projection();
        assert!(proj.lines.iter().any(|l| l.block.code_block));
    }

    /// 断言缓存投影与全量投影完全一致（校验增量刷新的正确性）。
    fn assert_projection_matches(d: &mut Document, step: &str) {
        let full = crate::build_projection(&d.text());
        let cached = d.projection();
        assert_eq!(cached.lines.len(), full.lines.len(), "[{step}] 行数应一致");
        assert_eq!(cached.widgets, full.widgets, "[{step}] 块级 widget 不一致");
        for (i, (a, b)) in cached.lines.iter().zip(full.lines.iter()).enumerate() {
            assert_eq!(
                a.source_range, b.source_range,
                "[{step}] 第 {i} 行 source_range 不一致"
            );
            assert_eq!(a.block, b.block, "[{step}] 第 {i} 行块样式不一致");
            assert_eq!(
                a.display_text(),
                b.display_text(),
                "[{step}] 第 {i} 行显示文本不一致"
            );
            assert_eq!(a.runs, b.runs, "[{step}] 第 {i} 行 runs 不一致");
            assert_eq!(a.widgets, b.widgets, "[{step}] 第 {i} 行 widgets 不一致");
            // 映射必须同样工作：与全量投影的双向换算结果完全一致
            let disp_len = a.display_text().chars().count();
            for disp in 0..=disp_len {
                let dp = crate::DisplayPos(disp);
                assert_eq!(
                    a.offset_map.display_to_source(dp),
                    b.offset_map.display_to_source(dp),
                    "[{step}] 第 {i} 行 display→source 不一致（disp={disp}）"
                );
            }
            for src in a.source_range.clone() {
                assert_eq!(
                    a.offset_map.source_to_display(src),
                    b.offset_map.source_to_display(src),
                    "[{step}] 第 {i} 行 source→display 不一致（src={src}）"
                );
            }
        }
    }

    #[test]
    fn new_ordered_item_before_nested_list_shows_marker() {
        let mut d = doc("1. a\n2. b\n   - n\n");
        let at = d.text().find("\n   - n").unwrap();
        d.apply(crate::intent::newline_edit(&d.text(), at)).unwrap();
        assert_eq!(d.text(), "1. a\n2. b\n3. \n   - n\n");
        let p = d.projection();
        let l = &p.lines[2];
        assert_eq!(l.display_text(), "");
        assert_eq!(
            l.block.bullet,
            Some(crate::Bullet {
                ordered: true,
                number: 3
            })
        );
    }

    #[test]
    fn projection_cache_matches_full_after_edits() {
        let mut d =
            doc("# 标题\n\n```rust\nfn a() {}\n```\n\n> 引用\n\n- 列表一\n- 列表二\n\n结尾\n");
        // 先读取一次以填充缓存，后续编辑都走增量刷新
        assert_projection_matches(&mut d, "初始");

        // 1) 段内插入（含行内标记）
        let at = d.text().find("列表一").unwrap();
        d.apply(Edit::Insert {
            at,
            text: "**粗** ".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "插入行内标记");

        // 2) 标题前缀化（样式变化）
        d.apply(Edit::Insert {
            at: 0,
            text: "## ".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "标题降级");

        // 3) 插入换行 → 行数增加
        let at = d.text().find("列表二").unwrap();
        d.apply(Edit::Insert {
            at,
            text: "\n".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "拆行");

        // 4) 删除整行（行数减少）
        let text = d.text();
        let start = text.find("\n\n结尾").unwrap();
        d.apply(Edit::Delete {
            range: start..text.len(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "删尾");

        // 5) 代码块内插入
        let at = d.text().find("fn a() {}").unwrap();
        d.apply(Edit::Insert {
            at,
            text: "fn x() {}\n".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "代码块插行");

        // 6) 撤销/重做
        d.undo().unwrap();
        assert_projection_matches(&mut d, "撤销");
        d.redo().unwrap();
        assert_projection_matches(&mut d, "重做");
    }

    #[test]
    fn projection_cache_handles_cjk_and_offsets() {
        let mut d = doc("你好**世界**\n\n第二段中文内容\n");
        assert_projection_matches(&mut d, "初始");
        let at = d.text().find("第二").unwrap();
        d.apply(Edit::Insert {
            at,
            text: "「引用」".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "中文插入");
        // 光标映射抽查：显示位置 0 → 源 0；显示末尾 → 行末源边界
        let proj = d.projection();
        let last = proj.lines.last().unwrap();
        assert_eq!(
            last.offset_map.source_to_display(last.source_range.start),
            crate::DisplayPos(0)
        );
    }

    #[test]
    fn selection_offsets_are_byte_based() {
        // 用中文验证偏移确实按字节而非字符
        let mut d = doc("你");
        let sel = Selection::new(0, 3);
        assert!(sel.contains(3));
        d.apply(Edit::Delete {
            range: sel.start()..sel.end(),
        })
        .unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn line_range_of_basic() {
        let d = doc("a\nbb\n");
        assert_eq!(d.line_range_of(0), 0..1); // 第一行
        assert_eq!(d.line_range_of(1), 0..1); // 换行符位置归前一行
        assert_eq!(d.line_range_of(2), 2..4); // 第二行
        assert_eq!(d.line_range_of(4), 2..4); // 行尾换行符位置归前一行
        assert_eq!(d.line_range_of(5), 5..5); // 文档末尾的空行（光标所在）
        assert_eq!(d.line_range_of(99), 5..5); // 越界 clamp 到末尾空行
    }

    #[test]
    fn line_range_of_no_trailing_newline() {
        let d = doc("a\nbb");
        assert_eq!(d.line_range_of(3), 2..4);
    }

    #[test]
    fn word_range_ascii() {
        let d = doc("hello world");
        assert_eq!(d.word_range_at(7), 6..11); // world 中间
        assert_eq!(d.word_range_at(6), 6..11); // 词首
        assert_eq!(d.word_range_at(11), 6..11); // 词尾（光标在右侧空白前）
        assert_eq!(d.word_range_at(2), 0..5); // hello
        assert_eq!(d.word_range_at(5), 0..5, "光标在词尾仍命中该词");
        assert_eq!(d.word_range_at(0), 0..5);
    }

    #[test]
    fn word_range_on_space_is_empty() {
        let d = doc("a   b"); // 三个空格，取正中间那个
        assert_eq!(d.word_range_at(2), 2..2, "两侧皆空白 → 无词");
        assert_eq!(d.word_range_at(1), 0..1, "紧贴词右侧 → 归该词");
        assert_eq!(d.word_range_at(4), 4..5, "紧贴词左侧 → 归该词");
    }

    #[test]
    fn word_range_cjk() {
        let d = doc("你好世界");
        assert_eq!(d.word_range_at(0), 0..12, "整个 CJK 连续段是一个词");
        assert_eq!(d.word_range_at(3), 0..12);
        assert_eq!(d.word_range_at(6), 0..12);
    }

    #[test]
    fn word_range_mixed_ascii_and_cjk_split() {
        let d = doc("abc你好def");
        assert_eq!(d.word_range_at(1), 0..3, "ASCII 段");
        assert_eq!(d.word_range_at(6), 3..9, "CJK 段");
        assert_eq!(d.word_range_at(7), 3..9, "多字节中部偏移会被吸附到词");
        assert_eq!(d.word_range_at(10), 9..12, "后段 ASCII");
    }

    #[test]
    fn word_range_punctuation_is_empty() {
        let d = doc("a * b");
        assert_eq!(d.word_range_at(2), 2..2, "标点不当词");
        assert_eq!(d.word_range_at(3), 3..3);
    }

    #[test]
    fn word_range_multiline_stays_in_line() {
        let d = doc("aa\nbbb");
        assert_eq!(d.word_range_at(1), 0..2, "第一行");
        assert_eq!(d.word_range_at(4), 3..6, "第二行不越界");
    }

    #[test]
    fn word_range_empty_text() {
        let d = doc("");
        assert_eq!(d.word_range_at(0), 0..0);
    }

    #[test]
    fn table_projects_with_visible_pipes_and_hidden_delimiter() {
        let mut d = doc("| a | b |\n| --- | --- |\n| 1 | 2 |");
        let proj = d.projection();
        // 3 行：表头 / 分隔行（隐藏）/ 数据行（无尾随换行）
        assert_eq!(proj.lines.len(), 3);
        // 显示文本 = 可见片段拼接：竖线（可见）+ 单元格内容（trim 后）；空格被裁掉
        assert_eq!(proj.lines[0].display_text(), "|a|b|");
        assert_eq!(proj.lines[1].display_text(), "", "分隔行整行隐藏");
        assert_eq!(proj.lines[2].display_text(), "|1|2|");
    }

    #[test]
    fn table_edit_keeps_projection_consistent() {
        let mut d = doc("| a | b |\n| --- | --- |\n| 1 | 2 |");
        assert_projection_matches(&mut d, "表格初始");
        // 追加一行 → 增量刷新后仍与全量一致
        let at = d.len();
        d.apply(Edit::Insert {
            at,
            text: "\n| 3 | 4 |".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "表格追加行");
        assert_eq!(d.projection().lines.len(), 4);
    }

    #[test]
    fn table_op_apply_keeps_projection_consistent() {
        let mut d = doc("| a | b |\n| --- | --- |\n| 1 | 2 |");
        // 光标落在首数据行 "1" 内
        let caret = d.text().find("| 1").unwrap() + 3;
        let edit = crate::edit_table(&d.text(), caret, crate::TableOp::InsertRowBelow)
            .expect("光标在表格内应产出编辑");
        d.apply(edit).unwrap();
        assert_eq!(d.text(), "| a | b |\n| --- | --- |\n| 1 | 2 |\n|  |  |");
        assert_projection_matches(&mut d, "表格结构编辑后");
    }

    #[test]
    fn callout_projects_as_quote_depth() {
        let mut d = doc("> [!NOTE]\n> 内容");
        assert_projection_matches(&mut d, "提示块");
        let proj = d.projection();
        // 2 行：标记行（隐藏）+ 内容行；内容行 quote_depth = 1
        assert_eq!(proj.lines.len(), 2);
        assert_eq!(proj.lines[1].display_text(), "内容");
        assert_eq!(proj.lines[1].block.quote_depth, 1, "提示块按引用深度投影");
    }

    #[test]
    fn frontmatter_projects_as_empty_lines() {
        let mut d = doc("---\ntitle: x\n---\n正文");
        assert_projection_matches(&mut d, "front-matter");
        let proj = d.projection();
        // front-matter 无片段 → 显示为空；正文正常
        let last = proj.lines.last().expect("至少一行");
        assert_eq!(last.display_text(), "正文");
    }

    /// 全语法特性集成：解析 → 投影一致 → 编辑后仍一致 → 序列化往返。
    #[test]
    fn full_pipeline_all_features() {
        let src = concat!(
            "---\ntitle: 集成\n---\n",
            "# 标题\n\n",
            "段落 **粗** *斜* ~~删~~ `码` [链接](https://x.com) [[Page|别名]] 脚注[^1]\n\n",
            "> [!NOTE]\n> 提示内容\n\n",
            "> 普通引用\n\n",
            "- 列表项\n- [x] 任务\n\n",
            "1. 有序\n\n",
            "| a | b |\n| --- | --- |\n| 1 | 2 |\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "$$E=mc^2$$\n\n",
            "[^1]: 脚注正文\n\n",
            "---\n\n",
            "![图](img.png)\n",
        );
        let mut d = doc(src);
        assert_projection_matches(&mut d, "全特性初始");

        // 验证块种类齐全
        let blocks = mazha_md::parse(src);
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|b| match b.kind {
                mazha_md::BlockKind::FrontMatter { .. } => "front-matter",
                mazha_md::BlockKind::Heading { .. } => "heading",
                mazha_md::BlockKind::Paragraph => "paragraph",
                mazha_md::BlockKind::Callout { .. } => "callout",
                mazha_md::BlockKind::BlockQuote => "quote",
                mazha_md::BlockKind::List { .. } => "list",
                mazha_md::BlockKind::Table { .. } => "table",
                mazha_md::BlockKind::CodeBlock { .. } => "code",
                mazha_md::BlockKind::MathBlock => "math",
                mazha_md::BlockKind::FootnoteDef { .. } => "footnote",
                mazha_md::BlockKind::ThematicBreak => "hr",
                mazha_md::BlockKind::ListItem { .. } => "list-item",
            })
            .collect();
        for expected in [
            "front-matter",
            "heading",
            "paragraph",
            "callout",
            "quote",
            "list",
            "table",
            "code",
            "math",
            "footnote",
            "hr",
        ] {
            assert!(
                kinds.contains(&expected),
                "应包含 {expected} 块，实际 {kinds:?}"
            );
        }

        // 序列化往返（结构保持）
        let roundtrip = mazha_md::to_md(src);
        assert_eq!(roundtrip, src, "全特性序列化应逐字节往返");

        // 在末尾追加段落 → 增量投影仍与全量一致
        let at = d.len();
        d.apply(Edit::Insert {
            at,
            text: "\n追加段落".into(),
        })
        .unwrap();
        assert_projection_matches(&mut d, "全特性追加");
    }
}
