# HANDOFF — 码札 / Mazha 交接与进度总览

> 每次提交代码时**同步更新本文件**（约定，见 `AGENTS.md`）。
> 单一事实来源是 `SPEC.md`；本文件只做"当前进度 / 下一步"的归总，不替代 SPEC。

- 最近更新：2026-10-16
- 当前阶段：**Phase 1（共享核心）已完成**；**Phase 2 进行中**（桌面只读投影渲染 + 光标/点击定位 + WYSIWYG 标记显隐 + 自写 Element 连续布局/跨行选区 + IME + 编辑命令 + 多光标 + PDF 导出）；**导出线**并行推进（HTML / PDF / DOCX / **EPUB** 均可用）
- 仓库：`git@github.com:RandyWei/mazha-notes.git`（私有，发版后转公开）
- 分支：`main`（当前提交见下方"提交历史"）

## 门禁（提交前必须全绿）

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# 桌面端（独立 workspace，见下文平台进度）
cd platforms/desktop && cargo fmt --check && cargo clippy -- -D warnings && cargo build
```

> rustc 需 **≥ 1.92**（workspace `rust-version`，Typst 0.15 的 MSRV），工具链见 `rust-toolchain.toml`（stable）。

## crate 进度

| crate | 状态 | 已实现 | 待办 |
|-------|------|--------|------|
| `mazha-core` | ✅ Phase 1 完成 | `Document`（rope/编辑/undo-redo/边界导航/`apply_and_sync`/块缓存+投影缓存**双增量**维护）、`Edit`/`Selection`/`ChangeSummary`、`OffsetMap`（含 `shift` 平移）、`Projection`（`BlockStyle`/`TextRun`/`Widget` + `build_projection_from` + **增量 refresh**）、`commands`、`IntentMapper`（选区语义/块级缩进/智能成对退格）、导航（`line_range_of`/CJK 感知 `word_range_at`）、`Workspace`（open/save/create/remove/rename/list/find）、`crdt` 桥接（`document_to_crdt`/`crdt_to_document`/`apply_edit_to_crdt`/`apply_remote`/`save_crdt`/`load_crdt`） | `DocId`、序列化往返 |
| `mazha-md` | ✅ Phase 1 完成（方言特性全齐） | `parse`（ATX/Setext 标题、段落、分割线、围栏代码、引用、列表/任务、**GFM 表格**、**Callout 提示块**、**Front-matter**）、`parse_inline`（强调/粗体/删除线/行内代码/链接/图片/自动链接/**wiki-link**/**tags**）、`serialize_source/to_md`（前缀穿线嵌套序列化：逐字节往返 + 幂等 + 结构保持）、`reparse`（增量：受影响块区间重解析 + range 平移 + 裁切点安全校验 + 全量兜底） | — |
| `mazha-store` | ✅ MVP | `StorageProvider`（同步，D24）、`FileSystemProvider`、`DocPath`、`Entry`、`StoreError` | 文件监听、异步/流式（Phase 5）、云端/插件 Provider |
| `mazha-crdt` | ✅ MVP | `CrdtDoc`（`Y.Text` content，字符↔字节转换）、`insert/delete/replace/push`、状态向量+增量更新（Yjs v1）、`apply_update`、`fork`（同状态多端副本）、`snapshot`（快照压缩，O(历史)→O(文本)）、并发收敛 | 多文档名册 |
| `mazha-export` | ✅ HTML + PDF + DOCX + **EPUB** 可用 | `to_html` + `to_html_document`（完整 HTML5 外壳 + 默认 CSS + title 转义）：标题/段落/引用/提示块/列表/任务/围栏代码/分割线/GFM 表格/行内全套/链接/wiki-link/标签/图片；`to_typst`（Markdown→Typst 标记：含 `escape_typst` 特殊字符转义、表格 `align:` + 逗号实参、图片 `alt:` 命名参数、动态围栏）；**`to_pdf`/`PdfExporter`**（`ExportWorld` 实现 `typst::World`：虚拟主文件 + `base_dir` 资源根 + `typst-kit` 内置/系统字体 → `typst::compile` → `typst_pdf::pdf`；`PdfDocument{bytes,warnings}`、`PdfError::{Compile,Export}`、`PdfExportOptions{base_dir,system_fonts}`）；**`to_docx`**（`docx-rs` 1.1 生成 OOXML，不经 HTML 中转：标题直接格式/段落/引用/提示块/列表/任务/GFM 表格/代码块逐行/分割线/行内 粗/斜/删/码/链接「文字 + (URL)」/wiki-link/标签/图片占位；`DocxError`；text 经 `strong-xml` 转义 + `xml:space="preserve"`）；**`to_epub`/`to_epub_with_meta`**（Zip + XHTML 打包 EPUB 3：`mimetype` 首条目 Stored / `container.xml` / `content.opf` / `nav.xhtml` / `toc.ncx` / `style.css` / 单一 `text.xhtml`；正文复用 HTML 的 **XHTML 方言** `to_xhtml_body`（空元素自闭合、布尔属性带值）；`EpubMeta`/`EpubError`；`dc:identifier` FNV-1a 内容哈希派生、`dcterms:modified` 固定时间戳） | 模板/主题、字体子集（Web）、PDF 字体覆盖/转义真实文档回归（R8） |
| `mazha-plugin` | ⬜ 占位 | — | WASM 宿主、能力授权 |
| `mazha-ffi` | ⬜ 占位 | — | UniFFI 绑定（iOS/Android/鸿蒙） |
| `mazha-wasm` | ⬜ 占位 | — | Web 绑定（Phase 7） |

## 平台进度（`platforms/*`，各自独立 workspace）

| 平台 | 状态 | 已实现 | 待办 |
|------|------|--------|------|
| `desktop`（GPUI + gpui-kit 0.7） | 🟡 Phase 2 进行中 | **只读投影渲染**（标题字号字重、列表符号、任务复选框、引用缩进、代码块等宽+弱底、行内样式 粗/斜/删除线/行内代码/链接、图片占位、分割线），主题色 + 深浅色跟随；**光标/点击定位 + WYSIWYG 标记显隐**：点击用 `TextLayout::index_for_position` 命中（显示字符索引 → `OffsetMap::display_to_source` → 源偏移），光标行渲染原始 Markdown（`#`/`**`/`-` 可见），其余行投影（隐藏），方向键/Home/End 移动，光标竖线用 `canvas` + `paint_quad`；**自写 `Element` + `shape_line`**：逐块连续布局（`DocElement` 记录 `x_for_index`/`closest_index_for_x` 几何）、整篇命中（`DocLayout` 一次换算窗口坐标→源偏移）、**跨行选区**（鼠标拖选 + Shift+方向键，选区为源字节 `Range<usize>`，块内按重叠区画高亮矩形）；**IME（`EntityInputHandler`）**：`text_for_range`/`selected_text_range`/`marked_text_range`/`unmark_text`/`replace_text_in_range`/`replace_and_mark_text_in_range`/`bounds_for_range`/`character_index_for_point`/`set_selected_text_range`/`text_length_utf16` 全实现，UTF-8↔UTF-16 双向映射，组合文本高亮、提交、取消全支持；**编辑命令（`commands`/`apply_and_sync`）**：行内格式 Cmd+B/I/X/\`（粗/斜/删除线/行内代码），块类型 Cmd+1-6/Shift+8/9/0/-（标题/无序/有序列表/引用/段落），选区/光标位置自动映射到源范围并应用 `toggle_inline`/`set_block`；**多光标**：Alt+Click 增减光标，方向键/编辑命令同步作用于所有光标，选区独立维护；**PDF 导出**：Cmd+E 调用 `mazha-export::to_pdf`，`rfd` 弹出保存对话框，写入 PDF 字节；`cargo run -- [path/to/file.md]` | — |
| `ios` / `android` / `harmony` / `web` | ⬜ 仅 README | — | Phase 6/8/7 |

## 测试现状

- `mazha-core`：119
- `mazha-md`：99
- `mazha-export`：88
- `mazha-crdt`：11
- 合计 **332** 条（其余为占位 crate 的版本号冒烟测试）

## 关键决策（详见 SPEC §14 / `docs/ADR`）

- D1–D22：重写、GPUI+gpui-kit、纯 Rust 核心、源文本真相、GFM+知识库方言、yrs CRDT、WASM 插件、AGPL-3.0 等。
- D23：Markdown **自研行扫描**（块级+行内，精确字节偏移），`pulldown-cmark` 降为 dev 依赖做对照。见 [`docs/ADR/ADR-001`](docs/ADR/ADR-001-self-authored-markdown-parser.md)。
- D24：`StorageProvider` 采用**同步**接口（本地文件系统为唯一 MVP Provider），异步/流式/文件监听推迟到 Phase 5。
- D25：PDF 编译走 `to_typst` + 自实现 `ExportWorld`（`typst::World`：虚拟主文件 / `base_dir` 资源根 / `typst-kit` 字体），`typst::compile` + `typst_pdf::pdf`；workspace `rust-version` 提到 1.92（typst 0.15 MSRV）。
- D26：DOCX 导出走 `docx-rs` 1.1，直接基于 `mazha-md` 块/片段模型生成 OOXML（不经 HTML 中转）；标题用直接格式、链接为「文字 + (URL)」、图片占位。
- D27：EPUB 导出走 Zip + XHTML（`zip` 0.5，与 docx-rs 复用同版本），正文复用 HTML 的 **XHTML 方言**（`to_xhtml_body`：空元素自闭合、布尔属性带值），打包为 EPUB 3；`dc:identifier` 用 FNV-1a 内容哈希派生、`dcterms:modified` 固定时间戳（输出确定）。

## 依赖方向（不可违反）

- `mazha-core → mazha-md`、`mazha-core → mazha-store`、`mazha-core → mazha-crdt`（core 是共享核心顶层）
- `mazha-md` **不得**反向依赖 `mazha-core`
- 所有核心 crate **不得**依赖任何 GUI（CI 校验）

## 测试策略（已建立）

- **增量 == 全量**逐字段比对：块缓存（`reparse`）、投影+OffsetMap（`Projection::refresh`）、编辑序列
- **CRDT 双端收敛**：fork → 双端编辑 → 合并 → 文本一致
- **序列化**：结构保持 + 逐字节往返 + 幂等（`to_md(to_md(x)) == to_md(x)`）
- **全管线集成**：`full_pipeline_all_features` 单文档覆盖全部语法 + 编辑后投影一致

## 下一步（建议顺序）

1. `mazha-export`：模板/主题、字体子集（Web）；PDF 字体覆盖/转义的**真实文档回归**（R8）
2. `mazha-crdt`：多文档名册；`mazha-store` 文件监听（`watch`，Phase 5）
3. Phase 3：`OffsetMap` 完善（跨行选区/命中/IME）+ 块级 widget（表格/图片/代码/公式/图表）

## 提交历史（近期，共 54 条；本次提交为最新一条，下次同步时补入哈希）

```
<本次提交：feat(export): EPUB 导出（Zip + XHTML）>
6922f6a feat(export): DOCX 导出（docx-rs，D26）
917d9ce chore: update Cargo.lock for mazha-export + rfd deps
531bdc7 feat(desktop): PDF 导出（Cmd+E）
72ee12a feat(desktop): 多光标支持
2a9c1c8 feat(desktop): 编辑命令接入（commands/apply_and_sync）
9e188db feat(desktop): IME 支持 + EntityInputHandler 完整实现
2e9a5af feat(desktop): 自写 Element + shape_line 连续布局 + 跨行选区
9e24880 feat(desktop): 光标/点击定位 + WYSIWYG 标记显隐（光标行原文，其余投影隐藏）
13e0295 feat(desktop): Phase 2 起步——GPUI 只读投影渲染（可运行）
e14e14f feat(export): PDF 编译——Typst World 实现 + to_pdf（PDF 管线第二步）
d60189c docs: HANDOFF 全面刷新（Phase 1 完成状态）
b028dad feat(export): Typst 标记导出（PDF 管线第一步）
90a4c25 feat(md): Tags 知识库扩展（解析 + 导出；方言特性补齐）
e956e3a test(core): 全管线集成测试（全语法特性）
495c551 feat(md): Front-matter 知识库扩展（解析 + 序列化 + 导出隐藏）
e000128 feat(md): Wiki-link 知识库扩展（解析 + 序列化 + HTML 导出）
862ab9a feat(md): Callout 提示块支持（解析 + 序列化 + HTML 导出 + 投影）
8ebc866 test(core): 表格投影显示文本 + 增量刷新一致性
a225bbc feat(md): GFM 表格支持（解析 + 序列化 + HTML 导出 + 投影）
a57df1d feat(core): 选区输入语义 + Workspace 文件管理 API
0ff8554 feat(core): Document 导航助手（行范围 + CJK 感知词范围）
cb5af25 feat(crdt): push() 末尾追加 + snapshot() 快照压缩
99d4545 fix(md): 序列化嵌套结构重写（前缀穿线）+ 往返一致性测试
441e959 feat(core): IntentMapper 增强——块级缩进 + 智能成对退格
064cac0 feat(core): Projection::refresh 增量刷新投影（只重建脏行）
64481dc feat(core): Document 增量维护块缓存 + reparse 裁切点安全性加固
d1f430a feat(md): 增量重解析 reparse（受影响块局部重解析 + range 平移）
5802297 feat(crdt): 更新帧持久化落盘（save_crdt / load_crdt）
9d745a0 feat(crdt): 协同闭环——fork 多端副本 + apply_remote 远端重建
39f3dec feat(core): Document::apply_and_sync 编辑+CRDT 同步原子操作
4c2df22 feat(core): CRDT 桥接（Document ↔ CrdtDoc 双向同步）
c690b8c feat(export): to_html_document 完整 HTML5 外壳 + 默认 CSS
d917644 fix(md): 序列化修复链接 URL 丢失 + 补充行内/链接回归测试
746202b docs: 同步 HANDOFF 测试计数（md +3 → 139）
c7c04bf test(md): serialize 更多回归（代码块/引用/任务）
4d59b23 docs: 同步 HANDOFF/SPEC 测试计数（md +3 → 136）
```
