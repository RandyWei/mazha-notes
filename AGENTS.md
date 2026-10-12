# 码札 (Mazha) 开发约定

## 开发流程

- **规范驱动开发**：`SPEC.md` 是单一事实来源。任何接口/架构变更，先改 SPEC 再实现（SPEC §0、§15）。
- 新决定与取舍记入 SPEC §14 决策记录，并在变更后更新 §15 变更记录。
- 远程仓库（私有，发版后转公开）：`git@github.com:RandyWei/mazha-notes.git`，主分支 `main`。

## 代码规范

- Rust **edition 2024**，最新 stable（见 `rust-toolchain.toml`）。
- **代码必须有注释**（方便人工阅读，AI 生成也要保留）：
  - 每个 crate / 模块 / 公开类型 / 公开函数：写 `///` doc 注释，说明职责、关键参数/返回值与行为约束。
  - 关键实现逻辑（如偏移映射、增量解析、CRDT 合并、隐藏语法标记的显隐切换）：写 `//` 行内注释说明**为什么这么写、边界在哪**。
  - 注释用**中文**编写。
- 核心 crate（`mazha-core` 及其下层）**不得依赖任何 GUI 框架**（SPEC §4.1），保证可无 UI 单测。
- 每个核心逻辑（尤其光标/偏移映射）配纯 Rust 单测，测试用例要能直接说明契约（SPEC §12）。

## 提交前必须通过

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 提交约定

- **每次提交代码时，同步更新 `HANDOFF.md`**（进度总览 / crate 状态 / 测试计数 / 近期提交 / 下一步），与该次提交一并纳入同一个 commit。
- 提交信息用中文，概括本次变更要点。

## 目录速查（SPEC §4.4）

- `crates/`：共享 Rust 核心（core / md / crdt / store / export / plugin / ffi / wasm）
- `platforms/`：各平台 UI（desktop / ios / android / harmony / web）
- `plugins/`：WASM 插件（Phase 5）
- `docs/`：设计文档 / ADR