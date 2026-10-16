//! 表格结构编辑（SPEC §11.5、§12.2）。
//!
//! 把"在光标处增删表格行列"翻译成对**源文本**的一次 [`Edit`]。与 [`crate::commands`]
//! 一样是**纯函数**：只读源文本、返回要应用的编辑，可脱离 rope/UI 单测。
//!
//! 实现分两层：
//! 1. [`TableModel`]：把一段 GFM 表格文本解析成 `表头 + 对齐 + 数据行`，并提供结构变更
//!    与重新序列化（总是输出带首尾竖线的规范形）。
//! 2. [`edit_table`]：用解析器定位光标所在的表格块，据光标行列选择变更，产出 [`Edit`]。
//!
//! 边界约定：光标不在表格内、或变更不产生实际变化（如删除唯一一列、在表头删除行）时
//! 返回 `None`，调用方据此忽略该命令。

use crate::edit::Edit;
use mazha_md::{Align, Block, BlockKind};
use std::ops::Range;

/// 表格结构编辑操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableOp {
    /// 在光标所在行上方插入一行（表头/分隔行视作首数据行）。
    InsertRowAbove,
    /// 在光标所在行下方插入一行（表头/分隔行视作首数据行）。
    InsertRowBelow,
    /// 在光标所在列左侧插入一列。
    InsertColumnLeft,
    /// 在光标所在列右侧插入一列。
    InsertColumnRight,
    /// 删除光标所在的数据行（表头/分隔行不可删）。
    DeleteRow,
    /// 删除光标所在列（仅剩一列时不可删）。
    DeleteColumn,
}

/// 解析一段 GFM 表格区域，按光标位置套用结构变更，返回一次 [`Edit`]。
///
/// `caret` 是**源文本字节偏移**。返回 `None` 表示光标不在任何表格块内，或该操作在此
/// 表格上不产生变化。
pub fn edit_table(source: &str, caret: usize, op: TableOp) -> Option<Edit> {
    let blocks = mazha_md::parse(source);
    let range = find_table(&blocks, caret)?;
    let region = source.get(range.clone())?;
    let mut model = TableModel::parse(region)?;

    // 光标在表格区域内的行索引（0=表头，1=分隔行，>=2 为数据行）与行内偏移。
    let (line_idx, line_offset) = locate_line(region, caret - range.start)?;
    let line = region.split('\n').nth(line_idx)?;
    let col = column_at(line, line_offset, model.width());

    let changed = match op {
        TableOp::InsertRowAbove => {
            let at = data_row_index(line_idx);
            model.insert_row(at);
            true
        }
        TableOp::InsertRowBelow => {
            let at = match line_idx {
                0 | 1 => 0,
                _ => data_row_index(line_idx) + 1,
            };
            model.insert_row(at);
            true
        }
        TableOp::InsertColumnLeft => {
            model.insert_col(col);
            true
        }
        TableOp::InsertColumnRight => {
            model.insert_col(col + 1);
            true
        }
        TableOp::DeleteRow => line_idx >= 2 && model.remove_row(data_row_index(line_idx)),
        TableOp::DeleteColumn => model.remove_col(col),
    };
    if !changed {
        return None;
    }

    Some(Edit::Replace {
        range,
        text: model.to_markdown(),
    })
}

/// 数据行索引：行索引 `line_idx` 对应的数据行下标（表头/分隔行归为 0）。
fn data_row_index(line_idx: usize) -> usize {
    line_idx.saturating_sub(2)
}

/// 递归查找包含 `caret` 的表格块，返回其源字节范围。
fn find_table(blocks: &[Block], caret: usize) -> Option<Range<usize>> {
    for b in blocks {
        if caret < b.range.start || caret > b.range.end {
            continue;
        }
        if matches!(b.kind, BlockKind::Table { .. }) {
            return Some(b.range.clone());
        }
        if let Some(r) = find_table(&b.children, caret) {
            return Some(r);
        }
    }
    None
}

/// 定位行内偏移在区域中的行索引与该行内的字节偏移。
fn locate_line(region: &str, rel: usize) -> Option<(usize, usize)> {
    let mut start = 0usize;
    for (i, line) in region.split('\n').enumerate() {
        let end = start + line.len();
        if rel <= end {
            return Some((i, rel - start));
        }
        start = end + 1; // 跳过 '\n'
    }
    None
}

/// 光标所在列的索引（按该行的单元格跨度判定；落在竖线上归入右列）。
fn column_at(line: &str, offset: usize, width: usize) -> usize {
    let last = width.saturating_sub(1);
    let cells = split_cells(line);
    for (i, (r, _)) in cells.iter().enumerate() {
        if offset <= r.end {
            return i.min(last);
        }
    }
    cells.len().saturating_sub(1).min(last)
}

/// 一段 GFM 表格的可变模型。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableModel {
    /// 表头单元格（已 trim，行首/行尾竖线不计）。
    header: Vec<String>,
    /// 每列对齐（长度应与 `header` 一致）。
    align: Vec<Align>,
    /// 数据行（每行单元格数应与 `header` 一致）。
    rows: Vec<Vec<String>>,
}

