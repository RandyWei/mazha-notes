# platforms/desktop — 桌面端点（macOS / Windows / Linux）

技术栈：**GPUI + gpui-kit**（见 `SPEC.md` §7.1）。macOS / Windows 为 MVP 平台。

**当前状态**：Phase 0，尚未创建。

- 独立于 workspace 构建（GPUI 为 git-only 依赖，避免拖慢核心构建）。
- GPUI 依赖锁定在 gpui-kit 对应的快照版本。
- Phase 2 在此创建编辑器渲染层（Element + `shape_line` + `EntityInputHandler`）。