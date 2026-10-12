//! # mazha-core
//!
//! 码札的共享核心：文档模型、编辑命令、撤销栈、光标/选区、显示↔源偏移映射、
//! 渲染投影。见 `SPEC.md` §5、§6.1、§11。
//!
//! **本 crate 不得依赖任何 GUI 框架**（SPEC §4.1），以便在无 UI 环境下单测。
//!
//! Phase 1 将在此实现契约草案中的 `Document` / `Edit` / `Projection` /
//! `OffsetMap` / `Selection`。

/// 核心 crate 版本号（编译期注入）。
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 返回核心 crate 版本号。
pub fn version() -> &'static str {
    CRATE_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_version() {
        assert!(!version().is_empty());
    }
}