impl TableModel {
    /// 解析一段表格区域（表头行 + 分隔行 + 若干数据行）。
    fn parse(region: &str) -> Option<TableModel> {
        let lines: Vec<&str> = region.split('\n').collect();
        if lines.len() < 2 {
            return None;
        }
        let header: Vec<String> = split_cells(lines[0])
            .into_iter()
            .map(|(_, s)| s.to_string())
            .collect();
        let align: Vec<Align> = split_cells(lines[1])
            .into_iter()
            .map(|(_, s)| align_of(s))
            .collect();
        let width = header.len().max(align.len());
        if width == 0 {
            return None;
        }

        // 补齐：表头/对齐按 width 补空。
        let mut header = header;
        let mut align = align;
        header.resize(width, String::new());
        align.resize(width, Align::Left);

        let rows = lines[2..]
            .iter()
            .map(|l| {
                let mut cells: Vec<String> = split_cells(l)
                    .into_iter()
                    .map(|(_, s)| s.to_string())
                    .collect();
                cells.resize(width, String::new());
                cells
            })
            .collect();

        Some(TableModel {
            header,
            align,
            rows,
        })
    }

    /// 列数。
    fn width(&self) -> usize {
        self.header.len()
    }

    /// 在第 `at` 个数据行前插入一个空行（`at` 允许等于 `rows.len()`）。
    fn insert_row(&mut self, at: usize) {
        let at = at.min(self.rows.len());
        self.rows.insert(at, vec![String::new(); self.width()]);
    }

    /// 在第 `at` 列前插入一个空列（`at` 允许等于列数）。
    fn insert_col(&mut self, at: usize) {
        let at = at.min(self.width());
        self.header.insert(at, String::new());
        self.align.insert(at, Align::Left);
        for row in &mut self.rows {
            row.insert(at, String::new());
        }
    }

    /// 删除第 `at` 个数据行；越界返回 `false`。
    fn remove_row(&mut self, at: usize) -> bool {
        if at < self.rows.len() {
            self.rows.remove(at);
            true
        } else {
            false
        }
    }

    /// 删除第 `at` 列；仅剩一列或越界返回 `false`。
    fn remove_col(&mut self, at: usize) -> bool {
        if self.width() <= 1 || at >= self.width() {
            return false;
        }
        self.header.remove(at);
        self.align.remove(at);
        for row in &mut self.rows {
            if at < row.len() {
                row.remove(at);
            }
        }
        true
    }

    /// 序列化回 GFM 表格（规范形：首尾竖线、单元格两侧留一空格）。
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        push_row(&mut out, &self.header);
        out.push('\n');
        let delim: Vec<String> = self
            .align
            .iter()
            .map(|a| align_marker(*a).to_string())
            .collect();
        push_row(&mut out, &delim);
        for row in &self.rows {
            out.push('\n');
            push_row(&mut out, row);
        }
        out
    }
}

/// 追加一行：`| a | b |`。
fn push_row(out: &mut String, cells: &[String]) {
    out.push('|');
    for c in cells {
        out.push(' ');
        out.push_str(c);
        out.push(' ');
        out.push('|');
    }
}

/// 对齐 → 分隔行片段。
fn align_marker(a: Align) -> &'static str {
    match a {
        Align::Left => "---",
        Align::Center => ":--:",
        Align::Right => "---:",
    }
}

/// 分隔行片段 → 对齐。
fn align_of(cell: &str) -> Align {
    let left = cell.starts_with(':');
    let right = cell.ends_with(':');
    match (left, right) {
        (true, true) => Align::Center,
        (false, true) => Align::Right,
        _ => Align::Left,
    }
}

/// 把一行按**未转义** `|` 拆成单元格，返回每个单元格 trim 后的字节范围与切片。
///
/// 行首/行尾竖线不产生单元格；中间的空单元格（如 `| a |  | b |`）会保留为空串。
fn split_cells(line: &str) -> Vec<(Range<usize>, &str)> {
    let bytes = line.as_bytes();
    let mut segs: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'|' && (i == 0 || bytes[i - 1] != b'\\') {
            segs.push((seg_start, i));
            seg_start = i + 1;
        }
        i += 1;
    }
    segs.push((seg_start, bytes.len()));

    // 丢弃由首/尾竖线带来的空段。
    let has_lead = line.trim_start().starts_with('|');
    let has_tail = line.trim_end().ends_with('|');
    if has_lead && !segs.is_empty() {
        segs.remove(0);
    }
    if has_tail && !segs.is_empty() {
        segs.pop();
    }

    segs.into_iter()
        .map(|(s, e)| {
            let (s, e) = trim_ws(bytes, s, e);
            (s..e, &line[s..e])
        })
        .collect()
}

