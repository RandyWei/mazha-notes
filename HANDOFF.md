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
| `mazha-core` | ✅ Phase 1 主体 | `Document`（rope/编辑/undo-redo/边界导航）、`Edit`（Insert/Delete/Replace/ToggleInline/ToggleBlock）、`Selection`、`ChangeSummary`、`OffsetMap`、`Projection`（`BlockStyle`/`TextRun`/`Widget` + 每行映射）、`commands`（toggle_inline/set_block）、`IntentMapper`（回车续列表等）、`Workspace`（open/save/list） | 增量投影/增量解析、更丰富意图（缩进/自动配对/删除键合并）、`DocId`、序列化往返 |
| `mazha-md` | ✅ 块级 + 行内 | `parse`（ATX/Setext 标题、段落、分割线、围栏代码、引用、列表/任务）、`parse_inline`（强调/粗体/删除线/代码/链接/图片/转义/自动链接）、`Block`/`SyntaxSpan`/`visible_runs` | 增量解析（脏块重解析）、`serialize`、表格/callout/wiki-link 等知识库扩展 |
| `mazha-store` | ✅ MVP | `StorageProvider`（同步，D24）、`FileSystemProvider`、`DocPath`、`Entry`、`StoreError` | 文件监听、异步/流式（Phase 5）、云端/插件 Provider |
| `mazha-crdt` | 🟡 MVP | `CrdtDoc`（`Y.Text` content，字符↔字节转换）、`insert/delete/replace`、状态向量+增量更新（Yjs v1）、`apply_update`、并发收敛 | 多文档/快照/持久化、字符索引统一封装 |
| `mazha-export` | 🟡 HTML 完成 | `to_html`：基于 md 块/片段模型渲染（标题/段落/引用/列表/任务/围栏代码/分割线 + 行内强调/粗体/删除线/代码/链接/自动链接/图片/换行，HTML 转义） | PDF（Typst）、DOCX/EPUB、模板/主题 |
| `mazha-plugin` | ⬜ 占位 | — | WASM 宿主、能力授权 |
| `mazha-ffi` | ⬜ 占位 | — | UniFFI 绑定（iOS/Android/鸿蒙） |
| `mazha-wasm` | ⬜ 占位 | — | Web 绑定（Phase 7） |

## 测试现状

- `mazha-core`：72
- `mazha-md`：29
- `mazha-store`：12
- `mazha-export`：11
- `mazha-crdt`：6
- 合计 **130** 条（其余为占位 crate 的版本号冒烟测试）

## 关键决策（详见 SPEC §14 / `docs/ADR`）

- D1–D22：重写、GPUI+gpui-kit、纯 Rust 核心、源文本真相、GFM+知识库方言、yrs CRDT、WASM 插件、AGPL-3.0 等。
- D23：Markdown **自研行扫描**（块级+行内，精确字节偏移），`pulldown-cmark` 降为 dev 依赖做对照。见 [`docs/ADR/ADR-001`](docs/ADR/ADR-001-self-authored-markdown-parser.md)。
- D24：`StorageProvider` 采用**同步**接口（本地文件系统为唯一 MVP Provider），异步/流式/文件监听推迟到 Phase 5。

## 依赖方向（不可违反）

- `mazha-core → mazha-md`、`mazha-core → mazha-store`（core 是共享核心顶层）
- `mazha-md` **不得**反向依赖 `mazha-core`
- 所有核心 crate **不得**依赖任何 GUI（CI 校验）

## 下一步（建议顺序）

1. `mazha-crdt`：多文档名册/持久化原子写入、`apply_update` 校验与合并策略、与 `Workspace`/`core::Document` 对接
2. `mazha-store` 文件监听（`watch`）或 `mazha-core` 增量投影 / `mazha-md` 增量解析
3. `mazha-md::serialize`（AST→Markdown）与往返一致性测试
4. `mazha-export`：Markdown → HTML 完整文档外壳 / CSS 主题；PDF（Typst）
5. Phase 2：桌面 GPUI 只读投影渲染 → WYSIWYG 显隐标记 → IME

## 提交历史（近期）

```
322bd33 docs: 新增 HANDOFF.md 进度总览，并约定每次提交同步更新
145a781 feat(core): Workspace 串联存储与文档（open/save/list）
f68f2b3 feat(store): 本地文件系统 StorageProvider（同步接口，D24）
334351e feat(core): 编辑命令 ToggleInline/ToggleBlock + IntentMapper
90e5977 feat(core): 落地 Projection 投影层；确认 D23 并新增 ADR-001
d24ce45 feat(md): 行内解析（强调/粗体/删除线/代码/链接/图片/转义）
5f07fe3 feat(md): 起步 mazha-md 行扫描块级解析（16 单测）
6438e2a feat(core): OffsetMap 显示↔源偏移双向映射
0b5b997 feat(core): Phase 1 最小可用内核
```
