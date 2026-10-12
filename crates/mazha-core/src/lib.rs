//! # mazha-core
//!
//! 码札的共享核心：文档模型、编辑命令、撤销栈、光标/选区、显示↔源偏移映射、
//! 渲染投影。见 `SPEC.md` §5、§6.1、§11。
//!
//! **本 crate 不得依赖任何 GUI 框架**（SPEC §4.1），以便在无 UI 环境下单测。
//!
//! Phase 1 当前里程碑：`Document`（源文本真相）+ `Edit` + 撤销/重做 +
//! 光标/选区 + 边界校验（最小可用内核）。`Projection` / `OffsetMap`
//! 在本里程碑之后的迭代中实现。

mod commands;
mod crdt;
mod document;
mod edit;
mod error;
mod intent;
mod offset_map;
mod projection;
mod selection;
mod workspace;

pub use commands::{set_block, toggle_inline};
pub use document::Document;
pub use edit::{BlockTarget, ChangeSummary, Edit, InlineMark};
pub use error::{EditError, Result};
pub use intent::{DefaultIntentMapper, Intent, IntentMapper};
pub use offset_map::{DisplayPos, OffsetMap, OffsetMapError};
pub use projection::{
    BlockStyle, Bullet, ProjectedLine, Projection, RunStyle, TextRun, Widget, WidgetKind,
};
pub use selection::Selection;
pub use workspace::{Workspace, WorkspaceError, WorkspaceResult};

// CRDT 桥接（core → mazha-crdt）
pub use crdt::{apply_edit_to_crdt, crdt_to_document, document_to_crdt};

// 便捷重导出存储层类型（依赖方向 core → store）
pub use mazha_store::{DocPath, StorageProvider};

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
