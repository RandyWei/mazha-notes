//! 光标与选区。
//!
//! 选区端点（`anchor` / `head`）都以**源文本的 UTF-8 字节偏移**表示（SPEC §6.1）。
//! 本类型不做边界校验——它只是纯数据结构，合法性由持有者的 `Document::apply`
//! 与 `snap_to_boundary` 保证。

/// 一个选区：从 `anchor` 到 `head`（含端点，按字节偏移）。
///
/// `anchor` 是选中前的固定端，`head` 是随光标移动的一端；
/// 二者顺序无关，用 [`Selection::start`] / [`Selection::end`] 获得有序区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// 固定端（字节偏移）。
    pub anchor: usize,
    /// 移动端（字节偏移）。
    pub head: usize,
}

impl Selection {
    /// 新建选区（不作边界校验）。
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// 折叠为单个光标位置。
    pub fn caret(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
        }
    }

    /// 有序区间的起点（字节偏移）。
    pub fn start(self) -> usize {
        self.anchor.min(self.head)
    }

    /// 有序区间的终点（字节偏移）。
    pub fn end(self) -> usize {
        self.anchor.max(self.head)
    }

    /// 是否为空选区（即光标而非区域）。
    pub fn is_collapsed(self) -> bool {
        self.anchor == self.head
    }

    /// 该选区是否包含字节偏移 `offset`（含边界）。
    pub fn contains(self, offset: usize) -> bool {
        self.start() <= offset && offset <= self.end()
    }

    /// 移动移动端（head）到新位置。
    pub fn set_head(&mut self, pos: usize) {
        self.head = pos;
    }

    /// 折叠到指定位置（光标移动后的落点）。
    pub fn collapse(&mut self, pos: usize) {
        self.anchor = pos;
        self.head = pos;
    }

    /// 折叠到选区起点。
    pub fn collapse_to_start(&mut self) {
        self.anchor = self.start();
        self.head = self.start();
    }

    /// 折叠到选区终点。
    pub fn collapse_to_end(&mut self) {
        self.anchor = self.end();
        self.head = self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_without_regard_to_direction() {
        let fwd = Selection::new(5, 10);
        let back = Selection::new(10, 5);
        assert_eq!(fwd.start(), 5);
        assert_eq!(fwd.end(), 10);
        assert_eq!(back.start(), 5);
        assert_eq!(back.end(), 10);
    }

    #[test]
    fn collapsed_is_caret() {
        assert!(Selection::caret(7).is_collapsed());
        assert!(!Selection::new(7, 9).is_collapsed());
    }

    #[test]
    fn contains_includes_endpoints() {
        let sel = Selection::new(3, 9);
        assert!(sel.contains(3));
        assert!(sel.contains(9));
        assert!(sel.contains(6));
        assert!(!sel.contains(2));
    }

    #[test]
    fn set_head_and_collapse() {
        let mut sel = Selection::new(3, 3);
        sel.set_head(8);
        assert_eq!(sel, Selection::new(3, 8));
        sel.collapse(8);
        assert!(sel.is_collapsed());
    }
}