/// 去掉段两端的 ASCII 空白（空格 / 制表符），返回 trim 后的范围。
fn trim_ws(bytes: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && (bytes[start] == b' ' || bytes[start] == b'\t') {
        start += 1;
    }
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
        end -= 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 应用一次编辑，返回新源文本。
    fn apply(source: &str, edit: Option<Edit>) -> String {
        let edit = edit.expect("应有编辑");
        let range = edit.source_range();
        match edit {
            Edit::Replace { text, .. } => {
                format!("{}{}{}", &source[..range.start], text, &source[range.end..])
            }
            other => panic!("表格编辑只产出 Replace，得到 {other:?}"),
        }
    }

    const DOC: &str = "para\n\n| a | b |\n| --- | :-: |\n| 1 | 2 |\n\ntail";

    /// "a" 的偏移（表格内）。
    fn caret_of(hay: &str, needle: &str) -> usize {
        hay.find(needle).unwrap()
    }

    #[test]
    fn insert_row_below_data() {
        let c = caret_of(DOC, "| 1 | 2 |") + 3; // 数据行内
        let out = apply(DOC, edit_table(DOC, c, TableOp::InsertRowBelow));
        assert_eq!(
            out,
            "para\n\n| a | b |\n| --- | :--: |\n| 1 | 2 |\n|  |  |\n\ntail"
        );
    }

    #[test]
    fn insert_row_above_data() {
        let c = caret_of(DOC, "| 1 | 2 |") + 3;
        let out = apply(DOC, edit_table(DOC, c, TableOp::InsertRowAbove));
        assert_eq!(
            out,
            "para\n\n| a | b |\n| --- | :--: |\n|  |  |\n| 1 | 2 |\n\ntail"
        );
    }

    #[test]
    fn insert_row_below_header_goes_before_first_data_row() {
        let c = caret_of(DOC, "| a | b |") + 3; // 表头行
        let out = apply(DOC, edit_table(DOC, c, TableOp::InsertRowBelow));
        assert_eq!(
            out,
            "para\n\n| a | b |\n| --- | :--: |\n|  |  |\n| 1 | 2 |\n\ntail"
        );
    }

    #[test]
    fn insert_column_left_of_second() {
        // 光标在 "2" 内（第二列）
        let c = caret_of(DOC, "| 1 | 2 |") + 7;
        let out = apply(DOC, edit_table(DOC, c, TableOp::InsertColumnLeft));
        assert_eq!(
            out,
            "para\n\n| a |  | b |\n| --- | --- | :--: |\n| 1 |  | 2 |\n\ntail"
        );
    }

    #[test]
    fn insert_column_right_of_first() {
        let c = caret_of(DOC, "| 1 | 2 |") + 3; // 第一列
        let out = apply(DOC, edit_table(DOC, c, TableOp::InsertColumnRight));
        assert_eq!(
            out,
            "para\n\n| a |  | b |\n| --- | --- | :--: |\n| 1 |  | 2 |\n\ntail"
        );
    }

    #[test]
    fn delete_data_row() {
        let c = caret_of(DOC, "| 1 | 2 |") + 3;
        let out = apply(DOC, edit_table(DOC, c, TableOp::DeleteRow));
        assert_eq!(out, "para\n\n| a | b |\n| --- | :--: |\n\ntail");
    }

    #[test]
    fn delete_header_row_is_noop() {
        let c = caret_of(DOC, "| a | b |") + 3;
        assert!(edit_table(DOC, c, TableOp::DeleteRow).is_none());
    }

    #[test]
    fn delete_column() {
        let c = caret_of(DOC, "| 1 | 2 |") + 7; // 第二列
        let out = apply(DOC, edit_table(DOC, c, TableOp::DeleteColumn));
        assert_eq!(out, "para\n\n| a |\n| --- |\n| 1 |\n\ntail");
    }

    #[test]
    fn delete_only_column_is_noop() {
        let src = "| a |\n| --- |\n| 1 |";
        let c = caret_of(src, "1");
        assert!(edit_table(src, c, TableOp::DeleteColumn).is_none());
    }

    #[test]
    fn caret_outside_table_returns_none() {
        let c = caret_of(DOC, "para");
        assert!(edit_table(DOC, c, TableOp::InsertRowBelow).is_none());
        let c = caret_of(DOC, "tail");
        assert!(edit_table(DOC, c, TableOp::DeleteColumn).is_none());
    }

    #[test]
    fn escaped_pipe_is_not_a_separator() {
        let src = "| a | b |\n| --- | --- |\n| x \\| y | z |";
        let c = caret_of(src, "z");
        let out = apply(src, edit_table(src, c, TableOp::InsertColumnRight));
        // "x \| y" 仍是一个单元格（转义竖线不拆列）
        assert_eq!(out, "| a | b |  |\n| --- | --- | --- |\n| x \\| y | z |  |");
    }

    #[test]
    fn realigns_delimiter_preserving_align() {
        let src = "| a | b | c |\n| :- | :-: | -: |\n| 1 | 2 | 3 |";
        let c = caret_of(src, "2");
        let out = apply(src, edit_table(src, c, TableOp::InsertColumnLeft));
        assert_eq!(
            out,
            "| a |  | b | c |\n| --- | --- | :--: | ---: |\n| 1 |  | 2 | 3 |"
        );
    }
}
