# HANDOFF — 码札 / Mazha 交接与进度总览

> 每次提交代码时**同步更新本文件**（约定，见 `AGENTS.md`）。
> 单一事实来源是 `SPEC.md`；本文件只做"当前进度 / 下一步"的归总，不替代 SPEC。

- 最近更新：2026-10-12
- 当前阶段：**Phase 1（共享核心）进行中**，桌面 MVP 尚未开始（Phase 2）
- 仓库：`git@github.com:RandyWei/mazha-notes.git`（私有，发版后转公开）
- 分支：`main`

## 门禁（提交前必须全绿）

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## crate 进度

| crate | 状态 | 已实现 | 待办 |
|-------|------|--------|------|
| `mazha-core` | ✅ Phase 1 主体 + CRDT 桥接 + 增量缓存 | `Document`（rope/编辑/undo-redo/边界导航/`apply_and_sync`/块缓存+投影缓存增量维护）、`Edit`（Insert/Delete/Replace/ToggleInline/ToggleBlock）、`Selection`、`ChangeSummary`、`OffsetMap`（含平移）、`Projection`（`BlockStyle`/`TextRun`/`Widget` + 每行映射 + `build_projection_from` + **增量 refresh**）、`commands`、`IntentMapper`、`Workspace`、`crdt` 桥接（`document_to_crdt`/`crdt_to_document`/`apply_edit_to_crdt`/`apply_remote`/`save_crdt`/`load_crdt`） | 更丰富意图（缩进/自动配对/删除键合并）、`DocId`、序列化往返 |
| `mazha-md` | ✅ 块级 + 行内 + 序列化 + 增量重解析 + GFM 表格 | `parse`（ATX/Setext 标题、段落、分割线、围栏代码、引用、列表/任务、**表格**）、`parse_inline`、`serialize_source/to_md`（前缀穿线嵌套序列化：逐字节往返 + 幂等 + 结构保持）、`reparse`（增量：受影响块区间重解析 + range 平移 + 裁切点安全校验 + 全量兜底） | callout/wiki-link/front-matter 等知识库扩展 |
| `mazha-store` | ✅ MVP | `StorageProvider`（同步，D24）、`FileSystemProvider`、`DocPath`、`Entry`、`StoreError` | 文件监听、异步/流式（Phase 5）、云端/插件 Provider |
| `mazha-crdt` | ✅ MVP | `CrdtDoc`（`Y.Text` content，字符↔字节转换）、`insert/delete/replace`、状态向量+增量更新（Yjs v1）、`apply_update`、`fork`（同状态多端副本）、并发收敛 | 多文档名册、持久化落盘 |
| `mazha-export` | 🟡 HTML 完成 | `to_html` + `to_html_document`（完整 HTML5 外壳 + 默认 CSS + title 转义）：标题/段落/引用/列表/任务/围栏代码/分割线/**GFM 表格（table/thead/tbody + 列对齐 + 单元格行内 + 转义）**/行内强调/粗体/删除线/代码/链接/自动链接/图片/换行 | PDF（Typst）、DOCX/EPUB、模板/主题 |
| `mazha-plugin` | ⬜ 占位 | — | WASM 宿主、能力授权 |
| `mazha-ffi` | ⬜ 占位 | — | UniFFI 绑定（iOS/Android/鸿蒙） |
| `mazha-wasm` | ⬜ 占位 | — | Web 绑定（Phase 7） |

## 测试现状

- `mazha-core`：116
- `mazha-md`：65
- `mazha-export`：19
- `mazha-crdt`：11
- 合计 **226** 条（其余为占位 crate 的版本号冒烟测试）

## 关键决策（详见 SPEC §14 / `docs/ADR`）

- D1–D22：重写、GPUI+gpui-kit、纯 Rust 核心、源文本真相、GFM+知识库方言、yrs CRDT、WASM 插件、AGPL-3.0 等。
- D23：Markdown **自研行扫描**（块级+行内，精确字节偏移），`pulldown-cmark` 降为 dev 依赖做对照。见 [`docs/ADR/ADR-001`](docs/ADR/ADR-001-self-authored-markdown-parser.md)。
- D24：`StorageProvider` 采用**同步**接口（本地文件系统为唯一 MVP Provider），异步/流式/文件监听推迟到 Phase 5。

## 依赖方向（不可违反）

- `mazha-core → mazha-md`、`mazha-core → mazha-store`、`mazha-core → mazha-crdt`（core 是共享核心顶层）
- `mazha-md` **不得**反向依赖 `mazha-core`
- 所有核心 crate **不得**依赖任何 GUI（CI 校验）

## 下一步（建议顺序）

1. `mazha-md::serialize` 完善覆盖（嵌套列表/转义/图片）与往返一致性测试
2. `mazha-crdt`：多文档名册、更新帧快照压缩；`mazha-store` 文件监听（`watch`）
3. `mazha-export`：PDF（Typst）、DOCX/EPUB、模板/主题
4. IntentMapper 增强：缩进、自动配对、删除键合并
5. Phase 2：桌面 GPUI 只读投影渲染 → WYSIWYG 显隐标记 → IME

## 提交历史（近期）

```
39f3dec feat(core): Document::apply_and_sync 编辑+CRDT 同步原子操作
4c2df22 feat(core): CRDT 桥接（Document ↔ CrdtDoc 双向同步）
c690b8c feat(export): to_html_document 完整 HTML5 外壳 + 默认 CSS
d917644 fix(md): 序列化修复链接 URL 丢失 + 补充行内/链接回归测试
746202b test(md): serialize 更多回归（代码块/引用/任务）
c7c04bf test(md): serialize 回归（段落/标题/列表）
5e927c9 feat(crdt): yrs Y.Text 封装（字符索引 ↔ UTF-8 字节）
```
